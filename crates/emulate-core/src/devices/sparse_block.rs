//! An in-memory sparse block device, plus the CRC used to check disk records.
//!
//! Only 4 KiB blocks holding non-zero bytes are stored; everything else reads
//! as zero, so a 512 MiB logical disk costs only what its live data costs.

use std::collections::HashMap;

use super::virtio_blk::{BlockBackend, BlockError};

/// Sparse allocation granularity, and the record payload size of the browser's
/// persistent log (`crates/emulate-wasm/src/disk.rs`).
pub const SPARSE_BLOCK_SIZE: usize = 4096;

/// Reflected CRC-32 (IEEE 802.3, polynomial 0xedb8_8320) byte table, built at
/// compile time: one lookup per byte instead of eight shift/xor rounds.
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
};

/// Reflected CRC-32 (IEEE 802.3) over `bytes`, as used by the browser's
/// persistent-log record checksums. `crc32(b"123456789") == 0xcbf4_3926`.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc = (crc >> 8) ^ CRC32_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize];
    }
    !crc
}

pub struct SparseMemBackend {
    logical_bytes: u64,
    blocks: HashMap<u32, Box<[u8; SPARSE_BLOCK_SIZE]>>,
}

impl SparseMemBackend {
    /// An all-zero disk of `logical_bytes`, which must be a non-zero multiple
    /// of [`SPARSE_BLOCK_SIZE`] addressable by a `u32` block index.
    pub fn new(logical_bytes: u64) -> Option<Self> {
        let block_count = logical_bytes / SPARSE_BLOCK_SIZE as u64;
        if logical_bytes == 0
            || !logical_bytes.is_multiple_of(SPARSE_BLOCK_SIZE as u64)
            || block_count > u32::MAX as u64
        {
            return None;
        }
        Some(Self {
            logical_bytes,
            blocks: HashMap::new(),
        })
    }

    /// Place one 4 KiB block. An all-zero block clears whatever was there, so
    /// a seeder can hand over every block it reads without growing the map.
    pub fn insert_block(&mut self, block_index: u32, block: &[u8; SPARSE_BLOCK_SIZE]) -> bool {
        if u64::from(block_index) >= self.logical_bytes / SPARSE_BLOCK_SIZE as u64 {
            return false;
        }
        if block.iter().all(|byte| *byte == 0) {
            self.blocks.remove(&block_index);
        } else {
            self.blocks.insert(block_index, Box::new(*block));
        }
        true
    }

    /// How many blocks are actually stored (zero blocks cost nothing).
    pub fn present_blocks(&self) -> usize {
        self.blocks.len()
    }

    fn checked_byte_range(&self, sector: u64, len: usize) -> Result<(usize, usize), BlockError> {
        let start = sector.checked_mul(512).ok_or(BlockError::OutOfRange)?;
        let end = start
            .checked_add(len as u64)
            .ok_or(BlockError::OutOfRange)?;
        if end > self.logical_bytes {
            return Err(BlockError::OutOfRange);
        }
        Ok((start as usize, end as usize))
    }
}

impl BlockBackend for SparseMemBackend {
    fn capacity_sectors(&self) -> u64 {
        self.logical_bytes / 512
    }

    fn read_at(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let (start, _) = self.checked_byte_range(sector, buf.len())?;
        buf.fill(0);
        let mut consumed = 0;
        while consumed < buf.len() {
            let byte_offset = start + consumed;
            let block_index = byte_offset / SPARSE_BLOCK_SIZE;
            let within_block = byte_offset % SPARSE_BLOCK_SIZE;
            let copy_len = (buf.len() - consumed).min(SPARSE_BLOCK_SIZE - within_block);
            if let Some(block) = self.blocks.get(&(block_index as u32)) {
                buf[consumed..consumed + copy_len]
                    .copy_from_slice(&block[within_block..within_block + copy_len]);
            }
            consumed += copy_len;
        }
        Ok(())
    }

    fn write_at(&mut self, sector: u64, buf: &[u8]) -> Result<(), BlockError> {
        let (start, _) = self.checked_byte_range(sector, buf.len())?;
        let mut consumed = 0;
        while consumed < buf.len() {
            let byte_offset = start + consumed;
            let block_index = (byte_offset / SPARSE_BLOCK_SIZE) as u32;
            let within_block = byte_offset % SPARSE_BLOCK_SIZE;
            let copy_len = (buf.len() - consumed).min(SPARSE_BLOCK_SIZE - within_block);
            let block = self
                .blocks
                .entry(block_index)
                .or_insert_with(|| Box::new([0; SPARSE_BLOCK_SIZE]));
            block[within_block..within_block + copy_len]
                .copy_from_slice(&buf[consumed..consumed + copy_len]);
            if block.iter().all(|byte| *byte == 0) {
                self.blocks.remove(&block_index);
            }
            consumed += copy_len;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_ieee_check_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(&[0u8; 4096]), 0xc71c_0011);
    }

    #[test]
    fn sparse_backend_rejects_unusable_geometry() {
        assert!(SparseMemBackend::new(0).is_none());
        assert!(SparseMemBackend::new(4095).is_none());
        assert!(SparseMemBackend::new(512 * 1024 * 1024).is_some());
    }

    #[test]
    fn sparse_backend_reports_logical_capacity() {
        let backend = SparseMemBackend::new(512 * 1024 * 1024).unwrap();
        assert_eq!(backend.capacity_sectors(), 1024 * 1024);
    }

    #[test]
    fn sparse_backend_reads_present_and_absent_blocks() {
        let mut backend = SparseMemBackend::new(512 * 1024 * 1024).unwrap();
        let mut block = [0; SPARSE_BLOCK_SIZE];
        block[0] = 0x5a;
        assert!(backend.insert_block(3, &block));

        let mut bytes = [0xff; SPARSE_BLOCK_SIZE * 2];
        backend.read_at(16, &mut bytes).unwrap();
        assert_eq!((bytes[4096], bytes[0]), (0x5a, 0));
    }

    #[test]
    fn sparse_backend_stores_nothing_for_zero_blocks() {
        let mut backend = SparseMemBackend::new(512 * 1024 * 1024).unwrap();
        assert!(backend.insert_block(0, &[0; SPARSE_BLOCK_SIZE]));
        assert_eq!(backend.present_blocks(), 0);
        assert!(!backend.insert_block(1024 * 128, &[1; SPARSE_BLOCK_SIZE]));
    }

    #[test]
    fn sparse_backend_writes_across_blocks() {
        let mut backend = SparseMemBackend::new(512 * 1024 * 1024).unwrap();
        let value = [0xa5; 1024];
        backend.write_at(7, &value).unwrap();
        let mut actual = [0; 1024];
        backend.read_at(7, &mut actual).unwrap();
        assert_eq!(actual, value);
    }
}
