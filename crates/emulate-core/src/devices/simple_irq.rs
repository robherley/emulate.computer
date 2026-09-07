//! ACT4/Sail simple interrupt generator.
//!
//! A 32-bit command write at offset 4 names interrupt-pending bits in the
//! low 31 bits. Bit 31 selects set (one) or clear (zero).

use crate::snapshot::{Reader, Writer};

use crate::trap::irq;

const COMMAND: u64 = 4;
const SET: u32 = 1 << 31;
const SUPPORTED: u32 = (irq::SSIP | irq::SEIP | irq::MEIP) as u32;

#[derive(Default)]
pub struct SimpleInterruptGenerator {
    pending: u32,
}

impl SimpleInterruptGenerator {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn pending(&self) -> u64 {
        self.pending as u64
    }

    pub fn read(&self, offset: u64, _size: u8) -> Result<u64, ()> {
        match offset {
            COMMAND => Ok(self.pending as u64),
            0..=0x1F => Ok(0),
            _ => Err(()),
        }
    }

    pub fn write(&mut self, offset: u64, val: u64, size: u8) -> Result<(), ()> {
        if offset != COMMAND || size != 4 {
            return if offset <= 0x1F { Ok(()) } else { Err(()) };
        }

        let command = val as u32;
        let mask = command & SUPPORTED;
        if command & SET != 0 {
            self.pending |= mask;
        } else {
            self.pending &= !mask;
        }
        Ok(())
    }
}

const SNAPSHOT_VERSION: u16 = 1;

impl SimpleInterruptGenerator {
    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| w.u32(self.pending));
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("simple-irq", SNAPSHOT_VERSION, |r| {
            // Levels outside the platform profile are unreachable through the
            // MMIO write path, so a container carrying them is not ours.
            let pending = r.u32()?;
            if pending & !SUPPORTED != 0 {
                return Err(format!("pending mask {pending:#x} is outside the profile"));
            }
            self.pending = pending;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_sets_selected_interrupt_levels() {
        let mut generator = SimpleInterruptGenerator::new();

        generator
            .write(COMMAND, (SET | irq::MEIP as u32) as u64, 4)
            .unwrap();

        assert_eq!(generator.pending(), irq::MEIP);
    }

    #[test]
    fn command_clears_only_selected_interrupt_levels() {
        let mut generator = SimpleInterruptGenerator::new();
        generator
            .write(COMMAND, (SET | SUPPORTED) as u64, 4)
            .unwrap();

        generator.write(COMMAND, irq::SEIP, 4).unwrap();

        assert_eq!(generator.pending(), irq::SSIP | irq::MEIP);
    }

    #[test]
    fn command_ignores_interrupt_levels_outside_the_platform_profile() {
        let mut generator = SimpleInterruptGenerator::new();

        generator
            .write(COMMAND, (SET | irq::MSIP as u32) as u64, 4)
            .unwrap();

        assert_eq!(generator.pending(), 0);
    }
}
