//! CLINT (core-local interruptor), single hart.
//!
//! Layout (offsets from CLINT_BASE, QEMU virt / SiFive convention):
//! - 0x0000: msip (hart 0), 32-bit, bit 0 significant
//! - 0x4000: mtimecmp (hart 0), 64-bit (also accessible as two 32-bit halves)
//! - 0xBFF8: mtime, 64-bit (also as two 32-bit halves)
//!
//! `mtime` is advanced by the machine (host-driven); reads/writes of any of
//! the three registers by the guest must work at 4- and 8-byte granularity.

use crate::snapshot::{Reader, Writer};

pub struct Clint {
    pub mtime: u64,
    pub mtimecmp: u64,
    pub msip: bool,
}

const MSIP: u64 = 0x0;
const MTIMECMP: u64 = 0x4000;
const MTIME: u64 = 0xBFF8;

/// Read a 64-bit register at 4- or 8-byte granularity. `half` is 0 for the
/// base offset (low half), 1 for base+4 (high half).
#[inline]
fn read64(reg: u64, half: u64, size: u8) -> u64 {
    match (half, size) {
        (0, 8) => reg,
        (0, _) => reg & 0xFFFF_FFFF,
        (_, _) => reg >> 32,
    }
}

/// Write a 64-bit register at 4- or 8-byte granularity (see `read64`).
#[inline]
fn write64(reg: &mut u64, half: u64, val: u64, size: u8) {
    match (half, size) {
        (0, 8) => *reg = val,
        (0, _) => *reg = (*reg & !0xFFFF_FFFF) | (val & 0xFFFF_FFFF),
        (_, _) => *reg = (*reg & 0xFFFF_FFFF) | ((val & 0xFFFF_FFFF) << 32),
    }
}

impl Clint {
    pub fn new() -> Self {
        Clint {
            mtime: 0,
            mtimecmp: u64::MAX,
            msip: false,
        }
    }

    /// Machine timer interrupt pending?
    #[inline]
    pub fn mtip(&self) -> bool {
        self.mtime >= self.mtimecmp
    }

    pub fn read(&mut self, offset: u64, size: u8) -> Result<u64, ()> {
        match offset {
            MSIP => Ok(self.msip as u64),
            MTIMECMP => Ok(read64(self.mtimecmp, 0, size)),
            o if o == MTIMECMP + 4 => Ok(read64(self.mtimecmp, 1, size)),
            MTIME => Ok(read64(self.mtime, 0, size)),
            o if o == MTIME + 4 => Ok(read64(self.mtime, 1, size)),
            // Region is only mapped for hart 0; everything else reads 0.
            _ => Ok(0),
        }
    }

    pub fn write(&mut self, offset: u64, val: u64, size: u8) -> Result<(), ()> {
        match offset {
            MSIP => self.msip = val & 1 != 0,
            MTIMECMP => write64(&mut self.mtimecmp, 0, val, size),
            o if o == MTIMECMP + 4 => write64(&mut self.mtimecmp, 1, val, size),
            MTIME => write64(&mut self.mtime, 0, val, size),
            o if o == MTIME + 4 => write64(&mut self.mtime, 1, val, size),
            // Region is only mapped for hart 0; other writes are ignored.
            _ => {}
        }
        Ok(())
    }
}

impl Default for Clint {
    fn default() -> Self {
        Self::new()
    }
}

const SNAPSHOT_VERSION: u16 = 1;

impl Clint {
    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            w.u64(self.mtime);
            w.u64(self.mtimecmp);
            w.bool(self.msip);
        });
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("clint", SNAPSHOT_VERSION, |r| {
            self.mtime = r.u64()?;
            self.mtimecmp = r.u64()?;
            self.msip = r.bool()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtimecmp_8_byte_access() {
        let mut c = Clint::new();
        c.write(0x4000, 0x1122_3344_5566_7788, 8).unwrap();
        assert_eq!(c.mtimecmp, 0x1122_3344_5566_7788);
        assert_eq!(c.read(0x4000, 8).unwrap(), 0x1122_3344_5566_7788);
    }

    #[test]
    fn mtimecmp_4_byte_halves_assemble() {
        let mut c = Clint::new();
        // Linux-style: write high half all-ones first, then low, then high.
        c.write(0x4004, 0xFFFF_FFFF, 4).unwrap();
        c.write(0x4000, 0xDEAD_BEEF, 4).unwrap();
        c.write(0x4004, 0x0000_00AB, 4).unwrap();
        assert_eq!(c.mtimecmp, 0x0000_00AB_DEAD_BEEF);
        // 32-bit reads return each half zero-extended.
        assert_eq!(c.read(0x4000, 4).unwrap(), 0xDEAD_BEEF);
        assert_eq!(c.read(0x4004, 4).unwrap(), 0x0000_00AB);
    }

    #[test]
    fn mtime_4_and_8_byte_access() {
        let mut c = Clint::new();
        c.mtime = 0xAABB_CCDD_0011_2233;
        assert_eq!(c.read(0xBFF8, 8).unwrap(), 0xAABB_CCDD_0011_2233);
        assert_eq!(c.read(0xBFF8, 4).unwrap(), 0x0011_2233);
        assert_eq!(c.read(0xBFFC, 4).unwrap(), 0xAABB_CCDD);
        // 32-bit write replaces just the addressed half.
        c.write(0xBFF8, 0x4455_6677, 4).unwrap();
        assert_eq!(c.mtime, 0xAABB_CCDD_4455_6677);
        c.write(0xBFFC, 0x8899_0011, 4).unwrap();
        assert_eq!(c.mtime, 0x8899_0011_4455_6677);
    }

    #[test]
    fn msip_and_mtip() {
        let mut c = Clint::new();
        assert!(!c.mtip()); // mtimecmp = u64::MAX at reset
        c.write(0x4000, 100, 8).unwrap();
        c.mtime = 99;
        assert!(!c.mtip());
        c.mtime = 100;
        assert!(c.mtip());

        assert_eq!(c.read(0x0, 4).unwrap(), 0);
        c.write(0x0, 1, 4).unwrap();
        assert!(c.msip);
        assert_eq!(c.read(0x0, 4).unwrap(), 1);
        c.write(0x0, 0xFFFF_FFFE, 4).unwrap(); // bit 0 clear
        assert!(!c.msip);
    }

    #[test]
    fn unmapped_offsets_in_range() {
        let mut c = Clint::new();
        assert_eq!(c.read(0x8, 4).unwrap(), 0);
        assert_eq!(c.read(0x4008, 8).unwrap(), 0); // hart 1 mtimecmp: not mapped
        assert!(c.write(0x4008, 0x1234, 8).is_ok());
        assert_eq!(c.mtimecmp, u64::MAX); // untouched
    }
}
