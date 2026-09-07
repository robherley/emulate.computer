//! Minimal VirtIO 1.x network device over the MMIO transport.
//!
//! Two split queues (RX = 0, TX = 1). The device holds no backend: frames sit
//! in a bounded inbox and outbox, and [`crate::machine::Machine`] moves them
//! across the [`crate::devices::net::NetBackend`] boundary on its network
//! tick, keeping host I/O out of every MMIO store.
//!
//! No offloads are negotiated (no checksum/TSO/mergeable buffers) and the MTU
//! is 1500, so every frame fits one descriptor chain.

use crate::snapshot::{Reader, Writer};

use std::collections::VecDeque;

use crate::devices::virtio_mmio::{Transport, TransportWrite};
use crate::devices::virtqueue::{self, ChainOutcome, Descriptor, QueueLayout, DESC_F_WRITE};
use crate::ram::Ram;

const DEVICE_ID_NET: u32 = 1;
/// Ring size offered per queue.
///
/// Not a free choice: Linux's virtio_net stops the transmit queue whenever
/// fewer than `MAX_SKB_FRAGS + 2` descriptors are free (19 on 4K-page riscv64)
/// and only restarts it from a completion callback, so a ring of 16 stops
/// after one frame and never wakes. QEMU offers 256 for the same reason.
const QUEUE_SIZE_MAX: u16 = 256;
/// Receive queue (device writes frames into guest-posted buffers).
const QUEUE_RX: usize = 0;
/// Transmit queue (guest hands frames to the device).
const QUEUE_TX: usize = 1;

/// `VIRTIO_NET_F_MAC` — the MAC below is authoritative, not guest-generated.
const NET_F_MAC: u32 = 5;

/// Size of `struct virtio_net_hdr`. Under VIRTIO_F_VERSION_1 `num_buffers` is
/// in the layout even without MRG_RXBUF, and must read as 1.
const HDR_LEN: usize = 12;
/// Offset of `num_buffers` inside the header.
const HDR_NUM_BUFFERS: usize = 10;

/// Largest Ethernet frame carried, excluding FCS (1500 MTU + 14 header).
const MAX_FRAME_BYTES: usize = 1514;

/// Upper bound on the bytes a single TX chain may describe.
///
/// Descriptor lengths are guest-controlled and sixteen of them can name
/// 64 GiB. Real frames are under 1526 bytes, so anything past 64 KiB is
/// dropped without allocating.
const MAX_CHAIN_BYTES: usize = 64 * 1024;

/// Frames held per direction before the oldest is dropped; Ethernet is
/// allowed to lose frames, an unbounded queue is not allowed to grow.
pub const FRAME_QUEUE_CAPACITY: usize = 64;

/// Locally-administered MAC advertised in configuration space, matching the
/// address QEMU's user-mode networking hands out.
pub const DEFAULT_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

/// Two-queue VirtIO-MMIO Ethernet device.
pub struct VirtioNet {
    transport: Transport,
    mac: [u8; 6],
    /// Frames received from the host, awaiting guest RX buffers.
    inbox: VecDeque<Vec<u8>>,
    /// Frames the guest transmitted, awaiting the machine's network tick.
    outbox: VecDeque<Vec<u8>>,
    /// Chain and payload buffers, reused so steady-state traffic performs no
    /// per-frame allocations.
    chain: Vec<Descriptor>,
    scratch: Vec<u8>,
    /// Cumulative frame bytes delivered into guest RX buffers.
    rx_bytes: u64,
    /// Cumulative frame bytes handed to the host backend.
    tx_bytes: u64,
}

impl VirtioNet {
    pub fn new() -> Self {
        Self {
            transport: Transport::new(2, QUEUE_SIZE_MAX),
            mac: DEFAULT_MAC,
            inbox: VecDeque::new(),
            outbox: VecDeque::new(),
            chain: Vec::new(),
            scratch: Vec::new(),
            rx_bytes: 0,
            tx_bytes: 0,
        }
    }

    /// Cumulative frame bytes delivered to the guest since construction.
    pub fn rx_bytes(&self) -> u64 {
        self.rx_bytes
    }

    /// Cumulative frame bytes handed to the host backend since construction.
    pub fn tx_bytes(&self) -> u64 {
        self.tx_bytes
    }

    pub fn irq_pending(&self) -> bool {
        self.transport.irq_pending()
    }

    /// Removes the oldest frame the guest transmitted, if any. The machine
    /// drains this into the attached [`crate::devices::net::NetBackend`].
    pub fn pop_transmitted(&mut self) -> Option<Vec<u8>> {
        let frame = self.outbox.pop_front()?;
        self.tx_bytes = self.tx_bytes.wrapping_add(frame.len() as u64);
        Some(frame)
    }

    /// Queues a host frame for delivery to the guest, dropping the oldest
    /// pending frame when the bounded inbox is full.
    pub fn receive_frame(&mut self, frame: Vec<u8>) {
        if frame.is_empty() || frame.len() > MAX_FRAME_BYTES {
            return;
        }
        if self.inbox.len() == FRAME_QUEUE_CAPACITY {
            self.inbox.pop_front();
        }
        self.inbox.push_back(frame);
    }

    pub fn pending_rx(&self) -> usize {
        self.inbox.len()
    }

    #[cfg(test)]
    pub fn pending_tx(&self) -> usize {
        self.outbox.len()
    }

    pub fn read(&self, offset: u64, size: u8) -> Result<u64, ()> {
        if offset >= 0x100 {
            // Configuration space is read byte-wise by virtio_mmio for the MAC,
            // so unlike the transport registers it must accept 1/2-byte loads.
            if !matches!(size, 1 | 2 | 4 | 8) {
                return Err(());
            }
            let mut value = 0u64;
            for byte in 0..u64::from(size) {
                let index = offset - 0x100 + byte;
                let raw = self.mac.get(index as usize).copied().unwrap_or(0);
                value |= u64::from(raw) << (byte * 8);
            }
            return Ok(value);
        }
        if size != 4 && size != 8 {
            return Err(());
        }
        // VIRTIO_NET_F_MAC in the low word, VIRTIO_F_VERSION_1 (bit 32) in the
        // high one. No offloads, no mergeable buffers, no MQ.
        Ok(self
            .transport
            .read(offset, DEVICE_ID_NET, [1 << NET_F_MAC, 1])
            .unwrap_or(0))
    }

    pub fn write(&mut self, offset: u64, val: u64, size: u8, ram: &mut Ram) -> Result<(), ()> {
        if size != 4 {
            return Err(());
        }
        match self.transport.write(offset, val as u32) {
            TransportWrite::Notify(queue) if queue as usize == QUEUE_TX => self.process_tx(ram),
            TransportWrite::Notify(queue) if queue as usize == QUEUE_RX => self.deliver_rx(ram),
            TransportWrite::Reset => {
                self.inbox.clear();
                self.outbox.clear();
            }
            _ => {}
        }
        Ok(())
    }

    /// Consumes available TX chains, moving each frame into the outbox.
    fn process_tx(&mut self, ram: &mut Ram) {
        let completed = self
            .transport
            .queue(QUEUE_TX)
            .drain(ram, |ram, layout, head| {
                self.transmit_chain(ram, layout, head)
            });
        self.transport.complete_queue(completed);
    }

    fn transmit_chain(&mut self, ram: &mut Ram, layout: QueueLayout, head: u16) -> ChainOutcome {
        let mut chain = std::mem::take(&mut self.chain);
        let mut scratch = std::mem::take(&mut self.scratch);
        // The driver chooses how to split the header from the frame across
        // descriptors, so the chain is read as one byte stream with the first
        // HDR_LEN bytes skipped. A failure past a well-formed chain drops the
        // frame but still publishes a zero-length used entry; withholding the
        // descriptor would stall the driver's ring.
        let outcome = if !virtqueue::collect_chain(ram, layout, head, &mut chain) {
            ChainOutcome::Skip
        } else if chain.iter().any(|desc| desc.flags & DESC_F_WRITE != 0) {
            ChainOutcome::Used(0)
        } else {
            match virtqueue::total_len(&chain) {
                Some(total) if (HDR_LEN..=MAX_CHAIN_BYTES).contains(&total) => {
                    scratch.clear();
                    if read_segments(ram, &chain, &mut scratch) {
                        let frame = &scratch[HDR_LEN..];
                        if !frame.is_empty() && frame.len() <= MAX_FRAME_BYTES {
                            if self.outbox.len() == FRAME_QUEUE_CAPACITY {
                                self.outbox.pop_front();
                            }
                            self.outbox.push_back(frame.to_vec());
                        }
                    }
                    ChainOutcome::Used(0)
                }
                _ => ChainOutcome::Used(0),
            }
        };
        chain.clear();
        self.chain = chain;
        self.scratch = scratch;
        outcome
    }

    /// Copies queued host frames into guest-posted RX buffers while both a
    /// frame and a chain exist. Safe to call when either side is empty.
    pub fn deliver_rx(&mut self, ram: &mut Ram) {
        if self.inbox.is_empty() {
            return;
        }
        let completed = self
            .transport
            .queue(QUEUE_RX)
            .drain(ram, |ram, layout, head| {
                if self.inbox.is_empty() {
                    // No frame to place: leave the chain posted for a later frame.
                    return ChainOutcome::Retry;
                }
                self.receive_chain(ram, layout, head)
            });
        self.transport.complete_queue(completed);
    }

    fn receive_chain(&mut self, ram: &mut Ram, layout: QueueLayout, head: u16) -> ChainOutcome {
        let mut chain = std::mem::take(&mut self.chain);
        let mut scratch = std::mem::take(&mut self.scratch);
        let outcome = if !virtqueue::collect_chain(ram, layout, head, &mut chain) {
            ChainOutcome::Skip
        } else {
            // Only device-writable descriptors may receive; a chain that mixes
            // in readable ones simply offers less room.
            chain.retain(|desc| desc.flags & DESC_F_WRITE != 0);
            let capacity = virtqueue::total_len(&chain).unwrap_or(0);
            let frame_len = self.inbox.front().map_or(0, Vec::len);
            let needed = HDR_LEN + frame_len;
            if capacity < needed || !segments_in_ram(ram, &chain, needed) {
                // The chain cannot hold this frame (or points outside RAM):
                // drop the frame and recycle the buffer rather than wedging.
                self.inbox.pop_front();
                ChainOutcome::Used(0)
            } else {
                let frame = self.inbox.pop_front().unwrap_or_default();
                scratch.clear();
                scratch.resize(HDR_LEN, 0);
                // num_buffers must read as 1 even without MRG_RXBUF, because
                // VIRTIO_F_VERSION_1 puts the field in the layout regardless.
                scratch[HDR_NUM_BUFFERS] = 1;
                scratch.extend_from_slice(&frame);
                if write_segments(ram, &chain, &scratch) {
                    self.rx_bytes = self.rx_bytes.wrapping_add(frame_len as u64);
                    ChainOutcome::Used(scratch.len() as u32)
                } else {
                    ChainOutcome::Used(0)
                }
            }
        };
        chain.clear();
        self.chain = chain;
        self.scratch = scratch;
        outcome
    }
}

impl Default for VirtioNet {
    fn default() -> Self {
        Self::new()
    }
}

/// Concatenates every descriptor's bytes into `out`, failing on a bad address.
fn read_segments(ram: &Ram, chain: &[Descriptor], out: &mut Vec<u8>) -> bool {
    for desc in chain {
        let Some(offset) = virtqueue::ram_offset(ram, desc.addr, u64::from(desc.len)) else {
            return false;
        };
        let Ok(bytes) = ram.read_slice(offset, desc.len as usize) else {
            return false;
        };
        out.extend_from_slice(bytes);
    }
    true
}

/// Whether the first `needed` bytes described by `chain` are all backed by RAM.
/// Checked before any write so a bad trailing segment cannot leave the guest
/// with half a frame.
fn segments_in_ram(ram: &Ram, chain: &[Descriptor], needed: usize) -> bool {
    let mut remaining = needed;
    for desc in chain {
        if remaining == 0 {
            return true;
        }
        let len = remaining.min(desc.len as usize);
        if virtqueue::ram_offset(ram, desc.addr, len as u64).is_none() {
            return false;
        }
        remaining -= len;
    }
    remaining == 0
}

/// Spreads `data` across the chain's descriptors in order.
fn write_segments(ram: &mut Ram, chain: &[Descriptor], data: &[u8]) -> bool {
    let mut written = 0;
    for desc in chain {
        if written == data.len() {
            break;
        }
        let len = (data.len() - written).min(desc.len as usize);
        let Some(offset) = virtqueue::ram_offset(ram, desc.addr, len as u64) else {
            return false;
        };
        if ram
            .write_slice(offset, &data[written..written + len])
            .is_err()
        {
            return false;
        }
        written += len;
    }
    written == data.len()
}

const SNAPSHOT_VERSION: u16 = 1;

impl VirtioNet {
    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            w.raw(&self.mac);
            w.u64(self.rx_bytes);
            w.u64(self.tx_bytes);
            w.u32(self.inbox.len() as u32);
            for frame in &self.inbox {
                w.bytes(frame);
            }
            w.u32(self.outbox.len() as u32);
            for frame in &self.outbox {
                w.bytes(frame);
            }
        });
        self.transport.snapshot(out);
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("virtio-net", SNAPSHOT_VERSION, |r| {
            self.mac.copy_from_slice(r.raw(6)?);
            self.rx_bytes = r.u64()?;
            self.tx_bytes = r.u64()?;
            let inbox = r.u32()? as usize;
            if inbox > FRAME_QUEUE_CAPACITY {
                return Err(format!("{inbox} queued RX frames exceed the inbox"));
            }
            self.inbox.clear();
            for _ in 0..inbox {
                self.inbox.push_back(r.bytes()?.to_vec());
            }
            let outbox = r.u32()? as usize;
            if outbox > FRAME_QUEUE_CAPACITY {
                return Err(format!("{outbox} queued TX frames exceed the outbox"));
            }
            self.outbox.clear();
            for _ in 0..outbox {
                self.outbox.push_back(r.bytes()?.to_vec());
            }
            Ok(())
        })?;
        self.transport.restore(input)?;
        self.chain.clear();
        self.scratch.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::DRAM_BASE;
    use crate::devices::net::NetBackend;
    use crate::devices::virtio_mmio::MAGIC;
    use crate::devices::virtqueue::DESC_F_NEXT;

    /// Loopback backend: returns every transmitted frame to the guest, so the
    /// TX -> host -> RX path can be exercised without host I/O.
    pub struct LoopbackBackend {
        queue: std::collections::VecDeque<Vec<u8>>,
        pub polls: usize,
        pub last_now_ms: u64,
    }

    impl LoopbackBackend {
        pub fn new() -> Self {
            Self {
                queue: std::collections::VecDeque::new(),
                polls: 0,
                last_now_ms: 0,
            }
        }
    }

    impl Default for LoopbackBackend {
        fn default() -> Self {
            Self::new()
        }
    }

    impl NetBackend for LoopbackBackend {
        fn transmit(&mut self, frame: &[u8]) {
            self.queue.push_back(frame.to_vec());
        }

        fn receive(&mut self) -> Option<Vec<u8>> {
            self.queue.pop_front()
        }

        fn poll(&mut self, now_ms: u64) {
            self.polls += 1;
            self.last_now_ms = now_ms;
        }
    }

    const RX_DESC: u64 = DRAM_BASE + 0x1000;
    const RX_AVAIL: u64 = DRAM_BASE + 0x2000;
    const RX_USED: u64 = DRAM_BASE + 0x3000;
    const TX_DESC: u64 = DRAM_BASE + 0x4000;
    const TX_AVAIL: u64 = DRAM_BASE + 0x5000;
    const TX_USED: u64 = DRAM_BASE + 0x6000;
    const BUF_A: u64 = DRAM_BASE + 0x7000;
    const BUF_B: u64 = DRAM_BASE + 0x9000;

    fn write_desc(
        ram: &mut Ram,
        table: u64,
        index: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let at = table + u64::from(index) * 16 - DRAM_BASE;
        ram.write(at, addr, 8).unwrap();
        ram.write(at + 8, u64::from(len), 4).unwrap();
        ram.write(at + 12, u64::from(flags), 2).unwrap();
        ram.write(at + 14, u64::from(next), 2).unwrap();
    }

    /// Programs both queues the way the Linux driver does: select, size,
    /// addresses, ready.
    fn configure(dev: &mut VirtioNet, ram: &mut Ram) {
        for (sel, desc, avail, used) in [
            (QUEUE_RX as u64, RX_DESC, RX_AVAIL, RX_USED),
            (QUEUE_TX as u64, TX_DESC, TX_AVAIL, TX_USED),
        ] {
            dev.write(0x030, sel, 4, ram).unwrap();
            dev.write(0x038, u64::from(QUEUE_SIZE_MAX), 4, ram).unwrap();
            for (low, high, address) in [
                (0x080, 0x084, desc),
                (0x090, 0x094, avail),
                (0x0a0, 0x0a4, used),
            ] {
                dev.write(low, address as u32 as u64, 4, ram).unwrap();
                dev.write(high, address >> 32, 4, ram).unwrap();
            }
            dev.write(0x044, 1, 4, ram).unwrap();
        }
    }

    /// Publishes `head` as available chain number `count - 1` on `avail`.
    fn post(ram: &mut Ram, avail: u64, slot: u16, head: u16, count: u16) {
        ram.write(
            avail + 4 - DRAM_BASE + u64::from(slot) * 2,
            u64::from(head),
            2,
        )
        .unwrap();
        ram.write(avail + 2 - DRAM_BASE, u64::from(count), 2)
            .unwrap();
    }

    fn ram_bytes(ram: &Ram, addr: u64, len: usize) -> Vec<u8> {
        ram.read_slice(addr - DRAM_BASE, len).unwrap().to_vec()
    }

    fn frame(len: usize, seed: u8) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_add(seed)).collect()
    }

    fn setup() -> (VirtioNet, Ram) {
        let mut dev = VirtioNet::new();
        let mut ram = Ram::new(0x20000);
        configure(&mut dev, &mut ram);
        (dev, ram)
    }

    #[test]
    fn identity_features_and_mac_configuration_space() {
        let mut dev = VirtioNet::new();
        let mut ram = Ram::new(0x1000);
        assert_eq!(dev.read(0x000, 4).unwrap(), u64::from(MAGIC));
        assert_eq!(dev.read(0x004, 4).unwrap(), 2);
        assert_eq!(dev.read(0x008, 4).unwrap(), u64::from(DEVICE_ID_NET));
        assert_eq!(dev.read(0x034, 4).unwrap(), u64::from(QUEUE_SIZE_MAX));

        // VIRTIO_NET_F_MAC low, VIRTIO_F_VERSION_1 high.
        dev.write(0x014, 0, 4, &mut ram).unwrap();
        assert_eq!(dev.read(0x010, 4).unwrap(), 1 << NET_F_MAC);
        dev.write(0x014, 1, 4, &mut ram).unwrap();
        assert_eq!(dev.read(0x010, 4).unwrap(), 1);

        for (index, expected) in DEFAULT_MAC.iter().enumerate() {
            assert_eq!(
                dev.read(0x100 + index as u64, 1).unwrap(),
                u64::from(*expected)
            );
        }

        dev.write(0x070, 15, 4, &mut ram).unwrap();
        assert_eq!(dev.read(0x070, 4).unwrap(), 15);
        dev.write(0x070, 0, 4, &mut ram).unwrap();
        assert_eq!(dev.read(0x070, 4).unwrap(), 0);
        assert_eq!(dev.read(0x044, 4).unwrap(), 0);
    }

    #[test]
    fn transmit_with_header_and_frame_in_one_descriptor() {
        let (mut dev, mut ram) = setup();
        let payload = frame(64, 1);
        ram.write_slice(BUF_A - DRAM_BASE + HDR_LEN as u64, &payload)
            .unwrap();
        write_desc(
            &mut ram,
            TX_DESC,
            0,
            BUF_A,
            (HDR_LEN + payload.len()) as u32,
            0,
            0,
        );
        post(&mut ram, TX_AVAIL, 0, 0, 1);

        dev.write(0x050, QUEUE_TX as u64, 4, &mut ram).unwrap();

        assert_eq!(dev.pop_transmitted().as_deref(), Some(payload.as_slice()));
        assert!(dev.pop_transmitted().is_none());
        assert_eq!(ram.read(TX_USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(TX_USED + 4 - DRAM_BASE, 4).unwrap(), 0);
        // The device writes nothing on transmit, so the used length is zero.
        assert_eq!(ram.read(TX_USED + 8 - DRAM_BASE, 4).unwrap(), 0);
        assert!(dev.irq_pending());
        dev.write(0x064, 1, 4, &mut ram).unwrap();
        assert!(!dev.irq_pending());
    }

    #[test]
    fn transmit_with_header_split_from_frame() {
        let (mut dev, mut ram) = setup();
        let payload = frame(100, 7);
        ram.write_slice(BUF_B - DRAM_BASE, &payload).unwrap();
        write_desc(&mut ram, TX_DESC, 0, BUF_A, HDR_LEN as u32, DESC_F_NEXT, 1);
        write_desc(&mut ram, TX_DESC, 1, BUF_B, payload.len() as u32, 0, 0);
        post(&mut ram, TX_AVAIL, 0, 0, 1);

        dev.write(0x050, QUEUE_TX as u64, 4, &mut ram).unwrap();

        assert_eq!(dev.pop_transmitted().as_deref(), Some(payload.as_slice()));
        assert_eq!(ram.read(TX_USED + 2 - DRAM_BASE, 2).unwrap(), 1);
    }

    #[test]
    fn oversized_and_headerless_transmits_are_dropped_but_still_published() {
        let (mut dev, mut ram) = setup();
        // One byte past the Ethernet maximum.
        write_desc(
            &mut ram,
            TX_DESC,
            0,
            BUF_A,
            (HDR_LEN + MAX_FRAME_BYTES + 1) as u32,
            0,
            0,
        );
        // A chain too short to even hold the header.
        write_desc(&mut ram, TX_DESC, 1, BUF_A, 4, 0, 0);
        ram.write(TX_AVAIL + 4 - DRAM_BASE, 0, 2).unwrap();
        ram.write(TX_AVAIL + 6 - DRAM_BASE, 1, 2).unwrap();
        ram.write(TX_AVAIL + 2 - DRAM_BASE, 2, 2).unwrap();

        dev.write(0x050, QUEUE_TX as u64, 4, &mut ram).unwrap();

        assert_eq!(dev.pending_tx(), 0);
        assert_eq!(ram.read(TX_USED + 2 - DRAM_BASE, 2).unwrap(), 2);
        assert_eq!(ram.read(TX_USED + 8 - DRAM_BASE, 4).unwrap(), 0);
        assert_eq!(ram.read(TX_USED + 16 - DRAM_BASE, 4).unwrap(), 0);
        assert!(dev.irq_pending());
    }

    #[test]
    fn transmit_queue_rejects_a_bogus_avail_backlog() {
        let (mut dev, mut ram) = setup();
        let payload = frame(64, 2);
        ram.write_slice(BUF_A - DRAM_BASE + HDR_LEN as u64, &payload)
            .unwrap();
        write_desc(
            &mut ram,
            TX_DESC,
            0,
            BUF_A,
            (HDR_LEN + payload.len()) as u32,
            0,
            0,
        );
        // A driver may never have more than queue_num (16) chains outstanding.
        post(&mut ram, TX_AVAIL, 0, 0, 40_000);

        dev.write(0x050, QUEUE_TX as u64, 4, &mut ram).unwrap();

        assert_eq!(dev.pending_tx(), 0);
        assert_eq!(ram.read(TX_USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert!(!dev.irq_pending());

        // A later notify with a sane index resumes normally.
        ram.write(TX_AVAIL + 2 - DRAM_BASE, 1, 2).unwrap();
        dev.write(0x050, QUEUE_TX as u64, 4, &mut ram).unwrap();
        assert_eq!(dev.pop_transmitted().as_deref(), Some(payload.as_slice()));
        assert_eq!(ram.read(TX_USED + 2 - DRAM_BASE, 2).unwrap(), 1);
    }

    #[test]
    fn receive_writes_zeroed_header_with_one_buffer_then_the_frame() {
        let (mut dev, mut ram) = setup();
        let payload = frame(200, 3);
        write_desc(&mut ram, RX_DESC, 0, BUF_A, 2048, DESC_F_WRITE, 0);
        post(&mut ram, RX_AVAIL, 0, 0, 1);

        dev.receive_frame(payload.clone());
        dev.deliver_rx(&mut ram);

        let header = ram_bytes(&ram, BUF_A, HDR_LEN);
        let mut expected_header = [0u8; HDR_LEN];
        expected_header[HDR_NUM_BUFFERS] = 1;
        assert_eq!(header, expected_header);
        assert_eq!(
            ram_bytes(&ram, BUF_A + HDR_LEN as u64, payload.len()),
            payload
        );
        assert_eq!(ram.read(RX_USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(
            ram.read(RX_USED + 8 - DRAM_BASE, 4).unwrap(),
            (HDR_LEN + payload.len()) as u64
        );
        assert!(dev.irq_pending());
        assert_eq!(dev.pending_rx(), 0);
    }

    #[test]
    fn byte_counters_advance_for_transmit_and_receive() {
        let (mut dev, mut ram) = setup();
        assert_eq!(dev.tx_bytes(), 0);
        assert_eq!(dev.rx_bytes(), 0);

        let sent = frame(64, 1);
        ram.write_slice(BUF_A - DRAM_BASE + HDR_LEN as u64, &sent)
            .unwrap();
        write_desc(
            &mut ram,
            TX_DESC,
            0,
            BUF_A,
            (HDR_LEN + sent.len()) as u32,
            0,
            0,
        );
        post(&mut ram, TX_AVAIL, 0, 0, 1);
        dev.write(0x050, QUEUE_TX as u64, 4, &mut ram).unwrap();
        // Queued but not yet handed to the backend: nothing counted.
        assert_eq!(dev.tx_bytes(), 0);
        assert!(dev.pop_transmitted().is_some());
        assert_eq!(dev.tx_bytes(), sent.len() as u64);

        let received = frame(200, 3);
        write_desc(&mut ram, RX_DESC, 0, BUF_B, 2048, DESC_F_WRITE, 0);
        post(&mut ram, RX_AVAIL, 0, 0, 1);
        dev.receive_frame(received.clone());
        dev.deliver_rx(&mut ram);
        assert_eq!(dev.rx_bytes(), received.len() as u64);
        assert_eq!(dev.tx_bytes(), sent.len() as u64);
    }

    #[test]
    fn receive_spans_multiple_writable_descriptors() {
        let (mut dev, mut ram) = setup();
        let payload = frame(64, 5);
        // Header lands in the first segment, the frame straddles both.
        write_desc(
            &mut ram,
            RX_DESC,
            0,
            BUF_A,
            16,
            DESC_F_WRITE | DESC_F_NEXT,
            1,
        );
        write_desc(&mut ram, RX_DESC, 1, BUF_B, 2048, DESC_F_WRITE, 0);
        post(&mut ram, RX_AVAIL, 0, 0, 1);

        dev.receive_frame(payload.clone());
        dev.deliver_rx(&mut ram);

        assert_eq!(
            ram.read(BUF_A - DRAM_BASE + HDR_NUM_BUFFERS as u64, 1)
                .unwrap(),
            1
        );
        assert_eq!(ram_bytes(&ram, BUF_A + HDR_LEN as u64, 4), payload[..4]);
        assert_eq!(ram_bytes(&ram, BUF_B, 4), payload[4..8]);
        assert_eq!(
            ram.read(RX_USED + 8 - DRAM_BASE, 4).unwrap(),
            (HDR_LEN + payload.len()) as u64
        );
    }

    #[test]
    fn frames_wait_until_the_guest_posts_receive_buffers() {
        let (mut dev, mut ram) = setup();
        let payload = frame(60, 9);

        dev.receive_frame(payload.clone());
        dev.deliver_rx(&mut ram);

        assert_eq!(dev.pending_rx(), 1);
        assert_eq!(ram.read(RX_USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert!(!dev.irq_pending());

        write_desc(&mut ram, RX_DESC, 0, BUF_A, 2048, DESC_F_WRITE, 0);
        post(&mut ram, RX_AVAIL, 0, 0, 1);
        dev.deliver_rx(&mut ram);

        assert_eq!(dev.pending_rx(), 0);
        assert_eq!(ram.read(RX_USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(
            ram_bytes(&ram, BUF_A + HDR_LEN as u64, payload.len()),
            payload
        );
    }

    #[test]
    fn surplus_receive_buffers_are_left_posted() {
        let (mut dev, mut ram) = setup();
        write_desc(&mut ram, RX_DESC, 0, BUF_A, 2048, DESC_F_WRITE, 0);
        write_desc(&mut ram, RX_DESC, 1, BUF_B, 2048, DESC_F_WRITE, 0);
        ram.write(RX_AVAIL + 4 - DRAM_BASE, 0, 2).unwrap();
        ram.write(RX_AVAIL + 6 - DRAM_BASE, 1, 2).unwrap();
        ram.write(RX_AVAIL + 2 - DRAM_BASE, 2, 2).unwrap();

        dev.receive_frame(frame(60, 4));
        dev.deliver_rx(&mut ram);

        assert_eq!(ram.read(RX_USED + 2 - DRAM_BASE, 2).unwrap(), 1);

        // The unused chain is still available for the next frame.
        dev.receive_frame(frame(60, 6));
        dev.deliver_rx(&mut ram);
        assert_eq!(ram.read(RX_USED + 2 - DRAM_BASE, 2).unwrap(), 2);
    }

    #[test]
    fn a_frame_too_large_for_its_chain_is_dropped_and_the_chain_recycled() {
        let (mut dev, mut ram) = setup();
        write_desc(&mut ram, RX_DESC, 0, BUF_A, 32, DESC_F_WRITE, 0);
        post(&mut ram, RX_AVAIL, 0, 0, 1);

        dev.receive_frame(frame(200, 1));
        dev.deliver_rx(&mut ram);

        assert_eq!(dev.pending_rx(), 0);
        assert_eq!(ram.read(RX_USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(RX_USED + 8 - DRAM_BASE, 4).unwrap(), 0);
    }

    #[test]
    fn receive_backlog_is_bounded_and_drops_the_oldest_frame() {
        let (mut dev, mut ram) = setup();
        for index in 0..FRAME_QUEUE_CAPACITY + 1 {
            dev.receive_frame(frame(60, index as u8));
        }
        assert_eq!(dev.pending_rx(), FRAME_QUEUE_CAPACITY);

        write_desc(&mut ram, RX_DESC, 0, BUF_A, 2048, DESC_F_WRITE, 0);
        post(&mut ram, RX_AVAIL, 0, 0, 1);
        dev.deliver_rx(&mut ram);

        // Frame 0 was evicted, so the first delivery is frame 1.
        assert_eq!(ram_bytes(&ram, BUF_A + HDR_LEN as u64, 60), frame(60, 1));
        assert_eq!(dev.pending_rx(), FRAME_QUEUE_CAPACITY - 1);
    }

    #[test]
    fn transmit_backlog_is_bounded_and_drops_the_oldest_frame() {
        let (mut dev, mut ram) = setup();
        let payload = frame(64, 0);
        ram.write_slice(BUF_A - DRAM_BASE + HDR_LEN as u64, &payload)
            .unwrap();
        write_desc(
            &mut ram,
            TX_DESC,
            0,
            BUF_A,
            (HDR_LEN + payload.len()) as u32,
            0,
            0,
        );
        // Sixteen chains per notify is the ring's maximum; repeat until the
        // bounded outbox has been overrun with nobody draining it.
        for round in 0..8u16 {
            for slot in 0..16u16 {
                ram.write(TX_AVAIL + 4 - DRAM_BASE + u64::from(slot) * 2, 0, 2)
                    .unwrap();
            }
            ram.write(TX_AVAIL + 2 - DRAM_BASE, u64::from((round + 1) * 16), 2)
                .unwrap();
            dev.write(0x050, QUEUE_TX as u64, 4, &mut ram).unwrap();
        }

        assert_eq!(dev.pending_tx(), FRAME_QUEUE_CAPACITY);
    }

    #[test]
    fn receive_queue_rejects_a_bogus_avail_backlog() {
        let (mut dev, mut ram) = setup();
        write_desc(&mut ram, RX_DESC, 0, BUF_A, 2048, DESC_F_WRITE, 0);
        post(&mut ram, RX_AVAIL, 0, 0, 40_000);

        dev.receive_frame(frame(60, 1));
        dev.deliver_rx(&mut ram);

        assert_eq!(dev.pending_rx(), 1);
        assert_eq!(ram.read(RX_USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert!(!dev.irq_pending());
    }

    #[test]
    fn cyclic_receive_chain_is_bounded_and_skipped() {
        let (mut dev, mut ram) = setup();
        write_desc(
            &mut ram,
            RX_DESC,
            0,
            BUF_A,
            2048,
            DESC_F_WRITE | DESC_F_NEXT,
            0,
        );
        post(&mut ram, RX_AVAIL, 0, 0, 1);

        dev.receive_frame(frame(60, 1));
        dev.deliver_rx(&mut ram);

        assert_eq!(ram.read(RX_USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert!(!dev.irq_pending());
        // The frame is untouched and the queue did not wedge.
        assert_eq!(dev.pending_rx(), 1);
    }

    #[test]
    fn receive_buffer_outside_ram_drops_the_frame_without_writing() {
        let (mut dev, mut ram) = setup();
        let end_of_ram = DRAM_BASE + ram.size();
        write_desc(&mut ram, RX_DESC, 0, end_of_ram, 2048, DESC_F_WRITE, 0);
        post(&mut ram, RX_AVAIL, 0, 0, 1);

        dev.receive_frame(frame(60, 1));
        dev.deliver_rx(&mut ram);

        assert_eq!(dev.pending_rx(), 0);
        assert_eq!(ram.read(RX_USED + 8 - DRAM_BASE, 4).unwrap(), 0);
    }

    #[test]
    fn a_loopback_backend_round_trips_a_frame_through_the_machine() {
        use crate::machine::Machine;

        let mut machine = Machine::new(0x20000);
        machine.set_net_backend(Box::new(LoopbackBackend::new()));
        let payload = frame(128, 11);
        {
            let dev = &mut machine.bus.virtio_net;
            let ram = &mut machine.bus.ram;
            configure(dev, ram);
            ram.write_slice(BUF_A - DRAM_BASE + HDR_LEN as u64, &payload)
                .unwrap();
            write_desc(
                ram,
                TX_DESC,
                0,
                BUF_A,
                (HDR_LEN + payload.len()) as u32,
                0,
                0,
            );
            post(ram, TX_AVAIL, 0, 0, 1);
            write_desc(ram, RX_DESC, 0, BUF_B, 2048, DESC_F_WRITE, 0);
            post(ram, RX_AVAIL, 0, 0, 1);
            dev.write(0x050, QUEUE_TX as u64, 4, ram).unwrap();
        }
        assert_eq!(machine.bus.virtio_net.pending_tx(), 1);

        machine.tick_net();

        assert_eq!(machine.bus.virtio_net.pending_tx(), 0);
        assert_eq!(
            ram_bytes(&machine.bus.ram, BUF_B + HDR_LEN as u64, payload.len()),
            payload
        );
        assert_eq!(
            machine.bus.ram.read(RX_USED + 8 - DRAM_BASE, 4).unwrap(),
            (HDR_LEN + payload.len()) as u64
        );
        assert!(machine.bus.virtio_net.irq_pending());
    }

    #[test]
    fn without_a_backend_the_link_is_down() {
        use crate::machine::Machine;

        let mut machine = Machine::new(0x20000);
        {
            let dev = &mut machine.bus.virtio_net;
            let ram = &mut machine.bus.ram;
            configure(dev, ram);
            write_desc(ram, TX_DESC, 0, BUF_A, (HDR_LEN + 64) as u32, 0, 0);
            post(ram, TX_AVAIL, 0, 0, 1);
            write_desc(ram, RX_DESC, 0, BUF_B, 2048, DESC_F_WRITE, 0);
            post(ram, RX_AVAIL, 0, 0, 1);
            dev.write(0x050, QUEUE_TX as u64, 4, ram).unwrap();
        }

        assert!(!machine.has_net_backend());
        assert_eq!(machine.net_deadline_ms(), None);
        machine.tick_net();

        // Nothing was delivered: the frame has nowhere to go and none arrives.
        assert_eq!(machine.bus.ram.read(RX_USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert_eq!(machine.bus.virtio_net.pending_rx(), 0);
    }
}
