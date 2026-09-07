//! Minimal VirtIO 1.x entropy device over the MMIO transport.
//!
//! The device owns no generator: hosts inject bytes from their platform
//! CSPRNG, so the core stays deterministic in tests and never exposes a weak
//! fallback to the guest.

use crate::snapshot::{Reader, Writer};

use std::collections::VecDeque;

use crate::devices::virtio_mmio::{Transport, TransportWrite};
use crate::devices::virtqueue::{
    self, ChainOutcome, Descriptor, QueueLayout, DESC_F_INDIRECT, DESC_F_WRITE,
};
use crate::ram::Ram;

const DEVICE_ID_ENTROPY: u32 = 4;
const QUEUE_SIZE_MAX: u16 = 8;
/// Sole queue index (entropy requests).
const QUEUE_ENTROPY: usize = 0;
const ENTROPY_CAPACITY: usize = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

enum RequestOutcome {
    Completed(u32),
    Pending,
    Malformed,
}

/// One-queue VirtIO-MMIO entropy device backed by host-injected bytes.
pub struct VirtioEntropy {
    entropy: VecDeque<u8>,
    transport: Transport,
    chain: Vec<Descriptor>,
}

impl VirtioEntropy {
    pub fn new() -> Self {
        Self {
            entropy: VecDeque::with_capacity(ENTROPY_CAPACITY),
            transport: Transport::new(1, QUEUE_SIZE_MAX),
            chain: Vec::new(),
        }
    }

    /// Number of bytes the bounded entropy buffer can currently accept.
    pub fn entropy_needed(&self) -> usize {
        ENTROPY_CAPACITY - self.entropy.len()
    }

    /// Adds as many bytes as fit and services any queue requests waiting for
    /// entropy. Returns the number of bytes accepted.
    pub fn add_entropy(&mut self, bytes: &[u8], ram: &mut Ram) -> usize {
        let accepted = bytes.len().min(self.entropy_needed());
        self.entropy.extend(&bytes[..accepted]);
        self.process_queue(ram);
        accepted
    }

    pub fn irq_pending(&self) -> bool {
        self.transport.irq_pending()
    }

    pub fn read(&self, offset: u64, size: u8) -> Result<u64, ()> {
        if size != 4 && size != 8 {
            return Err(());
        }
        // VIRTIO_F_VERSION_1 (bit 32) is the only offered feature.
        Ok(self
            .transport
            .read(offset, DEVICE_ID_ENTROPY, [0, 1])
            .unwrap_or(0))
    }

    pub fn write(&mut self, offset: u64, val: u64, size: u8, ram: &mut Ram) -> Result<(), ()> {
        if size != 4 {
            return Err(());
        }
        if let TransportWrite::Notify(0) = self.transport.write(offset, val as u32) {
            self.process_queue(ram);
        }
        Ok(())
    }

    fn process_queue(&mut self, ram: &mut Ram) {
        let completed = self
            .transport
            .queue(QUEUE_ENTROPY)
            .drain(ram, |ram, layout, head| {
                match self.process_request(ram, layout, head) {
                    RequestOutcome::Completed(used_len) => ChainOutcome::Used(used_len),
                    // Requests that cannot be served yet stay available: the next
                    // refill retries them from the same ring position.
                    RequestOutcome::Pending => ChainOutcome::Retry,
                    RequestOutcome::Malformed => ChainOutcome::Skip,
                }
            });
        self.transport.complete_queue(completed);
    }

    fn process_request(&mut self, ram: &mut Ram, layout: QueueLayout, head: u16) -> RequestOutcome {
        let mut chain = std::mem::take(&mut self.chain);
        let outcome = self.process_chain(ram, layout, head, &mut chain);
        chain.clear();
        self.chain = chain;
        outcome
    }

    fn process_chain(
        &mut self,
        ram: &mut Ram,
        layout: QueueLayout,
        head: u16,
        chain: &mut Vec<Descriptor>,
    ) -> RequestOutcome {
        if !virtqueue::collect_chain(ram, layout, head, chain)
            || chain.is_empty()
            || chain
                .iter()
                .any(|desc| desc.flags & DESC_F_WRITE == 0 || desc.flags & DESC_F_INDIRECT != 0)
        {
            return RequestOutcome::Malformed;
        }
        let Some(total) = virtqueue::total_len(chain) else {
            return RequestOutcome::Malformed;
        };
        if total == 0 || total > MAX_REQUEST_BYTES {
            return RequestOutcome::Malformed;
        }
        if chain
            .iter()
            .any(|desc| virtqueue::ram_offset(ram, desc.addr, u64::from(desc.len)).is_none())
        {
            return RequestOutcome::Malformed;
        }
        if self.entropy.is_empty() {
            return RequestOutcome::Pending;
        }

        let mut remaining = total.min(self.entropy.len());
        let written = remaining;
        for desc in chain {
            if remaining == 0 {
                break;
            }
            let len = remaining.min(desc.len as usize);
            if !self.write_entropy(ram, desc.addr, len) {
                return RequestOutcome::Malformed;
            }
            remaining -= len;
        }
        RequestOutcome::Completed(written as u32)
    }

    fn write_entropy(&mut self, ram: &mut Ram, addr: u64, len: usize) -> bool {
        let Some(offset) = virtqueue::ram_offset(ram, addr, len as u64) else {
            return false;
        };
        let mut written = 0;
        while written < len {
            let chunk_len = {
                let (first, second) = self.entropy.as_slices();
                let source = if first.is_empty() { second } else { first };
                let chunk_len = source.len().min(len - written);
                if ram
                    .write_slice(offset + written as u64, &source[..chunk_len])
                    .is_err()
                {
                    return false;
                }
                chunk_len
            };
            self.entropy.drain(..chunk_len);
            written += chunk_len;
        }
        true
    }
}

impl Default for VirtioEntropy {
    fn default() -> Self {
        Self::new()
    }
}

const SNAPSHOT_VERSION: u16 = 1;

impl VirtioEntropy {
    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            let entropy: Vec<u8> = self.entropy.iter().copied().collect();
            w.bytes(&entropy);
        });
        self.transport.snapshot(out);
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("virtio-rng", SNAPSHOT_VERSION, |r| {
            let entropy = r.bytes()?;
            if entropy.len() > ENTROPY_CAPACITY {
                return Err(format!(
                    "{} buffered bytes exceed the reservoir",
                    entropy.len()
                ));
            }
            self.entropy = entropy.iter().copied().collect();
            Ok(())
        })?;
        self.transport.restore(input)?;
        self.chain.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::DRAM_BASE;
    use crate::devices::virtio_mmio::MAGIC;
    use crate::devices::virtqueue::DESC_F_NEXT;

    const DESC: u64 = DRAM_BASE + 0x1000;
    const AVAIL: u64 = DRAM_BASE + 0x2000;
    const USED: u64 = DRAM_BASE + 0x3000;
    const DATA: u64 = DRAM_BASE + 0x4000;

    fn write_desc(ram: &mut Ram, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let at = DESC + u64::from(index) * 16 - DRAM_BASE;
        ram.write(at, addr, 8).unwrap();
        ram.write(at + 8, u64::from(len), 4).unwrap();
        ram.write(at + 12, u64::from(flags), 2).unwrap();
        ram.write(at + 14, u64::from(next), 2).unwrap();
    }

    fn configure(dev: &mut VirtioEntropy, ram: &mut Ram, len: u32) {
        dev.write(0x038, 8, 4, ram).unwrap();
        dev.write(0x080, DESC as u32 as u64, 4, ram).unwrap();
        dev.write(0x084, DESC >> 32, 4, ram).unwrap();
        dev.write(0x090, AVAIL as u32 as u64, 4, ram).unwrap();
        dev.write(0x094, AVAIL >> 32, 4, ram).unwrap();
        dev.write(0x0a0, USED as u32 as u64, 4, ram).unwrap();
        dev.write(0x0a4, USED >> 32, 4, ram).unwrap();
        dev.write(0x044, 1, 4, ram).unwrap();
        write_desc(ram, 0, DATA, len, DESC_F_WRITE, 0);
        ram.write(AVAIL + 4 - DRAM_BASE, 0, 2).unwrap();
        ram.write(AVAIL + 2 - DRAM_BASE, 1, 2).unwrap();
    }

    #[test]
    fn identity_features_and_reset_are_virtio_1_compliant() {
        let mut dev = VirtioEntropy::new();
        let mut ram = Ram::new(0x10000);
        assert_eq!(dev.read(0, 4).unwrap(), u64::from(MAGIC));
        assert_eq!(dev.read(8, 4).unwrap(), u64::from(DEVICE_ID_ENTROPY));
        dev.write(0x014, 1, 4, &mut ram).unwrap();
        assert_eq!(dev.read(0x010, 4).unwrap(), 1);
        dev.write(0x070, 15, 4, &mut ram).unwrap();
        dev.write(0x070, 0, 4, &mut ram).unwrap();
        assert_eq!(dev.read(0x070, 4).unwrap(), 0);
        assert_eq!(dev.read(0x044, 4).unwrap(), 0);
    }

    #[test]
    fn injected_entropy_completes_request_and_interrupts() {
        let mut dev = VirtioEntropy::new();
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, 4);
        assert_eq!(dev.add_entropy(&[1, 2, 3, 4], &mut ram), 4);
        assert_eq!(ram.read_slice(DATA - DRAM_BASE, 4).unwrap(), &[1, 2, 3, 4]);
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 4);
        assert!(dev.irq_pending());
        dev.write(0x064, 1, 4, &mut ram).unwrap();
        assert!(!dev.irq_pending());
    }

    #[test]
    fn empty_source_leaves_request_pending_until_refill() {
        let mut dev = VirtioEntropy::new();
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, 8);

        dev.write(0x050, 0, 4, &mut ram).unwrap();
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 0);

        dev.add_entropy(&[0xa5; 8], &mut ram);
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 1);
    }

    #[test]
    fn refill_is_bounded() {
        let mut dev = VirtioEntropy::new();
        let mut ram = Ram::new(1);
        let input = vec![0x5a; ENTROPY_CAPACITY + 1];

        assert_eq!(dev.add_entropy(&input, &mut ram), ENTROPY_CAPACITY);
        assert_eq!(dev.entropy_needed(), 0);
    }

    #[test]
    fn hostile_avail_backlog_is_rejected() {
        let mut dev = VirtioEntropy::new();
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, 4);
        ram.write(AVAIL + 2 - DRAM_BASE, 40_000, 2).unwrap();

        dev.add_entropy(&[1, 2, 3, 4], &mut ram);

        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert!(!dev.irq_pending());
    }

    #[test]
    fn out_of_ram_descriptor_is_skipped_without_consuming_entropy() {
        let mut dev = VirtioEntropy::new();
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, 4);
        let end_of_ram = DRAM_BASE + ram.size();
        write_desc(&mut ram, 0, end_of_ram, 4, DESC_F_WRITE, 0);

        dev.add_entropy(&[1, 2, 3, 4], &mut ram);

        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert_eq!(dev.entropy_needed(), ENTROPY_CAPACITY - 4);
    }

    #[test]
    fn cyclic_descriptor_chain_is_bounded_and_skipped() {
        let mut dev = VirtioEntropy::new();
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, 4);
        write_desc(&mut ram, 0, DATA, 4, DESC_F_WRITE | DESC_F_NEXT, 0);

        dev.add_entropy(&[1, 2, 3, 4], &mut ram);

        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert!(!dev.irq_pending());
    }

    #[test]
    fn out_of_ram_used_ring_does_not_consume_entropy_or_write_guest_data() {
        let mut dev = VirtioEntropy::new();
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, 4);
        let truncated_used = DRAM_BASE + ram.size() - 4;
        dev.write(0x0a0, truncated_used as u32 as u64, 4, &mut ram)
            .unwrap();
        dev.write(0x0a4, truncated_used >> 32, 4, &mut ram).unwrap();

        dev.add_entropy(&[1, 2, 3, 4], &mut ram);

        assert_eq!(dev.entropy_needed(), ENTROPY_CAPACITY - 4);
        assert_eq!(ram.read_slice(DATA - DRAM_BASE, 4).unwrap(), &[0, 0, 0, 0]);
    }

    #[test]
    fn partial_entropy_completes_with_actual_used_length() {
        let mut dev = VirtioEntropy::new();
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, 16);

        dev.add_entropy(&[9, 8, 7], &mut ram);

        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 3);
        assert_eq!(ram.read_slice(DATA - DRAM_BASE, 3).unwrap(), &[9, 8, 7]);
    }
}
