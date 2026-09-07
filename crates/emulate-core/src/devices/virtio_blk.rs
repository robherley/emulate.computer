//! Minimal VirtIO 1.x block device over the MMIO transport.
//!
//! This implements one split queue with direct descriptor chains, block
//! read/write/flush requests, and a level interrupt.

use crate::snapshot::{Reader, Writer};

use crate::devices::virtio_mmio::{Transport, TransportWrite};
use crate::devices::virtqueue::{
    self, ChainOutcome, Descriptor, QueueLayout, DESC_F_NEXT, DESC_F_WRITE,
};
use crate::ram::Ram;

const DEVICE_ID_BLOCK: u32 = 2;
const QUEUE_SIZE_MAX: u16 = 8;
/// Sole queue index (requests).
const QUEUE_REQUEST: usize = 0;

const BLK_T_IN: u32 = 0;
const BLK_T_OUT: u32 = 1;
const BLK_T_FLUSH: u32 = 4;
/// Indicates that `size_max` in the config space is valid.
const BLK_F_SIZE_MAX: u32 = 1;
const BLK_F_FLUSH: u32 = 9;
/// Offset of `size_max` within `struct virtio_blk_config`, in the MMIO
/// device-configuration window that starts at 0x100.
const BLK_CONFIG_SIZE_MAX: u64 = 0x108;

/// Upper bound on the data bytes carried by a single request chain.
///
/// Descriptor lengths are guest-controlled, so a buggy driver can describe a
/// request spanning the whole disk; sizing the bounce buffer from that aborts
/// the machine on a wasm allocation failure. Oversized requests fail IOERR.
///
/// The same number is published as `size_max` (see [`BLK_F_SIZE_MAX`]) so a
/// well-behaved driver never hits that IOERR: without it Linux builds
/// single-segment requests up to its own 1280 KiB `max_sectors` default and a
/// large writeback would take an unrecoverable EIO. The enforced bound and the
/// published bound must stay the same number.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    OutOfRange,
    Io,
}

/// Fixed-capacity random-access storage behind the VirtIO block device.
pub trait BlockBackend {
    fn capacity_sectors(&self) -> u64;
    fn read_at(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), BlockError>;
    fn write_at(&mut self, sector: u64, buf: &[u8]) -> Result<(), BlockError>;
    fn flush(&mut self) -> Result<(), BlockError>;
}

pub(crate) struct MemBackend {
    disk: Vec<u8>,
}

impl MemBackend {
    pub fn new(image: &[u8]) -> Self {
        Self {
            disk: image.to_vec(),
        }
    }
}

impl BlockBackend for MemBackend {
    fn capacity_sectors(&self) -> u64 {
        self.disk.len() as u64 / 512
    }

    fn read_at(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let range = checked_range(sector, buf.len(), self.disk.len())?;
        buf.copy_from_slice(&self.disk[range]);
        Ok(())
    }

    fn write_at(&mut self, sector: u64, buf: &[u8]) -> Result<(), BlockError> {
        let range = checked_range(sector, buf.len(), self.disk.len())?;
        self.disk[range].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        Ok(())
    }
}

pub struct VirtioBlock {
    backend: Option<Box<dyn BlockBackend>>,
    transport: Transport,
    /// Bounce buffer for request payloads, reused so steady-state I/O
    /// performs no allocations.
    scratch: Vec<u8>,
    /// Descriptor-chain buffer, reused for the same reason.
    chain: Vec<Descriptor>,
    /// Cumulative payload bytes handed to the guest by completed reads.
    bytes_read: u64,
    /// Cumulative payload bytes taken from the guest by completed writes.
    bytes_written: u64,
}

impl VirtioBlock {
    pub fn new() -> Self {
        Self {
            backend: None,
            transport: Transport::new(1, QUEUE_SIZE_MAX),
            scratch: Vec::new(),
            chain: Vec::new(),
            bytes_read: 0,
            bytes_written: 0,
        }
    }

    /// Cumulative payload bytes read from the disk since construction.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Cumulative payload bytes written to the disk since construction.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Replace the volatile, in-memory disk contents.
    pub fn set_disk(&mut self, image: &[u8]) {
        self.set_backend(Box::new(MemBackend::new(image)));
    }

    pub fn set_backend(&mut self, backend: Box<dyn BlockBackend>) {
        self.backend = Some(backend);
    }

    /// Test-only readback of `len` bytes at byte `offset`, taken through the
    /// backend's ordinary sector `read_at` path.
    #[cfg(test)]
    fn read_back(&mut self, offset: usize, len: usize) -> Vec<u8> {
        let first = offset / 512;
        let start = offset % 512;
        let mut buf = vec![0u8; 512 * (start + len).div_ceil(512)];
        self.backend
            .as_deref_mut()
            .expect("a backend must be attached")
            .read_at(first as u64, &mut buf)
            .expect("readback must stay in range");
        buf[start..start + len].to_vec()
    }

    pub fn flush(&mut self) -> Result<(), BlockError> {
        self.backend.as_deref_mut().ok_or(BlockError::Io)?.flush()
    }

    pub fn irq_pending(&self) -> bool {
        self.transport.irq_pending()
    }

    pub fn read(&self, offset: u64, size: u8) -> Result<u64, ()> {
        if size != 4 && size != 8 {
            return Err(());
        }
        // VIRTIO_F_VERSION_1 lives in bit 32, i.e. bit 0 of the second word.
        if let Some(value) = self.transport.read(
            offset,
            DEVICE_ID_BLOCK,
            [(1 << BLK_F_SIZE_MAX) | (1 << BLK_F_FLUSH), 1],
        ) {
            return Ok(value);
        }
        let value = match offset {
            // Block configuration: capacity in 512-byte sectors.
            0x100 if size == 8 => self.capacity_sectors(),
            0x100 => self.capacity_sectors() & 0xffff_ffff,
            0x104 => self.capacity_sectors() >> 32,
            // `size_max`: the largest segment this device will accept. `seg_max`
            // sits in the next word and is not advertised, so it stays zero.
            BLK_CONFIG_SIZE_MAX => MAX_REQUEST_BYTES as u64,
            _ => 0,
        };
        Ok(value)
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

    fn capacity_sectors(&self) -> u64 {
        self.backend
            .as_deref()
            .map(BlockBackend::capacity_sectors)
            .unwrap_or(0)
    }

    fn process_queue(&mut self, ram: &mut Ram) {
        let completed = self
            .transport
            .queue(QUEUE_REQUEST)
            .drain(ram, |ram, layout, head| {
                // No status byte means no used entry: publishing one would claim
                // completion while leaving the status uninitialized. The queue
                // still advances so it does not wedge on the bad chain.
                match self.process_request(ram, layout, head) {
                    Some(used_len) => ChainOutcome::Used(used_len),
                    None => ChainOutcome::Skip,
                }
            });
        self.transport.complete_queue(completed);
    }

    /// Runs one descriptor chain.
    ///
    /// Returns `Some(used_len)` when a used entry should be published (the
    /// status byte has been written: 0 on success, 1 on any failure) and
    /// `None` when no status descriptor could be located, in which case the
    /// caller must not publish anything for this chain.
    fn process_request(&mut self, ram: &mut Ram, layout: QueueLayout, head: u16) -> Option<u32> {
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
    ) -> Option<u32> {
        if !virtqueue::collect_chain(ram, layout, head, chain) {
            return None;
        }
        // The last descriptor carries the status byte. If it is not writable,
        // non-empty and RAM-backed there is nowhere to report a failure, so
        // the whole chain is unpublishable.
        let status = *chain.last()?;
        if status.len < 1
            || status.flags & DESC_F_WRITE == 0
            || virtqueue::ram_offset(ram, status.addr, 1).is_none()
        {
            return None;
        }
        // Past this point every failure is reported through the status byte
        // rather than dropped silently.
        let used_len = self.execute(ram, chain);
        let ok = used_len.is_some();
        // 0 = OK, 1 = IOERR. xv6 treats every non-zero value as fatal.
        ram.write(
            virtqueue::ram_offset(ram, status.addr, 1)?,
            u64::from(!ok),
            1,
        )
        .ok()?;
        Some(used_len.unwrap_or(0))
    }

    /// Performs the request described by `chain`, returning the number of
    /// bytes written into guest buffers on success and `None` on any failure.
    fn execute(&mut self, ram: &mut Ram, chain: &[Descriptor]) -> Option<u32> {
        if chain.len() < 2 {
            return None;
        }
        let header = chain[0];
        if header.len < 16 || header.flags & DESC_F_NEXT == 0 || header.flags & DESC_F_WRITE != 0 {
            return None;
        }
        let data = &chain[1..chain.len() - 1];
        let data_len = total_len(data).ok()?;
        if data_len > MAX_REQUEST_BYTES {
            return None;
        }

        let request_type = virtqueue::read_u32(ram, header.addr)?;
        let sector = virtqueue::read_u64(ram, header.addr.checked_add(8)?)?;
        let result = match request_type {
            BLK_T_IN => self.read_segments(ram, sector, data),
            BLK_T_OUT => self.write_segments(ram, sector, data),
            BLK_T_FLUSH if data.is_empty() => self.flush(),
            _ => Err(BlockError::Io),
        };
        result.ok()?;
        match request_type {
            BLK_T_IN => self.bytes_read = self.bytes_read.wrapping_add(data_len as u64),
            BLK_T_OUT => self.bytes_written = self.bytes_written.wrapping_add(data_len as u64),
            _ => {}
        }
        u32::try_from(data_len).ok()
    }

    fn read_segments(
        &mut self,
        ram: &mut Ram,
        sector: u64,
        segments: &[Descriptor],
    ) -> Result<(), BlockError> {
        let total = total_len(segments)?;
        if segments.iter().any(|desc| desc.flags & DESC_F_WRITE == 0) {
            return Err(BlockError::Io);
        }
        self.validate_transfer(ram, sector, total, segments)?;
        // clear + resize keeps the allocation, so steady-state I/O never
        // allocates.
        let mut data = std::mem::take(&mut self.scratch);
        data.clear();
        let result = data
            .try_reserve_exact(total)
            .map_err(|_| BlockError::Io)
            .and_then(|()| {
                data.resize(total, 0);
                self.backend_mut()?.read_at(sector, &mut data)
            })
            .and_then(|()| {
                let mut offset = 0;
                for desc in segments {
                    copy_slice_to_ram(&data[offset..offset + desc.len as usize], ram, desc.addr)?;
                    offset += desc.len as usize;
                }
                Ok(())
            });
        self.scratch = data;
        result
    }

    fn write_segments(
        &mut self,
        ram: &Ram,
        sector: u64,
        segments: &[Descriptor],
    ) -> Result<(), BlockError> {
        let total = total_len(segments)?;
        if segments.iter().any(|desc| desc.flags & DESC_F_WRITE != 0) {
            return Err(BlockError::Io);
        }
        self.validate_transfer(ram, sector, total, segments)?;
        let mut data = std::mem::take(&mut self.scratch);
        data.clear();
        let result = (|| {
            data.try_reserve_exact(total).map_err(|_| BlockError::Io)?;
            for desc in segments {
                copy_ram_to_vec(ram, desc.addr, desc.len, &mut data)?;
            }
            self.backend_mut()?.write_at(sector, &data)
        })();
        self.scratch = data;
        result
    }

    fn backend_mut(&mut self) -> Result<&mut (dyn BlockBackend + '_), BlockError> {
        match self.backend.as_deref_mut() {
            Some(backend) => Ok(backend),
            None => Err(BlockError::Io),
        }
    }

    fn validate_transfer(
        &self,
        ram: &Ram,
        sector: u64,
        total: usize,
        segments: &[Descriptor],
    ) -> Result<(), BlockError> {
        let start = sector.checked_mul(512).ok_or(BlockError::OutOfRange)?;
        let end = start
            .checked_add(total as u64)
            .ok_or(BlockError::OutOfRange)?;
        if end > self.capacity_sectors().saturating_mul(512)
            || segments
                .iter()
                .any(|desc| virtqueue::ram_offset(ram, desc.addr, desc.len as u64).is_none())
        {
            return Err(BlockError::OutOfRange);
        }
        Ok(())
    }
}

impl Default for VirtioBlock {
    fn default() -> Self {
        Self::new()
    }
}

fn checked_range(
    sector: u64,
    len: usize,
    capacity: usize,
) -> Result<std::ops::Range<usize>, BlockError> {
    let start = sector
        .checked_mul(512)
        .and_then(|offset| usize::try_from(offset).ok())
        .ok_or(BlockError::OutOfRange)?;
    let end = start.checked_add(len).ok_or(BlockError::OutOfRange)?;
    if end > capacity {
        return Err(BlockError::OutOfRange);
    }
    Ok(start..end)
}

fn total_len(segments: &[Descriptor]) -> Result<usize, BlockError> {
    virtqueue::total_len(segments).ok_or(BlockError::OutOfRange)
}

fn copy_slice_to_ram(data: &[u8], ram: &mut Ram, addr: u64) -> Result<(), BlockError> {
    let ram_offset =
        virtqueue::ram_offset(ram, addr, data.len() as u64).ok_or(BlockError::OutOfRange)?;
    // One bounds check and one page-generation bump per segment: going byte
    // by byte made a 128 KiB request 131072 write() calls.
    ram.write_slice(ram_offset, data)
        .map_err(|_| BlockError::Io)
}

fn copy_ram_to_vec(ram: &Ram, addr: u64, len: u32, data: &mut Vec<u8>) -> Result<(), BlockError> {
    let ram_offset = virtqueue::ram_offset(ram, addr, len as u64).ok_or(BlockError::OutOfRange)?;
    let bytes = ram
        .read_slice(ram_offset, len as usize)
        .map_err(|_| BlockError::Io)?;
    data.extend_from_slice(bytes);
    Ok(())
}

const SNAPSHOT_VERSION: u16 = 1;

impl VirtioBlock {
    /// Write one block straight to the backend, bypassing the virtqueue.
    ///
    /// Used only to lay a snapshot's disk overlay down before the guest
    /// resumes; the byte counters stay untouched, since the guest did not do
    /// this I/O and a host-applied overlay must not read as guest writes.
    pub(crate) fn host_write(&mut self, sector: u64, data: &[u8]) -> Result<(), BlockError> {
        let backend = self.backend.as_deref_mut().ok_or(BlockError::Io)?;
        backend.write_at(sector, data)
    }

    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            w.u64(self.bytes_read);
            w.u64(self.bytes_written);
        });
        self.transport.snapshot(out);
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("virtio-blk", SNAPSHOT_VERSION, |r| {
            self.bytes_read = r.u64()?;
            self.bytes_written = r.u64()?;
            Ok(())
        })?;
        self.transport.restore(input)?;
        // Reusable scratch, not state: a request in flight cannot straddle a
        // capture, which only happens between instructions.
        self.scratch.clear();
        self.chain.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::DRAM_BASE;
    use crate::devices::virtio_mmio::MAGIC;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const DESC: u64 = DRAM_BASE + 0x1000;
    const AVAIL: u64 = DRAM_BASE + 0x2000;
    const USED: u64 = DRAM_BASE + 0x3000;
    const HEADER: u64 = DRAM_BASE + 0x4000;
    const DATA: u64 = DRAM_BASE + 0x5000;
    const STATUS: u64 = DRAM_BASE + 0x6000;
    const DATA_2: u64 = DRAM_BASE + 0x7000;

    fn write_desc(ram: &mut Ram, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let at = DESC + index as u64 * 16;
        ram.write(at - DRAM_BASE, addr, 8).unwrap();
        ram.write(at + 8 - DRAM_BASE, len as u64, 4).unwrap();
        ram.write(at + 12 - DRAM_BASE, flags as u64, 2).unwrap();
        ram.write(at + 14 - DRAM_BASE, next as u64, 2).unwrap();
    }

    fn configure(
        dev: &mut VirtioBlock,
        ram: &mut Ram,
        request_type: u32,
        sector: u64,
        data_flags: u16,
    ) {
        dev.write(0x038, 8, 4, ram).unwrap();
        dev.write(0x080, DESC as u32 as u64, 4, ram).unwrap();
        dev.write(0x084, (DESC >> 32) as u32 as u64, 4, ram)
            .unwrap();
        dev.write(0x090, AVAIL as u32 as u64, 4, ram).unwrap();
        dev.write(0x094, (AVAIL >> 32) as u32 as u64, 4, ram)
            .unwrap();
        dev.write(0x0a0, USED as u32 as u64, 4, ram).unwrap();
        dev.write(0x0a4, (USED >> 32) as u32 as u64, 4, ram)
            .unwrap();
        dev.write(0x044, 1, 4, ram).unwrap();
        ram.write(HEADER - DRAM_BASE, request_type as u64, 4)
            .unwrap();
        ram.write(HEADER + 8 - DRAM_BASE, sector, 8).unwrap();
        write_desc(ram, 0, HEADER, 16, DESC_F_NEXT, 1);
        write_desc(ram, 1, DATA, 512, data_flags | DESC_F_NEXT, 2);
        write_desc(ram, 2, STATUS, 1, DESC_F_WRITE, 0);
        ram.write(AVAIL + 4 - DRAM_BASE, 0, 2).unwrap();
        ram.write(AVAIL + 2 - DRAM_BASE, 1, 2).unwrap();
    }

    #[test]
    fn identity_capacity_and_reset() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        assert_eq!(dev.read(0, 4).unwrap(), MAGIC as u64);
        assert_eq!(dev.read(4, 4).unwrap(), 2);
        assert_eq!(dev.read(8, 4).unwrap(), 2);
        assert_eq!(dev.read(0x100, 8).unwrap(), 2);
        dev.write(0x070, 15, 4, &mut ram).unwrap();
        assert_eq!(dev.read(0x070, 4).unwrap(), 15);
        dev.write(0x070, 0, 4, &mut ram).unwrap();
        assert_eq!(dev.read(0x070, 4).unwrap(), 0);
        assert_eq!(dev.read(0x044, 4).unwrap(), 0);
    }

    #[test]
    fn read_request_completes_and_interrupts() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        let mut disk = vec![0; 1024];
        disk[512..1024].fill(0xa5);
        dev.set_disk(&disk);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        dev.write(0x050, 0, 4, &mut ram).unwrap();
        assert_eq!(ram.read(DATA - DRAM_BASE, 1).unwrap(), 0xa5);
        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 0);
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(USED + 4 - DRAM_BASE, 4).unwrap(), 0);
        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 512);
        assert!(dev.irq_pending());
        dev.write(0x064, 1, 4, &mut ram).unwrap();
        assert!(!dev.irq_pending());
    }

    #[test]
    fn byte_counters_advance_for_reads_and_writes() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        assert_eq!(dev.bytes_read(), 0);
        assert_eq!(dev.bytes_written(), 0);

        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        dev.write(0x050, 0, 4, &mut ram).unwrap();
        assert_eq!(dev.bytes_read(), 512);
        assert_eq!(dev.bytes_written(), 0);

        // A second request needs a fresh ring (`configure` always republishes
        // avail index 1), so exercise the write direction on its own device.
        let mut writer = VirtioBlock::new();
        writer.set_disk(&vec![0; 1024]);
        configure(&mut writer, &mut ram, BLK_T_OUT, 0, 0);
        writer.write(0x050, 0, 4, &mut ram).unwrap();
        assert_eq!(writer.bytes_written(), 512);
        assert_eq!(writer.bytes_read(), 0);
    }

    #[test]
    fn read_request_bumps_data_page_generation_once() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0xa5; 1024]);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        let data_page = (DATA - DRAM_BASE) >> 12;
        let generation = ram.generation_for_page(data_page).unwrap();

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.generation_for_page(data_page), Some(generation + 1));
    }

    #[test]
    fn write_request_updates_disk() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, BLK_T_OUT, 0, 0);
        for i in 0..512 {
            ram.write(DATA - DRAM_BASE + i, i & 0xff, 1).unwrap();
        }
        dev.write(0x050, 0, 4, &mut ram).unwrap();
        assert_eq!(dev.read_back(0, 4), [0, 1, 2, 3]);
        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 0);
    }

    #[test]
    fn read_request_supports_multiple_data_descriptors() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        let mut disk = vec![0; 1024];
        for (i, byte) in disk[512..].iter_mut().enumerate() {
            *byte = i as u8;
        }
        dev.set_disk(&disk);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        write_desc(&mut ram, 1, DATA, 256, DESC_F_WRITE | DESC_F_NEXT, 3);
        write_desc(&mut ram, 3, DATA_2, 256, DESC_F_WRITE | DESC_F_NEXT, 2);

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(DATA - DRAM_BASE + 255, 1).unwrap(), 255);
        assert_eq!(ram.read(DATA_2 - DRAM_BASE, 1).unwrap(), 0);
        assert_eq!(ram.read(DATA_2 - DRAM_BASE + 255, 1).unwrap(), 255);
        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 512);
        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 0);
    }

    #[test]
    fn write_request_supports_multiple_data_descriptors() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, BLK_T_OUT, 1, 0);
        write_desc(&mut ram, 1, DATA, 256, DESC_F_NEXT, 3);
        write_desc(&mut ram, 3, DATA_2, 256, DESC_F_NEXT, 2);
        for i in 0..256 {
            ram.write(DATA - DRAM_BASE + i, i, 1).unwrap();
            ram.write(DATA_2 - DRAM_BASE + i, 255 - i, 1).unwrap();
        }

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(dev.read_back(512, 4), [0, 1, 2, 3]);
        assert_eq!(dev.read_back(768, 4), [255, 254, 253, 252]);
        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 0);
    }

    struct FlushBackend {
        flushes: Arc<AtomicUsize>,
    }

    impl BlockBackend for FlushBackend {
        fn capacity_sectors(&self) -> u64 {
            2
        }

        fn read_at(&mut self, _sector: u64, _buf: &mut [u8]) -> Result<(), BlockError> {
            Ok(())
        }

        fn write_at(&mut self, _sector: u64, _buf: &[u8]) -> Result<(), BlockError> {
            Ok(())
        }

        fn flush(&mut self) -> Result<(), BlockError> {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn flush_request_reaches_backend() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let mut dev = VirtioBlock::new();
        dev.set_backend(Box::new(FlushBackend {
            flushes: Arc::clone(&flushes),
        }));
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, BLK_T_FLUSH, 0, 0);
        write_desc(&mut ram, 0, HEADER, 16, DESC_F_NEXT, 2);

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(flushes.load(Ordering::Relaxed), 1);
        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 0);
        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 0);
    }

    /// Without a visible `size_max` Linux builds requests up to its 1280 KiB
    /// `max_sectors` default and takes an unrecoverable EIO mid-write. The
    /// enforced bound, the published bound and the feature bit must agree.
    #[test]
    fn published_size_max_matches_the_request_bound() {
        let dev = VirtioBlock::new();

        // DeviceFeaturesSel = 0, then DeviceFeatures.
        assert_eq!(dev.read(0x014, 4), Ok(0));
        let features = dev.read(0x010, 4).unwrap();
        assert_ne!(
            features & (1 << BLK_F_SIZE_MAX),
            0,
            "VIRTIO_BLK_F_SIZE_MAX must be offered, or size_max is ignored"
        );
        assert_eq!(
            dev.read(BLK_CONFIG_SIZE_MAX, 4),
            Ok(MAX_REQUEST_BYTES as u64)
        );
        // Comfortably above Linux's BLK_DEF_MAX_SECTORS_CAP (1280 KiB), which
        // is what a request would otherwise grow to.
        const { assert!(MAX_REQUEST_BYTES >= 1024 * 1024) };
    }

    #[test]
    fn unknown_request_writes_ioerr_status() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, 99, 0, 0);
        ram.write(STATUS - DRAM_BASE, 0xaa, 1).unwrap();

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 1);
    }

    struct ErrorBackend;

    impl BlockBackend for ErrorBackend {
        fn capacity_sectors(&self) -> u64 {
            2
        }

        fn read_at(&mut self, _sector: u64, _buf: &mut [u8]) -> Result<(), BlockError> {
            Err(BlockError::Io)
        }

        fn write_at(&mut self, _sector: u64, _buf: &[u8]) -> Result<(), BlockError> {
            Err(BlockError::Io)
        }

        fn flush(&mut self) -> Result<(), BlockError> {
            Err(BlockError::Io)
        }
    }

    #[test]
    fn backend_error_writes_ioerr_status() {
        let mut dev = VirtioBlock::new();
        dev.set_backend(Box::new(ErrorBackend));
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, BLK_T_IN, 0, DESC_F_WRITE);

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 1);
        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 0);
    }

    #[test]
    fn bogus_avail_idx_is_rejected_without_replaying_the_ring() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        let mut disk = vec![0; 1024];
        disk[512..1024].fill(0xa5);
        dev.set_disk(&disk);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        // A driver may never have more than queue_num (8) chains outstanding.
        ram.write(AVAIL + 2 - DRAM_BASE, 40_000, 2).unwrap();

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        // Nothing published, no interrupt, and crucially the call returned.
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert!(!dev.irq_pending());

        // A subsequent notify with a sane index still works.
        ram.write(AVAIL + 2 - DRAM_BASE, 1, 2).unwrap();
        dev.write(0x050, 0, 4, &mut ram).unwrap();
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 0);
        assert!(dev.irq_pending());
    }

    #[test]
    fn full_queue_backlog_is_capped_at_queue_num_chains() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        // Eight identical chain heads, i.e. the largest legal backlog.
        for slot in 0..8u64 {
            ram.write(AVAIL + 4 - DRAM_BASE + slot * 2, 0, 2).unwrap();
        }
        ram.write(AVAIL + 2 - DRAM_BASE, 8, 2).unwrap();

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 8);
    }

    #[test]
    fn chain_without_writable_status_publishes_nothing() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        // Status descriptor is device-readable: there is nowhere to report a
        // result, so no used entry may be published.
        write_desc(&mut ram, 2, STATUS, 1, 0, 0);
        ram.write(STATUS - DRAM_BASE, 0xaa, 1).unwrap();

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert!(!dev.irq_pending());
        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 0xaa);

        // The queue did not wedge: the next chain is still processed.
        ram.write(AVAIL + 2 - DRAM_BASE, 2, 2).unwrap();
        ram.write(AVAIL + 6 - DRAM_BASE, 0, 2).unwrap();
        write_desc(&mut ram, 2, STATUS, 1, DESC_F_WRITE, 0);
        dev.write(0x050, 0, 4, &mut ram).unwrap();
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 0);
    }

    #[test]
    fn bad_data_address_still_writes_ioerr_status() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        // Data buffer points outside RAM; the status descriptor is fine.
        write_desc(
            &mut ram,
            1,
            DRAM_BASE + 0x8000_0000,
            512,
            DESC_F_WRITE | DESC_F_NEXT,
            2,
        );
        ram.write(STATUS - DRAM_BASE, 0xaa, 1).unwrap();

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 0);
        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 1);
        assert!(dev.irq_pending());
    }

    #[test]
    fn malformed_header_still_writes_ioerr_status() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        write_desc(&mut ram, 0, HEADER, 8, DESC_F_NEXT, 1);
        ram.write(STATUS - DRAM_BASE, 0xaa, 1).unwrap();

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 1);
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 0);
    }

    #[test]
    fn sole_writable_descriptor_gets_ioerr_status() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        write_desc(&mut ram, 0, STATUS, 1, DESC_F_WRITE, 0);
        ram.write(STATUS - DRAM_BASE, 0xaa, 1).unwrap();

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 1);
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 0);
    }

    #[test]
    fn out_of_ram_status_publishes_nothing() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, BLK_T_IN, 1, DESC_F_WRITE);
        let end_of_ram = DRAM_BASE + ram.size();
        write_desc(&mut ram, 2, end_of_ram, 1, DESC_F_WRITE, 0);

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 0);
        assert!(!dev.irq_pending());
    }

    #[test]
    fn oversized_request_writes_ioerr_status() {
        let mut dev = VirtioBlock::new();
        let mut ram = Ram::new(0x10000);
        dev.set_disk(&vec![0; 1024]);
        configure(&mut dev, &mut ram, BLK_T_IN, 0, DESC_F_WRITE);
        let oversized = (MAX_REQUEST_BYTES + 1) as u32;
        write_desc(&mut ram, 1, DATA, oversized, DESC_F_WRITE | DESC_F_NEXT, 2);
        ram.write(STATUS - DRAM_BASE, 0xaa, 1).unwrap();

        dev.write(0x050, 0, 4, &mut ram).unwrap();

        assert_eq!(ram.read(STATUS - DRAM_BASE, 1).unwrap(), 1);
        assert_eq!(ram.read(USED + 2 - DRAM_BASE, 2).unwrap(), 1);
        assert_eq!(ram.read(USED + 8 - DRAM_BASE, 4).unwrap(), 0);
        assert_eq!(dev.scratch.capacity(), 0);
    }
}
