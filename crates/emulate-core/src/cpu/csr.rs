//! CSR file: storage plus the access rules (privilege, WARL masks, views).
//!
//! `read`/`write` implement architectural access (used by CSR instructions)
//! and return `Err(())` for accesses that must raise illegal-instruction;
//! the caller maps that onto `Exception::IllegalInstruction(raw)`.
//! `load_raw`/`store_raw` bypass checks for internal trap machinery.

use super::Mode;

// User FP CSRs
pub const FFLAGS: u16 = 0x001;
pub const FRM: u16 = 0x002;
pub const FCSR: u16 = 0x003;
// User counters
pub const CYCLE: u16 = 0xC00;
pub const TIME: u16 = 0xC01;
pub const INSTRET: u16 = 0xC02;
// Supervisor
pub const SSTATUS: u16 = 0x100;
pub const SIE: u16 = 0x104;
pub const STVEC: u16 = 0x105;
pub const SCOUNTEREN: u16 = 0x106;
pub const SENVCFG: u16 = 0x10A;
pub const SSCRATCH: u16 = 0x140;
pub const SEPC: u16 = 0x141;
pub const SCAUSE: u16 = 0x142;
pub const STVAL: u16 = 0x143;
pub const SIP: u16 = 0x144;
pub const STIMECMP: u16 = 0x14D;
pub const SATP: u16 = 0x180;
// Machine
pub const MSTATUS: u16 = 0x300;
pub const MISA: u16 = 0x301;
pub const MEDELEG: u16 = 0x302;
pub const MIDELEG: u16 = 0x303;
pub const MIE: u16 = 0x304;
pub const MTVEC: u16 = 0x305;
pub const MCOUNTEREN: u16 = 0x306;
pub const MENVCFG: u16 = 0x30A;
pub const MCOUNTINHIBIT: u16 = 0x320;
pub const MSCRATCH: u16 = 0x340;
pub const MEPC: u16 = 0x341;
pub const MCAUSE: u16 = 0x342;
pub const MTVAL: u16 = 0x343;
pub const MIP: u16 = 0x344;
pub const PMPCFG0: u16 = 0x3A0;
pub const PMPADDR0: u16 = 0x3B0;
pub const MCYCLE: u16 = 0xB00;
pub const MINSTRET: u16 = 0xB02;
pub const MVENDORID: u16 = 0xF11;
pub const MARCHID: u16 = 0xF12;
pub const MIMPID: u16 = 0xF13;
pub const MHARTID: u16 = 0xF14;
pub const MCONFIGPTR: u16 = 0xF15;
// Debug trigger CSRs
pub const TSELECT: u16 = 0x7A0;
pub const TDATA1: u16 = 0x7A1;
pub const TDATA2: u16 = 0x7A2;
pub const TDATA3: u16 = 0x7A3;
pub const TINFO: u16 = 0x7A4;

// mstatus bit fields
pub mod mstatus {
    pub const SIE: u64 = 1 << 1;
    pub const MIE: u64 = 1 << 3;
    pub const SPIE: u64 = 1 << 5;
    pub const UBE: u64 = 1 << 6;
    pub const MPIE: u64 = 1 << 7;
    pub const SPP: u64 = 1 << 8;
    pub const MPP_MASK: u64 = 3 << 11;
    pub const MPP_SHIFT: u32 = 11;
    pub const FS_MASK: u64 = 3 << 13;
    pub const FS_SHIFT: u32 = 13;
    pub const XS_MASK: u64 = 3 << 15;
    pub const MPRV: u64 = 1 << 17;
    pub const SUM: u64 = 1 << 18;
    pub const MXR: u64 = 1 << 19;
    pub const TVM: u64 = 1 << 20;
    pub const TW: u64 = 1 << 21;
    pub const TSR: u64 = 1 << 22;
    pub const UXL_MASK: u64 = 3 << 32;
    pub const SXL_MASK: u64 = 3 << 34;
    pub const SD: u64 = 1 << 63;

    /// Bits software may modify via mstatus writes.
    pub const WRITE_MASK: u64 =
        SIE | MIE | SPIE | MPIE | SPP | MPP_MASK | FS_MASK | MPRV | SUM | MXR | TVM | TW | TSR;
    /// sstatus view (subset visible/writable from S-mode).
    pub const SSTATUS_MASK: u64 = SIE | SPIE | SPP | FS_MASK | XS_MASK | SUM | MXR | UXL_MASK | SD;
    pub const SSTATUS_WRITE_MASK: u64 = SIE | SPIE | SPP | FS_MASK | SUM | MXR;
}

pub mod menvcfg {
    pub const FIOM: u64 = 1;
    pub const ADUE: u64 = 1 << 61;
    pub const STCE: u64 = 1 << 63;
}

/// FS field values
pub const FS_OFF: u64 = 0;
pub const FS_INITIAL: u64 = 1;
pub const FS_CLEAN: u64 = 2;
pub const FS_DIRTY: u64 = 3;

const MIP_WRITE_MASK: u64 =
    crate::trap::irq::SSIP | crate::trap::irq::STIP | crate::trap::irq::SEIP;
const SIP_WRITE_MASK: u64 = crate::trap::irq::SSIP;
const MIE_MASK: u64 = crate::trap::irq::SSIP
    | crate::trap::irq::MSIP
    | crate::trap::irq::STIP
    | crate::trap::irq::MTIP
    | crate::trap::irq::SEIP
    | crate::trap::irq::MEIP;
const MIDELEG_MASK: u64 = 0x222;
// The C extension makes instruction-address-misaligned impossible, and
// ECALL-from-M is never delegatable. Page-fault causes 12, 13, and 15 are.
const MEDELEG_MASK: u64 = 0xC_B3FE;
const COUNTER_ENABLE_MASK: u64 = 0x7;
const MCOUNTINHIBIT_MASK: u64 = (1 << 0) | (1 << 2);
const MENVCFG_MASK: u64 = menvcfg::STCE | menvcfg::ADUE | menvcfg::FIOM;
const SENVCFG_MASK: u64 = 1; // FIOM

fn misa_value() -> u64 {
    let mxl = 2u64 << 62; // RV64
    let ext = |c: u8| 1u64 << (c - b'a') as u64;
    mxl | ext(b'a')
        | ext(b'c')
        | ext(b'd')
        | ext(b'f')
        | ext(b'i')
        | ext(b'm')
        | ext(b's')
        | ext(b'u')
}

pub struct CsrFile {
    regs: Box<[u64; 4096]>,
    device_mip: u64,
    legacy_pmp_stubs: bool,
    pub cycle: u64,
    pub instret: u64,
}

impl Default for CsrFile {
    fn default() -> Self {
        Self::new()
    }
}

impl CsrFile {
    pub fn new() -> Self {
        let mut regs: Box<[u64; 4096]> = vec![0u64; 4096].into_boxed_slice().try_into().unwrap();
        regs[MISA as usize] = misa_value();
        // UXL/SXL are read-only 2 (RV64); keep them present in the stored value.
        regs[MSTATUS as usize] = (2 << 32) | (2 << 34);
        regs[STIMECMP as usize] = u64::MAX;
        Self {
            regs,
            device_mip: 0,
            legacy_pmp_stubs: false,
            cycle: 0,
            instret: 0,
        }
    }

    /// Expose storage-only PMP CSRs for compatibility system profiles.
    ///
    /// The default profile has zero PMP entries, so these CSRs trap. Legacy
    /// riscv-tests and xv6's machine-mode startup assume PMP is present and
    /// have no architectural way to skip their PMP setup.
    pub fn enable_pmp_stubs(&mut self) {
        self.legacy_pmp_stubs = true;
    }

    /// Unchecked read of the stored value (trap machinery / devices sync).
    #[inline]
    pub fn load_raw(&self, addr: u16) -> u64 {
        match addr {
            MCYCLE | CYCLE => self.cycle,
            MINSTRET | INSTRET => self.instret,
            TSELECT | TDATA1 | TDATA2 | TDATA3 | TINFO => 0,
            MSTATUS => self.mstatus_read(),
            SSTATUS => self.mstatus_read() & mstatus::SSTATUS_MASK,
            MIP => self.mip_value(),
            SIE => self.regs[MIE as usize] & self.regs[MIDELEG as usize],
            SIP => self.mip_value() & self.regs[MIDELEG as usize],
            FFLAGS => self.regs[FCSR as usize] & 0x1F,
            FRM => (self.regs[FCSR as usize] >> 5) & 7,
            _ => self.regs[addr as usize],
        }
    }

    /// Unchecked write (trap machinery). Applies field coercions but no
    /// privilege/legality checks and no FS side effects.
    #[inline]
    pub fn store_raw(&mut self, addr: u16, val: u64) {
        match addr {
            MSTATUS => self.mstatus_write(val, mstatus::WRITE_MASK),
            SSTATUS => self.mstatus_write(val, mstatus::SSTATUS_WRITE_MASK),
            MCYCLE | CYCLE => self.cycle = val,
            MINSTRET | INSTRET => self.instret = val,
            TSELECT | TDATA1 | TDATA2 | TDATA3 | TINFO => {}
            MIP => self.write_mip(val),
            SIP => self.write_sip(val),
            SIE => self.write_sie(val),
            FFLAGS => {
                let f = self.regs[FCSR as usize];
                self.regs[FCSR as usize] = (f & !0x1F) | (val & 0x1F);
            }
            FRM => {
                let f = self.regs[FCSR as usize];
                self.regs[FCSR as usize] = (f & !0xE0) | ((val & 7) << 5);
            }
            _ => self.regs[addr as usize] = val,
        }
    }

    /// Replace the device-driven subset of `mip` without disturbing pending
    /// bits written by software through the CSR.
    pub fn set_device_mip(&mut self, mask: u64, pending: u64) {
        self.device_mip = (self.device_mip & !mask) | (pending & mask);
    }

    fn mip_value(&self) -> u64 {
        let mut software = self.regs[MIP as usize];
        if self.stce_enabled() {
            software &= !crate::trap::irq::STIP;
        }
        software | self.device_mip
    }

    #[inline]
    pub fn stce_enabled(&self) -> bool {
        self.regs[MENVCFG as usize] & menvcfg::STCE != 0
    }

    #[inline]
    pub fn adue_enabled(&self) -> bool {
        self.regs[MENVCFG as usize] & menvcfg::ADUE != 0
    }

    fn write_mip(&mut self, val: u64) {
        let writable = if self.stce_enabled() {
            MIP_WRITE_MASK & !crate::trap::irq::STIP
        } else {
            MIP_WRITE_MASK
        };
        let old = self.regs[MIP as usize];
        self.regs[MIP as usize] = (old & !writable) | (val & writable);
    }

    fn write_sip(&mut self, val: u64) {
        let writable = SIP_WRITE_MASK & self.regs[MIDELEG as usize];
        let old = self.regs[MIP as usize];
        self.regs[MIP as usize] = (old & !writable) | (val & writable);
    }

    fn write_sie(&mut self, val: u64) {
        let delegated = self.regs[MIDELEG as usize];
        let old = self.regs[MIE as usize];
        self.regs[MIE as usize] = (old & !delegated) | (val & delegated & MIE_MASK);
    }

    fn mstatus_read(&self) -> u64 {
        let mut v = self.regs[MSTATUS as usize];
        if (v & mstatus::FS_MASK) == mstatus::FS_MASK {
            v |= mstatus::SD;
        } else {
            v &= !mstatus::SD;
        }
        v
    }

    fn mstatus_write(&mut self, val: u64, mask: u64) {
        let old = self.regs[MSTATUS as usize];
        let mut new = (old & !mask) | (val & mask);
        // MPP is WARL {0,1,3}: coerce 2 -> previous value.
        if mask & mstatus::MPP_MASK != 0 {
            let mpp = (new & mstatus::MPP_MASK) >> mstatus::MPP_SHIFT;
            if mpp == 2 {
                new = (new & !mstatus::MPP_MASK) | (old & mstatus::MPP_MASK);
            }
        }
        // UXL/SXL read-only 2.
        new = (new & !(mstatus::UXL_MASK | mstatus::SXL_MASK)) | (2 << 32) | (2 << 34);
        self.regs[MSTATUS as usize] = new;
    }

    #[inline]
    pub fn fs(&self) -> u64 {
        (self.regs[MSTATUS as usize] & mstatus::FS_MASK) >> mstatus::FS_SHIFT
    }

    #[inline]
    pub fn set_fs_dirty(&mut self) {
        self.regs[MSTATUS as usize] |= mstatus::FS_MASK; // 3 = Dirty
    }

    #[inline]
    pub fn mstatus_field(&self, bit: u64) -> bool {
        self.regs[MSTATUS as usize] & bit != 0
    }

    /// Checked CSR read for CSR instructions. `mtime` backs the TIME CSR.
    pub fn read(&self, addr: u16, mode: Mode, mtime: u64) -> Result<u64, ()> {
        self.check_access(addr, mode, false)?;
        Ok(match addr {
            TIME => mtime,
            _ => self.load_raw(addr),
        })
    }

    /// Checked CSR write for CSR instructions.
    pub fn write(&mut self, addr: u16, val: u64, mode: Mode) -> Result<(), ()> {
        self.check_access(addr, mode, true)?;
        match addr {
            MISA | MVENDORID | MARCHID | MIMPID | MHARTID | MCONFIGPTR => {} // WARL ignore
            // No triggers are implemented: the selector and data bank are
            // hardwired to zero so software can discover that.
            TSELECT | TDATA1 | TDATA2 | TDATA3 | TINFO => {}
            MSTATUS => self.mstatus_write(val, mstatus::WRITE_MASK),
            SSTATUS => self.mstatus_write(val, mstatus::SSTATUS_WRITE_MASK),
            MIP => self.write_mip(val),
            SIP => self.write_sip(val),
            MIE => self.regs[MIE as usize] = val & MIE_MASK,
            SIE => self.write_sie(val),
            MEDELEG => self.regs[MEDELEG as usize] = val & MEDELEG_MASK,
            MIDELEG => self.regs[MIDELEG as usize] = val & MIDELEG_MASK,
            MCOUNTEREN => self.regs[MCOUNTEREN as usize] = val & COUNTER_ENABLE_MASK,
            SCOUNTEREN => self.regs[SCOUNTEREN as usize] = val & COUNTER_ENABLE_MASK,
            MCOUNTINHIBIT => self.regs[MCOUNTINHIBIT as usize] = val & MCOUNTINHIBIT_MASK,
            MENVCFG => self.regs[MENVCFG as usize] = val & MENVCFG_MASK,
            SENVCFG => self.regs[SENVCFG as usize] = val & SENVCFG_MASK,
            MTVEC | STVEC => {
                // mode field WARL: 0 (direct) or 1 (vectored).
                let mode_bits = val & 3;
                let coerced = if mode_bits > 1 { val & !3 } else { val };
                self.regs[addr as usize] = coerced;
            }
            MEPC | SEPC => self.regs[addr as usize] = val & !1,
            SATP => {
                // Mode WARL: only Bare (0) and Sv39 (8); other writes ignored entirely.
                let m = val >> 60;
                if m == 0 || m == 8 {
                    // RV64 Sv39's MODE, 16-bit ASID, and 44-bit PPN occupy
                    // the entire CSR, so every bit is writable.
                    self.regs[SATP as usize] = val;
                }
            }
            FFLAGS => {
                let f = self.regs[FCSR as usize];
                self.regs[FCSR as usize] = (f & !0x1F) | (val & 0x1F);
                self.set_fs_dirty();
            }
            FRM => {
                let f = self.regs[FCSR as usize];
                self.regs[FCSR as usize] = (f & !0xE0) | ((val & 7) << 5);
                self.set_fs_dirty();
            }
            FCSR => {
                self.regs[FCSR as usize] = val & 0xFF;
                self.set_fs_dirty();
            }
            MCYCLE => self.cycle = val,
            MINSTRET => self.instret = val,
            CYCLE | TIME | INSTRET => return Err(()), // read-only user counters
            _ => self.regs[addr as usize] = val,
        }
        Ok(())
    }

    fn check_access(&self, addr: u16, mode: Mode, is_write: bool) -> Result<(), ()> {
        // Privilege encoded in addr bits 9:8.
        let required = ((addr >> 8) & 3) as u8;
        if (mode as u8) < required {
            return Err(());
        }
        // Read-only region: top two bits == 0b11.
        if is_write && (addr >> 10) & 3 == 3 {
            return Err(());
        }
        // TVM: S-mode access to satp traps.
        if addr == SATP && mode == Mode::Supervisor && self.mstatus_field(mstatus::TVM) {
            return Err(());
        }
        // Sstc grants S-mode access only when both machine-level gates are
        // open. M-mode is always permitted; U-mode already failed the
        // privilege check encoded in the CSR address.
        if addr == STIMECMP
            && mode != Mode::Machine
            && (!self.stce_enabled() || self.regs[MCOUNTEREN as usize] & (1 << 1) == 0)
        {
            return Err(());
        }
        // FP CSRs are illegal when FS is Off.
        if matches!(addr, FFLAGS | FRM | FCSR) && self.fs() == FS_OFF {
            return Err(());
        }
        // User counters gated by mcounteren/scounteren.
        if matches!(addr, CYCLE | TIME | INSTRET) {
            let bit = 1u64 << (addr - CYCLE);
            if mode != Mode::Machine && self.regs[MCOUNTEREN as usize] & bit == 0 {
                return Err(());
            }
            if mode == Mode::User && self.regs[SCOUNTEREN as usize] & bit == 0 {
                return Err(());
            }
        }
        self.exists(addr)
    }

    fn exists(&self, addr: u16) -> Result<(), ()> {
        if self.legacy_pmp_stubs && matches!(addr, PMPCFG0 | PMPADDR0) {
            return Ok(());
        }
        match addr {
            FFLAGS | FRM | FCSR | CYCLE | TIME | INSTRET => Ok(()),
            SSTATUS | SIE | STVEC | SCOUNTEREN | SENVCFG | SSCRATCH | SEPC | SCAUSE | STVAL
            | SIP | STIMECMP | SATP => Ok(()),
            MSTATUS | MISA | MEDELEG | MIDELEG | MIE | MTVEC | MCOUNTEREN | MENVCFG
            | MCOUNTINHIBIT | MSCRATCH | MEPC | MCAUSE | MTVAL | MIP => Ok(()),
            MVENDORID | MARCHID | MIMPID | MHARTID | MCONFIGPTR => Ok(()),
            TSELECT | TDATA1 | TDATA2 | TDATA3 | TINFO => Ok(()),
            MCYCLE | MINSTRET => Ok(()),
            _ => Err(()),
        }
    }
}

// ---------------------------------------------------------------------------
// State snapshot
// ---------------------------------------------------------------------------

const SNAPSHOT_VERSION: u16 = 1;

impl CsrFile {
    /// Serialize the whole 4096-entry file plus the state that is not stored
    /// in it: the device-driven `mip` levels, whether the compatibility PMP
    /// bank is exposed, and the two counters that live outside `regs`.
    ///
    /// The file is written whole rather than as a list of the CSRs this build
    /// implements, so adding a CSR does not silently change what a snapshot
    /// means. Most of it is zero, and the container is compressed.
    pub(crate) fn snapshot(&self, out: &mut crate::snapshot::Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            w.u32(self.regs.len() as u32);
            for value in self.regs.iter() {
                w.u64(*value);
            }
            w.u64(self.device_mip);
            w.bool(self.legacy_pmp_stubs);
            w.u64(self.cycle);
            w.u64(self.instret);
        });
    }

    pub(crate) fn restore(
        &mut self,
        input: &mut crate::snapshot::Reader<'_>,
    ) -> Result<(), String> {
        input.section("csr", SNAPSHOT_VERSION, |r| {
            let count = r.u32()? as usize;
            if count != self.regs.len() {
                return Err(format!(
                    "{count} CSR slots, this build has {}",
                    self.regs.len()
                ));
            }
            for slot in self.regs.iter_mut() {
                *slot = r.u64()?;
            }
            self.device_mip = r.u64()?;
            self.legacy_pmp_stubs = r.bool()?;
            self.cycle = r.u64()?;
            self.instret = r.u64()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_csrs_are_hardwired_to_zero_when_no_triggers_exist() {
        let mut csr = CsrFile::new();

        for addr in [TSELECT, TDATA1, TDATA2, TDATA3, TINFO] {
            csr.write(addr, u64::MAX, Mode::Machine).unwrap();
        }

        let values = [TSELECT, TDATA1, TDATA2, TDATA3, TINFO]
            .map(|addr| csr.read(addr, Mode::Machine, 0).unwrap());
        assert_eq!(values, [0; 5]);
    }

    #[test]
    fn trigger_csrs_are_inaccessible_below_machine_mode() {
        let csr = CsrFile::new();

        assert_eq!(csr.read(TSELECT, Mode::Supervisor, 0), Err(()));
    }

    #[test]
    fn profile_masks_delegation_counter_and_environment_csrs() {
        let mut csr = CsrFile::new();

        for (addr, expected) in [
            (MEDELEG, MEDELEG_MASK),
            (MIDELEG, MIDELEG_MASK),
            (MCOUNTEREN, COUNTER_ENABLE_MASK),
            (SCOUNTEREN, COUNTER_ENABLE_MASK),
            (MCOUNTINHIBIT, MCOUNTINHIBIT_MASK),
            (MENVCFG, MENVCFG_MASK),
            (SENVCFG, SENVCFG_MASK),
        ] {
            csr.write(addr, u64::MAX, Mode::Machine).unwrap();
            assert_eq!(csr.read(addr, Mode::Machine, 0).unwrap(), expected);
        }
    }

    #[test]
    fn stimecmp_supervisor_access_requires_stce_and_tm() {
        let mut csr = CsrFile::new();
        assert_eq!(csr.read(STIMECMP, Mode::Supervisor, 0), Err(()));

        csr.write(MENVCFG, menvcfg::STCE, Mode::Machine).unwrap();
        assert_eq!(csr.read(STIMECMP, Mode::Supervisor, 0), Err(()));

        csr.write(MCOUNTEREN, 1 << 1, Mode::Machine).unwrap();
        assert_eq!(csr.read(STIMECMP, Mode::Supervisor, 0), Ok(u64::MAX));
        assert_eq!(csr.write(STIMECMP, 42, Mode::Supervisor), Ok(()));
    }

    #[test]
    fn stimecmp_machine_access_ignores_stce_and_tm() {
        let mut csr = CsrFile::new();

        assert_eq!(csr.write(STIMECMP, 42, Mode::Machine), Ok(()));

        assert_eq!(csr.read(STIMECMP, Mode::Machine, 0), Ok(42));
    }

    #[test]
    fn stce_makes_stip_read_only_and_timer_driven() {
        let mut csr = CsrFile::new();
        csr.write(MIP, crate::trap::irq::STIP, Mode::Machine)
            .unwrap();
        csr.write(MENVCFG, menvcfg::STCE, Mode::Machine).unwrap();
        assert_eq!(
            csr.read(MIP, Mode::Machine, 0).unwrap() & crate::trap::irq::STIP,
            0
        );

        csr.set_device_mip(crate::trap::irq::STIP, crate::trap::irq::STIP);

        assert_eq!(
            csr.read(MIP, Mode::Machine, 0).unwrap() & crate::trap::irq::STIP,
            crate::trap::irq::STIP
        );
    }

    #[test]
    fn checked_fp_csr_writes_dirty_fs() {
        for addr in [FFLAGS, FRM, FCSR] {
            let mut csr = CsrFile::new();
            csr.store_raw(MSTATUS, FS_INITIAL << mstatus::FS_SHIFT);

            csr.write(addr, 0, Mode::Machine).unwrap();

            assert_eq!(csr.fs(), FS_DIRTY, "CSR {addr:#x}");
        }
    }

    #[test]
    fn unsupported_hpm_counters_and_event_selectors_trap() {
        let csr = CsrFile::new();

        assert_eq!(csr.read(0xB03, Mode::Machine, 0), Err(()));
        assert_eq!(csr.read(0xC03, Mode::Machine, 0), Err(()));
        assert_eq!(csr.read(0x323, Mode::Machine, 0), Err(()));
    }

    #[test]
    fn device_pending_bits_do_not_erase_software_pending_bits() {
        let mut csr = CsrFile::new();
        csr.write(MIP, crate::trap::irq::STIP, Mode::Machine)
            .unwrap();

        csr.set_device_mip(crate::trap::irq::STIP, 0);

        assert_eq!(
            csr.read(MIP, Mode::Machine, 0).unwrap() & crate::trap::irq::STIP,
            crate::trap::irq::STIP
        );
    }

    #[test]
    fn raw_mip_round_trip_does_not_latch_device_pending_bits() {
        let mut csr = CsrFile::new();
        csr.set_device_mip(crate::trap::irq::MEIP, crate::trap::irq::MEIP);

        csr.store_raw(MIP, csr.load_raw(MIP));
        csr.set_device_mip(crate::trap::irq::MEIP, 0);

        assert_eq!(csr.load_raw(MIP) & crate::trap::irq::MEIP, 0);
    }

    #[test]
    fn raw_supervisor_interrupt_aliases_update_machine_backing_csrs() {
        let mut csr = CsrFile::new();
        csr.write(MIDELEG, u64::MAX, Mode::Machine).unwrap();

        csr.store_raw(SIE, crate::trap::irq::SSIP | crate::trap::irq::MEIP);
        csr.store_raw(SIP, crate::trap::irq::SSIP | crate::trap::irq::STIP);

        assert_eq!(
            (csr.load_raw(MIE), csr.load_raw(MIP)),
            (crate::trap::irq::SSIP, crate::trap::irq::SSIP)
        );
    }

    #[test]
    fn raw_fp_aliases_update_fcsr_fields() {
        let mut csr = CsrFile::new();

        csr.store_raw(FFLAGS, 0x15);
        csr.store_raw(FRM, 6);

        assert_eq!(csr.load_raw(FCSR), 0xd5);
    }

    #[test]
    fn pmp_csrs_trap_when_the_hart_has_no_pmp_entries() {
        let csr = CsrFile::new();

        assert_eq!(csr.read(PMPCFG0, Mode::Machine, 0), Err(()));
        assert_eq!(csr.read(PMPADDR0, Mode::Machine, 0), Err(()));
    }

    #[test]
    fn legacy_pmp_stubs_are_opt_in_storage_only_csrs() {
        let mut csr = CsrFile::new();
        csr.enable_pmp_stubs();

        csr.write(PMPCFG0, 0x18, Mode::Machine).unwrap();
        csr.write(PMPADDR0, u64::MAX, Mode::Machine).unwrap();

        assert_eq!(csr.read(PMPCFG0, Mode::Machine, 0), Ok(0x18));
        assert_eq!(csr.read(PMPADDR0, Mode::Machine, 0), Ok(u64::MAX));
    }

    #[test]
    fn satp_preserves_the_full_sv39_asid() {
        let mut csr = CsrFile::new();
        let sv39 = 0x8fff_f123_4567_89ab;

        csr.write(SATP, sv39, Mode::Machine).unwrap();

        assert_eq!(csr.read(SATP, Mode::Machine, 0), Ok(sv39));
        csr.write(SATP, 0x9fff_ffff_ffff_ffff, Mode::Machine)
            .unwrap();
        assert_eq!(csr.read(SATP, Mode::Machine, 0), Ok(sv39));
    }
}
