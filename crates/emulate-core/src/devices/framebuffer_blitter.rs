//! Synchronous rectangle copy/fill for Xfbdev; register layout in docs/display.md.

use super::framebuffer::{
    FB_BYTES, FB_FIRST_PAGE, FB_HEIGHT, FB_MAX_HEIGHT, FB_MAX_WIDTH, FB_PAGES_PER_ROW,
    FB_RAM_OFFSET, FB_STRIDE, FB_WIDTH,
};
use crate::ram::Ram;

const ID: u32 = 0x4546_4203;
const STRIDE: usize = FB_STRIDE as usize;

pub struct FramebufferBlitter {
    // Status, source, destination, width in bytes, height, AND/plane mask, XOR.
    registers: [u32; 7],
    front: Vec<u8>,
    generations: Vec<u64>,
    pub presents: u64,
    requested: (u32, u32),
    active: (u32, u32),
    pending: (u32, u32),
    geometry_changed: bool,
}

impl Default for FramebufferBlitter {
    fn default() -> Self {
        Self {
            registers: [0; 7],
            front: Vec::new(),
            generations: Vec::new(),
            presents: 0,
            requested: (FB_WIDTH, FB_HEIGHT),
            active: (FB_WIDTH, FB_HEIGHT),
            pending: (FB_WIDTH, FB_HEIGHT),
            geometry_changed: false,
        }
    }
}

impl FramebufferBlitter {
    pub fn read(&self, offset: u64, size: u8) -> Result<u64, ()> {
        if size != 4 || offset > 40 || !offset.is_multiple_of(4) {
            return Err(());
        }
        if offset == 0 {
            return Ok(ID as u64);
        }
        if offset == 32 {
            return Ok(self.requested.0 as u64);
        }
        if offset == 36 {
            return Ok(self.requested.1 as u64);
        }
        if offset == 40 {
            return Ok(((self.active.1 << 16) | self.active.0) as u64);
        }
        self.registers
            .get((offset / 4 - 1) as usize)
            .map(|&value| value as u64)
            .ok_or(())
    }

    pub fn write(&mut self, offset: u64, value: u64, size: u8, ram: &mut Ram) -> Result<(), ()> {
        if size != 4 || offset == 0 || offset > 40 || !offset.is_multiple_of(4) {
            return Err(());
        }
        if offset == 32 || offset == 36 {
            return Err(());
        }
        if offset == 40 {
            let size = (value as u32 & 0xffff, (value as u32 >> 16) & 0xffff);
            if !valid_size(size) {
                return Err(());
            }
            self.pending = size;
            return Ok(());
        }
        let register = self
            .registers
            .get_mut((offset / 4 - 1) as usize)
            .ok_or(())?;
        *register = value as u32;
        if offset == 4 {
            self.registers[0] = u32::from(self.execute(value as u32, ram).is_err());
        }
        Ok(())
    }

    pub fn request_size(&mut self, width: u32, height: u32) -> Result<(), ()> {
        if !valid_size((width, height)) {
            return Err(());
        }
        self.requested = (width, height);
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        self.active
    }

    pub fn take_geometry_changed(&mut self) -> bool {
        std::mem::take(&mut self.geometry_changed)
    }

    pub fn committed(&self) -> bool {
        !self.front.is_empty()
    }

    pub fn generations(&self) -> &[u64] {
        &self.generations
    }

    pub fn row(&self, row: u32) -> &[u8] {
        self.front
            .get(row as usize * STRIDE..(row as usize + 1) * STRIDE)
            .unwrap_or(&[])
    }

    fn present(&mut self, ram: &Ram) -> Result<(), ()> {
        let pixels = ram.read_slice(FB_RAM_OFFSET, FB_BYTES as usize)?;
        let pages = ram.page_generations(FB_FIRST_PAGE, FB_MAX_HEIGHT as usize * FB_PAGES_PER_ROW);
        let generations: Vec<u64> = pages
            .chunks_exact(FB_PAGES_PER_ROW)
            .map(|pages| pages.iter().fold(0u64, |sum, n| sum.wrapping_add(*n)))
            .collect();
        if self.front.is_empty() {
            self.front = pixels.to_vec();
            self.generations = generations.to_vec();
        } else {
            for (row, (&generation, seen)) in
                generations.iter().zip(&mut self.generations).enumerate()
            {
                if generation != *seen {
                    let start = row * STRIDE;
                    self.front[start..start + STRIDE]
                        .copy_from_slice(&pixels[start..start + STRIDE]);
                    *seen = generation;
                }
            }
        }
        self.geometry_changed |= self.active != self.pending;
        self.active = self.pending;
        self.presents = self.presents.wrapping_add(1);
        Ok(())
    }

    fn execute(&mut self, command: u32, ram: &mut Ram) -> Result<(), ()> {
        if command == 3 {
            return self.present(ram);
        }
        if command == 4 {
            self.front.clear();
            self.generations.clear();
            return Ok(());
        }
        let [_, source, destination, width, height, and, xor] = self.registers;
        let (source, destination, width, height) = (
            source as usize,
            destination as usize,
            width as usize,
            height as usize,
        );
        if !matches!(command, 1 | 2)
            || ram.size() < FB_RAM_OFFSET + FB_BYTES
            || !valid_rectangle(destination, width, height)
            || (command == 1 && !valid_rectangle(source, width, height))
        {
            return Err(());
        }

        let mut row = [0u8; STRIDE];
        if command == 2 && and == 0 {
            for pixel in row[..width].chunks_exact_mut(4) {
                pixel.copy_from_slice(&xor.to_le_bytes());
            }
        }
        for index in 0..height {
            let y = if command == 1 && destination > source {
                height - 1 - index
            } else {
                index
            };
            let dst = FB_RAM_OFFSET + (destination + y * STRIDE) as u64;
            if command == 1 {
                let src = FB_RAM_OFFSET + (source + y * STRIDE) as u64;
                row[..width].copy_from_slice(ram.read_slice(src, width)?);
                if and != u32::MAX {
                    for (pixel, previous) in row[..width]
                        .chunks_exact_mut(4)
                        .zip(ram.read_slice(dst, width)?.chunks_exact(4))
                    {
                        for (byte, (&old, mask)) in
                            pixel.iter_mut().zip(previous.iter().zip(and.to_le_bytes()))
                        {
                            *byte = (*byte & mask) | (old & !mask);
                        }
                    }
                }
            } else if and != 0 {
                row[..width].copy_from_slice(ram.read_slice(dst, width)?);
                for pixel in row[..width].chunks_exact_mut(4) {
                    for (byte, (mask, value)) in pixel
                        .iter_mut()
                        .zip(and.to_le_bytes().into_iter().zip(xor.to_le_bytes()))
                    {
                        *byte = (*byte & mask) ^ value;
                    }
                }
            }
            ram.write_slice(dst, &row[..width])?;
        }
        Ok(())
    }

    pub(crate) fn snapshot(&self, out: &mut crate::snapshot::Writer) {
        out.section(3, |w| {
            for value in self.registers {
                w.u32(value);
            }
            w.bytes(&self.front);
            w.u64(self.presents);
            w.u32(self.active.0);
            w.u32(self.active.1);
            w.u32(self.pending.0);
            w.u32(self.pending.1);
        });
    }

    pub(crate) fn restore(
        &mut self,
        input: &mut crate::snapshot::Reader<'_>,
    ) -> Result<(), String> {
        input.section("framebuffer blitter", 3, |r| {
            for value in &mut self.registers {
                *value = r.u32()?;
            }
            let front = r.bytes()?;
            if !front.is_empty() && front.len() != FB_BYTES as usize {
                return Err("invalid committed framebuffer size".into());
            }
            self.front = front.to_vec();
            self.generations = vec![
                u64::MAX - 1;
                if front.is_empty() {
                    0
                } else {
                    FB_MAX_HEIGHT as usize
                }
            ];
            self.presents = r.u64()?;
            self.active = (r.u32()?, r.u32()?);
            if !valid_size(self.active) {
                return Err("invalid framebuffer geometry".into());
            }
            self.pending = (r.u32()?, r.u32()?);
            if !valid_size(self.pending) {
                return Err("invalid pending framebuffer geometry".into());
            }
            self.requested = self.pending;
            self.geometry_changed = true;
            Ok(())
        })
    }
}

fn valid_size((width, height): (u32, u32)) -> bool {
    (320..=FB_MAX_WIDTH).contains(&width) && (200..=FB_MAX_HEIGHT).contains(&height)
}

fn valid_rectangle(offset: usize, width: usize, height: usize) -> bool {
    offset < FB_BYTES as usize
        && offset.is_multiple_of(4)
        && width > 0
        && width <= STRIDE
        && width.is_multiple_of(4)
        && offset % STRIDE + width <= STRIDE
        && height > 0
        && height <= FB_BYTES as usize / STRIDE
        && offset + (height - 1) * STRIDE + width <= FB_BYTES as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Bus, FB_BLITTER_BASE};
    use crate::devices::framebuffer::{FB_FIRST_PAGE, FB_MACHINE_RAM_BYTES};
    use crate::snapshot::{Reader, Writer};

    fn submit(bus: &mut Bus, registers: [u32; 7]) -> u64 {
        for (index, value) in registers.iter().enumerate().skip(1) {
            bus.write(FB_BLITTER_BASE + 4 + index as u64 * 4, *value as u64, 4)
                .unwrap();
        }
        bus.write(FB_BLITTER_BASE + 4, registers[0] as u64, 4)
            .unwrap();
        bus.read(FB_BLITTER_BASE + 4, 4).unwrap()
    }

    #[test]
    fn resolution_changes_commit_with_pixels_and_survive_restore() {
        use crate::devices::framebuffer::FB_MACHINE_RAM_BYTES;
        let mut machine = crate::Machine::new_system(FB_MACHINE_RAM_BYTES as usize);
        let bus = &mut machine.bus;
        assert!(bus.framebuffer_blitter.request_size(0, 900).is_err());
        assert!(bus.framebuffer_blitter.request_size(2049, 900).is_err());
        bus.framebuffer_blitter.request_size(1440, 900).unwrap();
        assert_eq!(bus.read(crate::bus::FB_BLITTER_BASE + 32, 4), Ok(1440));
        assert_eq!(bus.read(crate::bus::FB_BLITTER_BASE + 36, 4), Ok(900));
        assert!(bus
            .framebuffer_blitter
            .write(32, 1, 4, &mut bus.ram)
            .is_err());
        bus.framebuffer_blitter
            .write(40, (900 << 16) | 1440, 4, &mut bus.ram)
            .unwrap();
        assert_eq!(bus.framebuffer_blitter.size(), (FB_WIDTH, FB_HEIGHT));
        bus.ram
            .write(
                FB_RAM_OFFSET + 899 * FB_STRIDE as u64 + 1439 * 4,
                0x00123456,
                4,
            )
            .unwrap();
        bus.framebuffer_blitter
            .write(4, 3, 4, &mut bus.ram)
            .unwrap();
        assert_eq!(bus.framebuffer_blitter.size(), (1440, 900));
        let mut rows = Vec::new();
        machine.framebuffer_dirty_rows(&mut rows);
        assert_eq!(rows.len(), 900);
        let mut rgba = vec![0; 1440 * 4];
        assert_eq!(
            machine.framebuffer_copy_rows_rgba(&[899], &mut rgba),
            rgba.len()
        );
        assert_eq!(&rgba[1439 * 4..], &[0x12, 0x34, 0x56, 255]);
        let mut writer = Writer::new();
        machine.bus.framebuffer_blitter.snapshot(&mut writer);
        let bytes = writer.into_bytes();
        let mut restored = FramebufferBlitter::default();
        restored.restore(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(restored.size(), (1440, 900));
        assert_eq!(restored.read(32, 4), Ok(1440));
    }

    #[test]
    fn writes_in_either_page_dirty_the_same_scanline() {
        let mut machine =
            crate::Machine::new_system(crate::devices::framebuffer::FB_MACHINE_RAM_BYTES as usize);
        let mut rows = Vec::new();
        machine.framebuffer_dirty_rows(&mut rows);
        for column in [0, 1200] {
            rows.clear();
            machine
                .bus
                .ram
                .write(FB_RAM_OFFSET + 7 * FB_STRIDE as u64 + column * 4, 1, 4)
                .unwrap();
            machine.framebuffer_dirty_rows(&mut rows);
            assert_eq!(rows, [7]);
        }
    }

    #[test]
    fn presentation_hides_partial_draws_and_survives_snapshot() {
        let mut bus = Bus::new(FB_MACHINE_RAM_BYTES as usize);
        bus.ram.write_slice(FB_RAM_OFFSET, &[1, 2, 3, 4]).unwrap();
        submit(&mut bus, [3, 0, 0, 0, 0, 0, 0]);
        bus.ram.write_slice(FB_RAM_OFFSET, &[9, 8, 7, 6]).unwrap();
        assert_eq!(&bus.framebuffer_blitter.row(0)[..4], &[1, 2, 3, 4]);
        let mut writer = Writer::new();
        bus.framebuffer_blitter.snapshot(&mut writer);
        let bytes = writer.into_bytes();
        let mut restored = FramebufferBlitter::default();
        restored.restore(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(&restored.row(0)[..4], &[1, 2, 3, 4]);
        restored.present(&bus.ram).unwrap();
        assert_eq!(&restored.row(0)[..4], &[9, 8, 7, 6]);
        submit(&mut bus, [4, 0, 0, 0, 0, 0, 0]);
        assert!(!bus.framebuffer_blitter.committed());
    }

    #[test]
    fn overlapping_copies_match_an_unchanging_source_in_every_direction() {
        let mut bus = Bus::new(FB_MACHINE_RAM_BYTES as usize);
        let initial: Vec<u8> = (0..STRIDE * 8)
            .map(|i| (i * 17 + i / STRIDE) as u8)
            .collect();
        for (source, destination) in [(4, 16), (16, 4), (4, STRIDE + 16), (STRIDE + 16, 4)] {
            for mask in [u32::MAX, 0x00ff_ffff, 0x3333_cccc] {
                bus.ram.load_blob(FB_RAM_OFFSET, &initial).unwrap();
                let mut expected = initial.clone();
                for y in 0..6 {
                    for x in 0..256 {
                        let src = source + y * STRIDE + x;
                        let dst = destination + y * STRIDE + x;
                        let byte_mask = mask.to_le_bytes()[x % 4];
                        expected[dst] = (initial[src] & byte_mask) | (initial[dst] & !byte_mask);
                    }
                }
                assert_eq!(
                    submit(
                        &mut bus,
                        [1, source as u32, destination as u32, 256, 6, mask, 0]
                    ),
                    0
                );
                assert_eq!(
                    bus.ram.read_slice(FB_RAM_OFFSET, initial.len()).unwrap(),
                    expected
                );
            }
        }
    }

    #[test]
    fn fills_apply_raster_masks_and_mark_only_destination_rows_dirty() {
        let mut bus = Bus::new(FB_MACHINE_RAM_BYTES as usize);
        bus.ram
            .load_blob(FB_RAM_OFFSET, &vec![0x55; STRIDE * 4])
            .unwrap();
        for (and, xor) in [(0, 0x1234_5678), (0xff00_ff00, 0xabcd_ef01)] {
            let old = bus.ram.read(FB_RAM_OFFSET + STRIDE as u64 + 4, 4).unwrap() as u32;
            let generations = bus.ram.page_generations(FB_FIRST_PAGE, 8).to_vec();
            assert_eq!(
                submit(&mut bus, [2, 0, STRIDE as u32 + 4, 8, 2, and, xor]),
                0
            );
            for y in 1..3 {
                for x in [4, 8] {
                    assert_eq!(
                        bus.ram.read(FB_RAM_OFFSET + (y * STRIDE + x) as u64, 4),
                        Ok(((old & and) ^ xor) as u64)
                    );
                }
                assert_eq!(
                    bus.ram.read(FB_RAM_OFFSET + (y * STRIDE) as u64, 4),
                    Ok(0x5555_5555)
                );
                assert_eq!(
                    bus.ram.read(FB_RAM_OFFSET + (y * STRIDE + 12) as u64, 4),
                    Ok(0x5555_5555)
                );
            }
            let after = bus.ram.page_generations(FB_FIRST_PAGE, 8);
            for (page, (&before, &after)) in generations.iter().zip(after).enumerate() {
                assert_eq!(after, before + u64::from(page == 2 || page == 4));
            }
        }
    }

    #[test]
    fn invalid_commands_leave_the_framebuffer_untouched() {
        let mut bus = Bus::new(FB_MACHINE_RAM_BYTES as usize);
        for registers in [
            [0, 0, 0, 4, 1, 0, 1],
            [1, u32::MAX, 0, 4, 1, u32::MAX, 0],
            [2, 0, u32::MAX, 4, 1, 0, 1],
            [2, 0, 1, 4, 1, 0, 1],
            [2, 0, 0, u32::MAX, 1, 0, 1],
            [2, 0, 0, 4, u32::MAX, 0, 1],
            [2, 0, STRIDE as u32 - 4, 8, 1, 0, 1],
            [2, 0, FB_BYTES as u32 - 4, 4, 2, 0, 1],
            [2, 0, 0, 0, 1, 0, 1],
            [2, 0, 0, 4, 0, 0, 1],
        ] {
            assert_eq!(submit(&mut bus, registers), 1);
        }
        assert!(bus
            .ram
            .page_generations(FB_FIRST_PAGE, 768)
            .iter()
            .all(|&g| g == 0));
        let mut small_bus = Bus::new(4096);
        assert_eq!(submit(&mut small_bus, [2, 0, 0, 4, 1, 0, 1]), 1);
    }

    #[test]
    fn register_access_and_snapshot_preserve_a_partially_submitted_command() {
        let mut bus = Bus::new(FB_MACHINE_RAM_BYTES as usize);
        assert_eq!(bus.read(FB_BLITTER_BASE, 4), Ok(ID as u64));
        assert!(bus.read(FB_BLITTER_BASE, 8).is_err());
        assert!(bus.write(FB_BLITTER_BASE + 5, 1, 4).is_err());
        assert!(bus.write(FB_BLITTER_BASE, 1, 4).is_err());
        assert!(bus.write_accessible(FB_BLITTER_BASE + 28, 4));
        for (offset, value) in [(12, 4), (16, 4), (20, 1), (28, 0x1234_5678)] {
            bus.write(FB_BLITTER_BASE + offset, value, 4).unwrap();
        }
        let mut writer = Writer::new();
        bus.framebuffer_blitter.snapshot(&mut writer);
        let bytes = writer.into_bytes();
        let mut restored = FramebufferBlitter::default();
        restored.restore(&mut Reader::new(&bytes)).unwrap();
        restored.write(4, 2, 4, &mut bus.ram).unwrap();
        assert_eq!(bus.ram.read(FB_RAM_OFFSET + 4, 4), Ok(0x1234_5678));
    }
}
