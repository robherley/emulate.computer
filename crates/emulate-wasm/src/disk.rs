//! Ephemeral OPFS block storage and streaming disk seeding.

use std::cell::RefCell;
use std::rc::Rc;

use emulate_core::devices::sparse_block::{SparseMemBackend, SPARSE_BLOCK_SIZE};
use emulate_core::devices::virtio_blk::{BlockBackend, BlockError};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{FileSystemReadWriteOptions, FileSystemSyncAccessHandle};

pub(crate) const SEED_BLOCK_SIZE: usize = SPARSE_BLOCK_SIZE;
const SEED_BATCH_BYTES: usize = 1024 * 1024;
const MAX_SAFE_INTEGER: u64 = 1u64 << 53;

pub(crate) struct SessionDiskStatus {
    logical_bytes: u64,
    error: RefCell<Option<String>>,
}

impl SessionDiskStatus {
    fn new(logical_bytes: u64) -> Self {
        Self {
            logical_bytes,
            error: RefCell::new(None),
        }
    }

    pub(crate) fn line(&self) -> String {
        format!(
            "ephemeral ({} MiB image)",
            self.logical_bytes / (1024 * 1024)
        )
    }

    fn record(&self, operation: &str) {
        let detail = take_image_io_failure().unwrap_or_else(|| "I/O failed".to_owned());
        self.error
            .replace(Some(format!("session disk {operation}: {detail}")));
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        self.error.borrow_mut().take()
    }
}

pub(crate) trait ImageIo {
    fn size_bytes(&self) -> Result<u64, BlockError>;
    fn read_at_exact(&self, offset: u64, buf: &mut [u8]) -> Result<(), BlockError>;
    fn write_at_all(&self, offset: u64, buf: &[u8]) -> Result<(), BlockError>;
    fn truncate_to(&self, size: u64) -> Result<(), BlockError>;
    fn sync(&self) -> Result<(), BlockError>;
    fn release(&self) {}
}

thread_local! {
    static LAST_IMAGE_IO_FAILURE: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn image_io_failure(detail: String) -> BlockError {
    #[cfg(target_arch = "wasm32")]
    web_sys::console::error_1(&JsValue::from_str(&format!("opfs: {detail}")));
    LAST_IMAGE_IO_FAILURE.with(|slot| slot.replace(Some(detail)));
    BlockError::Io
}

fn take_image_io_failure() -> Option<String> {
    LAST_IMAGE_IO_FAILURE.with(|slot| slot.borrow_mut().take())
}

fn describe_js_error(error: &JsValue) -> String {
    if let Some(exception) = error.dyn_ref::<web_sys::DomException>() {
        return format!("{}: {}", exception.name(), exception.message());
    }
    error
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(error, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| format!("{error:?}"))
}

fn offset_options(offset: u64) -> Result<FileSystemReadWriteOptions, BlockError> {
    if offset > MAX_SAFE_INTEGER {
        return Err(BlockError::OutOfRange);
    }
    let options = FileSystemReadWriteOptions::new();
    options.set_at(offset as f64);
    Ok(options)
}

fn exact_transfer(
    operation: &str,
    offset: u64,
    count: f64,
    requested: usize,
) -> Result<(), BlockError> {
    if count == requested as f64 {
        return Ok(());
    }
    Err(image_io_failure(format!(
        "{operation} at {offset} transferred {count} of {requested} bytes"
    )))
}

impl ImageIo for FileSystemSyncAccessHandle {
    fn size_bytes(&self) -> Result<u64, BlockError> {
        let size = self
            .get_size()
            .map_err(|error| image_io_failure(format!("getSize: {}", describe_js_error(&error))))?;
        if !size.is_finite()
            || size.fract() != 0.0
            || !(0.0..=MAX_SAFE_INTEGER as f64).contains(&size)
        {
            return Err(image_io_failure(format!("getSize returned {size}")));
        }
        Ok(size as u64)
    }

    fn read_at_exact(&self, offset: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let options = offset_options(offset)?;
        let count = self
            .read_with_u8_array_and_options(buf, &options)
            .map_err(|error| {
                image_io_failure(format!("read at {offset}: {}", describe_js_error(&error)))
            })?;
        exact_transfer("read", offset, count, buf.len())
    }

    fn write_at_all(&self, offset: u64, buf: &[u8]) -> Result<(), BlockError> {
        let options = offset_options(offset)?;
        let count = self
            .write_with_u8_array_and_options(buf, &options)
            .map_err(|error| {
                image_io_failure(format!("write at {offset}: {}", describe_js_error(&error)))
            })?;
        exact_transfer("write", offset, count, buf.len())
    }

    fn truncate_to(&self, size: u64) -> Result<(), BlockError> {
        if size > MAX_SAFE_INTEGER {
            return Err(BlockError::OutOfRange);
        }
        self.truncate_with_f64(size as f64).map_err(|error| {
            image_io_failure(format!("truncate to {size}: {}", describe_js_error(&error)))
        })
    }

    fn sync(&self) -> Result<(), BlockError> {
        self.flush()
            .map_err(|error| image_io_failure(format!("flush: {}", describe_js_error(&error))))
    }

    fn release(&self) {
        FileSystemSyncAccessHandle::close(self);
    }
}

fn check_geometry(logical_bytes: u64) -> Result<(), String> {
    if logical_bytes == 0 || !logical_bytes.is_multiple_of(SEED_BLOCK_SIZE as u64) {
        return Err(format!(
            "{logical_bytes} is not a non-zero multiple of {SEED_BLOCK_SIZE}"
        ));
    }
    Ok(())
}

pub(crate) struct OpfsBackend<I: ImageIo = FileSystemSyncAccessHandle> {
    io: I,
    logical_bytes: u64,
    status: Rc<SessionDiskStatus>,
}

impl<I: ImageIo> OpfsBackend<I> {
    pub(crate) fn open(io: I, logical_bytes: u64) -> Result<(Self, Rc<SessionDiskStatus>), String> {
        check_geometry(logical_bytes).map_err(|why| format!("OPFS disk: {why}"))?;
        let size = io
            .size_bytes()
            .map_err(|_| "OPFS disk: cannot read image size".to_owned())?;
        if size != logical_bytes {
            return Err(format!(
                "OPFS disk is {size} bytes, expected {logical_bytes}"
            ));
        }
        let status = Rc::new(SessionDiskStatus::new(logical_bytes));
        Ok((
            Self {
                io,
                logical_bytes,
                status: Rc::clone(&status),
            },
            status,
        ))
    }

    fn offset(&self, sector: u64, len: usize) -> Result<u64, BlockError> {
        let start = sector.checked_mul(512).ok_or(BlockError::OutOfRange)?;
        let end = start
            .checked_add(len as u64)
            .ok_or(BlockError::OutOfRange)?;
        (end <= self.logical_bytes)
            .then_some(start)
            .ok_or(BlockError::OutOfRange)
    }
}

impl<I: ImageIo> BlockBackend for OpfsBackend<I> {
    fn capacity_sectors(&self) -> u64 {
        self.logical_bytes / 512
    }

    fn read_at(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let offset = self.offset(sector, buf.len()).inspect_err(|_| {
            self.status.record("read outside image");
        })?;
        self.io.read_at_exact(offset, buf).inspect_err(|_| {
            self.status.record("read failed");
        })
    }

    fn write_at(&mut self, sector: u64, buf: &[u8]) -> Result<(), BlockError> {
        let offset = self.offset(sector, buf.len()).inspect_err(|_| {
            self.status.record("write outside image");
        })?;
        self.io.write_at_all(offset, buf).inspect_err(|_| {
            self.status.record("write failed");
        })
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.io.sync().inspect_err(|_| {
            self.status.record("flush failed");
        })
    }
}

impl<I: ImageIo> Drop for OpfsBackend<I> {
    fn drop(&mut self) {
        let _ = self.io.sync();
        self.io.release();
    }
}

enum SeedSink<I: ImageIo> {
    Image {
        io: I,
        batch: Vec<u8>,
        batch_start: u64,
    },
    Memory(SparseMemBackend),
}

pub(crate) struct DiskSeeder<I: ImageIo> {
    sink: SeedSink<I>,
    logical_bytes: u64,
    consumed: u64,
    pending: Vec<u8>,
    next_block: u32,
}

fn seed_geometry(logical_bytes: u64) -> Result<(), String> {
    check_geometry(logical_bytes).map_err(|why| format!("seed: {why}"))?;
    if logical_bytes / SEED_BLOCK_SIZE as u64 > u32::MAX as u64 {
        return Err(format!("seed: {logical_bytes} is too large"));
    }
    Ok(())
}

impl<I: ImageIo> DiskSeeder<I> {
    pub(crate) fn begin_image(io: I, logical_bytes: u64) -> Result<Self, String> {
        seed_geometry(logical_bytes)?;
        io.truncate_to(0)
            .and_then(|()| io.truncate_to(logical_bytes))
            .map_err(|_| "seed: cannot size image".to_owned())?;
        Ok(Self {
            sink: SeedSink::Image {
                io,
                batch: Vec::with_capacity(SEED_BATCH_BYTES),
                batch_start: 0,
            },
            logical_bytes,
            consumed: 0,
            pending: Vec::with_capacity(SEED_BLOCK_SIZE),
            next_block: 0,
        })
    }

    pub(crate) fn begin_memory(logical_bytes: u64) -> Result<Self, String> {
        seed_geometry(logical_bytes)?;
        let backend = SparseMemBackend::new(logical_bytes)
            .ok_or_else(|| "seed: unusable in-memory disk size".to_owned())?;
        Ok(Self {
            sink: SeedSink::Memory(backend),
            logical_bytes,
            consumed: 0,
            pending: Vec::with_capacity(SEED_BLOCK_SIZE),
            next_block: 0,
        })
    }

    pub(crate) fn write(&mut self, chunk: &[u8]) -> Result<(), String> {
        self.consumed = self
            .consumed
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| "seed: image size overflowed".to_owned())?;
        if self.consumed > self.logical_bytes {
            return Err(format!("seed: image exceeds {} bytes", self.logical_bytes));
        }
        let mut rest = chunk;
        while !rest.is_empty() {
            if self.pending.is_empty() && rest.len() >= SEED_BLOCK_SIZE {
                let (block, tail) = rest.split_at(SEED_BLOCK_SIZE);
                self.emit(block)?;
                rest = tail;
                continue;
            }
            let take = (SEED_BLOCK_SIZE - self.pending.len()).min(rest.len());
            self.pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending.len() == SEED_BLOCK_SIZE {
                let block = std::mem::take(&mut self.pending);
                self.emit(&block)?;
                self.pending = block;
                self.pending.clear();
            }
        }
        Ok(())
    }

    fn emit(&mut self, block: &[u8]) -> Result<(), String> {
        let block_index = self.next_block;
        self.next_block += 1;
        if block.iter().all(|byte| *byte == 0) {
            return Ok(());
        }
        match &mut self.sink {
            SeedSink::Memory(backend) => {
                let block = block
                    .try_into()
                    .map_err(|_| "seed: short block".to_owned())?;
                backend.insert_block(block_index, block);
            }
            SeedSink::Image {
                io,
                batch,
                batch_start,
            } => {
                let offset = u64::from(block_index) * SEED_BLOCK_SIZE as u64;
                if !batch.is_empty() && *batch_start + batch.len() as u64 != offset {
                    Self::flush_batch(io, batch, *batch_start)?;
                }
                if batch.is_empty() {
                    *batch_start = offset;
                }
                batch.extend_from_slice(block);
                if batch.len() >= SEED_BATCH_BYTES {
                    Self::flush_batch(io, batch, *batch_start)?;
                }
            }
        }
        Ok(())
    }

    fn flush_batch(io: &I, batch: &mut Vec<u8>, offset: u64) -> Result<(), String> {
        if batch.is_empty() {
            return Ok(());
        }
        io.write_at_all(offset, batch)
            .map_err(|_| format!("seed: write failed at byte {offset}"))?;
        batch.clear();
        Ok(())
    }

    pub(crate) fn abort(self) {
        if let SeedSink::Image { io, .. } = self.sink {
            io.release();
        }
    }

    pub(crate) fn finish(self) -> Result<Option<SparseMemBackend>, String> {
        if self.consumed != self.logical_bytes {
            return Err(format!(
                "seed: received {} of {} bytes",
                self.consumed, self.logical_bytes
            ));
        }
        match self.sink {
            SeedSink::Memory(backend) => Ok(Some(backend)),
            SeedSink::Image {
                io,
                mut batch,
                batch_start,
            } => {
                Self::flush_batch(&io, &mut batch, batch_start)?;
                io.sync().map_err(|_| "seed: flush failed".to_owned())?;
                io.release();
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemImage {
        bytes: RefCell<Vec<u8>>,
        writes: RefCell<Vec<(u64, usize)>>,
    }

    impl MemImage {
        fn sized(size: usize) -> Self {
            Self {
                bytes: RefCell::new(vec![0; size]),
                writes: RefCell::new(Vec::new()),
            }
        }
    }

    impl ImageIo for Rc<MemImage> {
        fn size_bytes(&self) -> Result<u64, BlockError> {
            Ok(self.bytes.borrow().len() as u64)
        }

        fn read_at_exact(&self, offset: u64, buf: &mut [u8]) -> Result<(), BlockError> {
            let bytes = self.bytes.borrow();
            let start = offset as usize;
            let end = start.checked_add(buf.len()).ok_or(BlockError::OutOfRange)?;
            buf.copy_from_slice(bytes.get(start..end).ok_or(BlockError::OutOfRange)?);
            Ok(())
        }

        fn write_at_all(&self, offset: u64, buf: &[u8]) -> Result<(), BlockError> {
            let start = offset as usize;
            let end = start.checked_add(buf.len()).ok_or(BlockError::OutOfRange)?;
            let mut bytes = self.bytes.borrow_mut();
            if end > bytes.len() {
                bytes.resize(end, 0);
            }
            bytes[start..end].copy_from_slice(buf);
            self.writes.borrow_mut().push((offset, buf.len()));
            Ok(())
        }

        fn truncate_to(&self, size: u64) -> Result<(), BlockError> {
            self.bytes.borrow_mut().resize(size as usize, 0);
            Ok(())
        }

        fn sync(&self) -> Result<(), BlockError> {
            Ok(())
        }
    }

    const DISK_BYTES: usize = 8 * SEED_BLOCK_SIZE;

    #[test]
    fn backend_maps_sectors_to_image_offsets() {
        let image = Rc::new(MemImage::sized(DISK_BYTES));
        let (mut backend, _) = OpfsBackend::open(Rc::clone(&image), DISK_BYTES as u64).unwrap();
        backend.write_at(3, &[0x5a; 512]).unwrap();
        assert_eq!(image.bytes.borrow()[3 * 512], 0x5a);
    }

    #[test]
    fn backend_rejects_wrong_image_size() {
        let image = Rc::new(MemImage::sized(DISK_BYTES - 1));
        assert!(OpfsBackend::open(image, DISK_BYTES as u64).is_err());
    }

    #[test]
    fn image_seeder_skips_zero_blocks() {
        let image = Rc::new(MemImage::default());
        let mut seeder = DiskSeeder::begin_image(Rc::clone(&image), DISK_BYTES as u64).unwrap();
        let mut contents = vec![0; DISK_BYTES];
        contents[SEED_BLOCK_SIZE] = 1;
        seeder.write(&contents[..123]).unwrap();
        seeder.write(&contents[123..]).unwrap();
        seeder.finish().unwrap();
        assert_eq!(
            &*image.writes.borrow(),
            &[(SEED_BLOCK_SIZE as u64, SEED_BLOCK_SIZE)]
        );
    }

    #[test]
    fn memory_seeder_keeps_only_nonzero_blocks() {
        let mut seeder = DiskSeeder::<Rc<MemImage>>::begin_memory(DISK_BYTES as u64).unwrap();
        let mut contents = vec![0; DISK_BYTES];
        contents[2 * SEED_BLOCK_SIZE] = 1;
        seeder.write(&contents).unwrap();
        let backend = seeder.finish().unwrap().unwrap();
        assert_eq!(backend.present_blocks(), 1);
    }

    #[test]
    fn seeder_rejects_truncated_input() {
        let mut seeder = DiskSeeder::<Rc<MemImage>>::begin_memory(DISK_BYTES as u64).unwrap();
        seeder.write(&[0; SEED_BLOCK_SIZE]).unwrap();
        assert!(seeder.finish().is_err());
    }
}
