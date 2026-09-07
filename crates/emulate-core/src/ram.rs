//! Guest DRAM: one contiguous buffer with little-endian accessors.
//! Callers pass offsets relative to DRAM base; bounds are checked here.

pub struct Ram {
    data: Vec<u8>,
    /// Monotonic write generation per 4-KiB page. The decoded instruction
    /// cache polls these to notice stores and DMA to code, so no invalidation
    /// callback is needed on every bus write.
    page_generations: Vec<u64>,
    /// Pages written at least once. A high-water mark, not live usage: a page
    /// is counted on first write and never uncounted, so a guest freeing
    /// memory does not lower it.
    touched_pages: u64,
}

const PAGE_SHIFT: u32 = 12;
const PAGE_SIZE: usize = 1 << PAGE_SHIFT;

impl Ram {
    pub fn new(size: usize) -> Self {
        Ram {
            data: vec![0; size],
            page_generations: vec![0; size.div_ceil(PAGE_SIZE)],
            touched_pages: 0,
        }
    }

    /// Bytes of guest RAM ever written, as a high-water mark; never decreases.
    #[inline]
    pub(crate) fn touched_bytes(&self) -> u64 {
        self.touched_pages * PAGE_SIZE as u64
    }

    #[inline]
    pub fn size(&self) -> u64 {
        self.data.len() as u64
    }

    /// Little-endian read of `size` bytes (1/2/4/8) at `offset`.
    #[inline]
    pub fn read(&self, offset: u64, size: u8) -> Result<u64, ()> {
        let n = size as usize;
        let bytes = self.read_slice(offset, n)?;
        let mut v: u64 = 0;
        for (i, b) in bytes.iter().enumerate() {
            v |= (*b as u64) << (i * 8);
        }
        Ok(v)
    }

    #[inline]
    pub fn write(&mut self, offset: u64, val: u64, size: u8) -> Result<(), ()> {
        let n = size as usize;
        let bytes = val.to_le_bytes();
        self.write_slice(offset, bytes.get(..n).ok_or(())?)
    }

    pub fn load_blob(&mut self, offset: u64, blob: &[u8]) -> Result<(), ()> {
        self.write_slice(offset, blob)
    }

    /// Return a checked, borrowed range of guest RAM.
    pub(crate) fn read_slice(&self, offset: u64, len: usize) -> Result<&[u8], ()> {
        let range = checked_range(offset, len)?;
        self.data.get(range).ok_or(())
    }

    /// Copy bytes into a checked range of guest RAM and invalidate each
    /// touched instruction-cache page once.
    pub(crate) fn write_slice(&mut self, offset: u64, data: &[u8]) -> Result<(), ()> {
        let range = checked_range(offset, data.len())?;
        let start = range.start;
        self.data.get_mut(range).ok_or(())?.copy_from_slice(data);
        self.bump_page_generations(start, data.len());
        Ok(())
    }

    /// Return `(page index, write generation)` for a valid RAM offset.
    #[inline]
    pub(crate) fn page_generation(&self, offset: u64) -> Option<(u64, u64)> {
        if offset >= self.size() {
            return None;
        }
        let page = offset >> PAGE_SHIFT;
        Some((page, self.page_generations[page as usize]))
    }

    /// Return the current generation of an already-resolved RAM page.
    #[inline]
    pub(crate) fn generation_for_page(&self, page: u64) -> Option<u64> {
        self.page_generations.get(page as usize).copied()
    }

    /// Write generations for `count` pages starting at page `first`, or an
    /// empty slice if that range is not wholly inside RAM.
    ///
    /// The public half of the per-page generations: the framebuffer's
    /// dirty-row scan compares this slice against its own copy, which is why
    /// one scanline is sized to one page (see `devices::framebuffer`).
    pub fn page_generations(&self, first: u64, count: usize) -> &[u64] {
        let Ok(start) = usize::try_from(first) else {
            return &[];
        };
        let Some(end) = start.checked_add(count) else {
            return &[];
        };
        self.page_generations.get(start..end).unwrap_or(&[])
    }

    fn bump_page_generations(&mut self, offset: usize, len: usize) {
        if len == 0 {
            return;
        }
        let first = offset >> PAGE_SHIFT;
        let last = (offset + len - 1) >> PAGE_SHIFT;
        let mut newly_touched = 0u64;
        for generation in &mut self.page_generations[first..=last] {
            if *generation == 0 {
                newly_touched += 1;
            }
            *generation = generation.wrapping_add(1);
        }
        self.touched_pages += newly_touched;
    }
}

/// State snapshots. Only pages that are not entirely zero are carried: after a
/// `drop_caches` the guest's live footprint is a small fraction of DRAM, and
/// zeros are exactly what a fresh `Ram` already holds.
impl Ram {
    /// How many 4 KiB pages [`Ram::snapshot`] will write.
    pub(crate) fn nonzero_page_count(&self) -> u64 {
        self.data
            .chunks(PAGE_SIZE)
            .filter(|page| page.iter().any(|&b| b != 0))
            .count() as u64
    }

    pub(crate) fn snapshot(&self, out: &mut crate::snapshot::Writer) {
        out.section(RAM_SECTION_VERSION, |w| {
            w.u64(self.data.len() as u64);
            w.u64(self.nonzero_page_count());
            for (index, page) in self.data.chunks(PAGE_SIZE).enumerate() {
                if page.iter().any(|&b| b != 0) {
                    w.u64(index as u64);
                    w.raw(page);
                    // A final short page is written short; its length is
                    // implied by the RAM size, which the reader already has.
                }
            }
        });
    }

    pub(crate) fn restore(
        &mut self,
        input: &mut crate::snapshot::Reader<'_>,
    ) -> Result<(), String> {
        let pages: Vec<(u64, Vec<u8>)> = input.section("ram", RAM_SECTION_VERSION, |r| {
            let size = r.u64()?;
            if size != self.data.len() as u64 {
                return Err(format!(
                    "RAM size {size} does not match this machine's {}",
                    self.data.len()
                ));
            }
            let count = r.u64()?;
            let page_count = self.data.len().div_ceil(PAGE_SIZE) as u64;
            if count > page_count {
                return Err(format!(
                    "{count} pages is more than the {page_count} in RAM"
                ));
            }
            let mut pages = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let index = r.u64()?;
                if index >= page_count {
                    return Err(format!("page index {index} is outside RAM"));
                }
                let start = index as usize * PAGE_SIZE;
                let len = PAGE_SIZE.min(self.data.len() - start);
                pages.push((index, r.raw(len)?.to_vec()));
            }
            Ok(pages)
        })?;

        self.data.fill(0);
        self.page_generations.fill(0);
        self.touched_pages = 0;
        for (index, bytes) in pages {
            let start = index as usize * PAGE_SIZE;
            self.data[start..start + bytes.len()].copy_from_slice(&bytes);
            // A restored page counts as written once, which keeps
            // `touched_bytes` a meaningful high-water mark across a restore.
            self.page_generations[index as usize] = 1;
            self.touched_pages += 1;
        }
        Ok(())
    }
}

const RAM_SECTION_VERSION: u16 = 1;

fn checked_range(offset: u64, len: usize) -> Result<std::ops::Range<usize>, ()> {
    let start = usize::try_from(offset).map_err(|_| ())?;
    let end = start.checked_add(len).ok_or(())?;
    Ok(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_bump_only_touched_page_generations() {
        let mut ram = Ram::new(PAGE_SIZE * 3);
        assert_eq!(ram.page_generation(0), Some((0, 0)));
        assert_eq!(ram.page_generation(PAGE_SIZE as u64), Some((1, 0)));

        ram.write(PAGE_SIZE as u64 - 1, 0x2211, 2).unwrap();
        assert_eq!(ram.generation_for_page(0), Some(1));
        assert_eq!(ram.generation_for_page(1), Some(1));
        assert_eq!(ram.generation_for_page(2), Some(0));

        ram.load_blob(PAGE_SIZE as u64 * 2, &[1, 2, 3]).unwrap();
        assert_eq!(ram.generation_for_page(2), Some(1));
    }

    #[test]
    fn page_generation_ranges_are_bounds_checked() {
        let mut ram = Ram::new(PAGE_SIZE * 3);
        assert_eq!(ram.page_generations(0, 3), &[0, 0, 0]);

        ram.write(PAGE_SIZE as u64, 1, 8).unwrap();
        assert_eq!(ram.page_generations(0, 3), &[0, 1, 0]);
        assert_eq!(ram.page_generations(1, 1), &[1]);

        // Ranges that leave RAM, or overflow, yield nothing rather than a
        // short slice a caller might mistake for the whole framebuffer.
        assert!(ram.page_generations(2, 2).is_empty());
        assert!(ram.page_generations(u64::MAX, 1).is_empty());
        assert!(ram.page_generations(0, usize::MAX).is_empty());
    }

    #[test]
    fn touched_bytes_is_a_high_water_mark_over_written_pages() {
        let mut ram = Ram::new(4 * PAGE_SIZE);
        assert_eq!(ram.touched_bytes(), 0);
        ram.write(0, 1, 8).unwrap();
        assert_eq!(ram.touched_bytes(), PAGE_SIZE as u64);
        // A second write to the same page must not count it twice.
        ram.write(8, 1, 8).unwrap();
        assert_eq!(ram.touched_bytes(), PAGE_SIZE as u64);
        // A slice spanning two fresh pages counts both, once each.
        ram.write_slice(PAGE_SIZE as u64 - 4, &[0xff; 8]).unwrap();
        assert_eq!(ram.touched_bytes(), 2 * PAGE_SIZE as u64);
    }

    #[test]
    fn empty_blob_does_not_bump_a_generation() {
        let mut ram = Ram::new(PAGE_SIZE);
        ram.load_blob(0, &[]).unwrap();
        assert_eq!(ram.generation_for_page(0), Some(0));
    }

    #[test]
    fn slice_access_rejects_wrapping_and_out_of_range_ranges() {
        let mut ram = Ram::new(8);

        assert!(ram.read_slice(u64::MAX, 2).is_err());
        assert!(ram.write_slice(7, &[1, 2]).is_err());
        assert_eq!(ram.read_slice(0, 8).unwrap(), &[0; 8]);
    }

    #[test]
    fn rejected_slice_write_is_atomic() {
        let mut ram = Ram::new(8);

        assert!(ram.write_slice(7, &[0xaa, 0xbb]).is_err());

        assert_eq!(ram.read_slice(0, 8).unwrap(), &[0; 8]);
        assert_eq!(ram.generation_for_page(0), Some(0));
    }

    #[test]
    fn slice_write_bumps_each_touched_page_once() {
        let mut ram = Ram::new(PAGE_SIZE * 3);

        ram.write_slice(PAGE_SIZE as u64 - 2, &[0xaa; 8]).unwrap();

        assert_eq!(ram.generation_for_page(0), Some(1));
        assert_eq!(ram.generation_for_page(1), Some(1));
        assert_eq!(ram.generation_for_page(2), Some(0));
    }
}
