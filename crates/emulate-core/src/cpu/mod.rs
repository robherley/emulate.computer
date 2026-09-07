//! Single RV64GC hart: registers, privilege state, fetch/step, traps.

pub mod bitmanip;
pub mod csr;
pub mod decode;
mod decode_cache;
pub mod execute;
pub mod fpu;

use crate::bus::{Bus, DRAM_BASE};
use crate::mmu::{AccessType, Mmu};
use crate::trap::{irq, Exception, Interrupt};
use csr::mstatus;
pub use decode_cache::DecodeCacheStats;

/// One-entry memo of the last instruction-fetch translation.
///
/// The block cache is keyed on physical addresses, so every block entry has to
/// translate the PC. This makes the repeat case a three-word comparison
/// instead of four CSR reads and a TLB probe.
///
/// Fetch permission depends only on `satp`, the privilege mode, and the PTEs:
/// `mstatus.MXR` and `mstatus.SUM` apply to loads and stores, never to
/// fetches. So the tag needs the virtual page, the mode, and the MMU flush
/// generation, which every `satp` write and `SFENCE.VMA` bumps. This memo is
/// therefore exactly as strong as the TLB it shortcuts: a page-table edit that
/// is not followed by an `SFENCE.VMA` is not guaranteed to be visible to
/// either.
#[derive(Clone, Copy)]
struct FetchTranslation {
    vpn: u64,
    pa_base: u64,
    mode: Mode,
    mmu_generation: u64,
}

struct FetchedInstruction {
    raw: u32,
    /// Physical RAM page and its generation.  `None` means the instruction is
    /// valid to execute but must not be put in the decoded block cache.
    cache_page: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub enum Mode {
    User = 0,
    Supervisor = 1,
    Machine = 3,
}

impl Mode {
    fn from_bits(v: u64) -> Mode {
        // Fail closed: only 1 and 3 select elevated privilege. The reserved
        // MPP/SPP encoding 2 (and anything unexpected) maps to User so a bad
        // value can never grant Machine privilege / bypass translation.
        match v {
            1 => Mode::Supervisor,
            3 => Mode::Machine,
            _ => Mode::User,
        }
    }
}

pub struct Cpu {
    pub regs: [u64; 32],
    /// FP registers; f32 values are NaN-boxed (upper 32 bits all-ones).
    pub fregs: [u64; 32],
    pub pc: u64,
    /// Set to pc+len before execute; jumps/branches overwrite it.
    pub next_pc: u64,
    pub mode: Mode,
    pub csr: csr::CsrFile,
    pub mmu: Mmu,
    decode_cache: decode_cache::DecodeCache,
    fetch_translation: Option<FetchTranslation>,
    /// LR/SC reservation (physical address), cleared by stores/traps/SC.
    pub reservation: Option<u64>,
    /// Halted in WFI waiting for an interrupt.
    pub wfi: bool,
}

/// Result of one step: Ok(()) or "trap was taken" (already handled).
pub enum StepResult {
    Executed,
    WaitingForInterrupt,
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            regs: [0; 32],
            fregs: [0; 32],
            pc: 0,
            next_pc: 0,
            mode: Mode::Machine,
            csr: csr::CsrFile::new(),
            mmu: Mmu::new(),
            decode_cache: decode_cache::DecodeCache::new(),
            fetch_translation: None,
            reservation: None,
            wfi: false,
        }
    }

    #[inline]
    pub fn reg(&self, r: usize) -> u64 {
        self.regs[r]
    }

    #[inline]
    pub fn set_reg(&mut self, r: usize, v: u64) {
        if r != 0 {
            self.regs[r] = v;
        }
    }

    /// Effective privilege for data accesses (honors mstatus.MPRV).
    #[inline]
    fn data_mode(&self) -> Mode {
        if self.csr.mstatus_field(mstatus::MPRV) {
            let mpp = (self.csr.load_raw(csr::MSTATUS) & mstatus::MPP_MASK) >> mstatus::MPP_SHIFT;
            Mode::from_bits(mpp)
        } else {
            self.mode
        }
    }

    #[inline]
    fn translate(
        &mut self,
        bus: &mut Bus,
        va: u64,
        acc: AccessType,
        mode: Mode,
    ) -> Result<u64, Exception> {
        let satp = self.csr.load_raw(csr::SATP);
        // Identity early-out so the common untranslated access skips the
        // mxr/sum/adue CSR reads. `mmu.translate` re-checks the same
        // condition; this only hoists it.
        if satp >> 60 != 8 || mode == Mode::Machine {
            return Ok(va);
        }
        let mxr = self.csr.mstatus_field(mstatus::MXR);
        let sum = self.csr.mstatus_field(mstatus::SUM);
        let adue = self.csr.adue_enabled();
        self.mmu.translate(bus, satp, mode, mxr, sum, adue, acc, va)
    }

    /// Translate `pc` for an instruction fetch, memoising the page.
    #[inline]
    fn translate_fetch(&mut self, bus: &mut Bus, pc: u64) -> Result<u64, Exception> {
        let vpn = pc >> 12;
        if let Some(cached) = self.fetch_translation {
            if cached.vpn == vpn
                && cached.mode == self.mode
                && cached.mmu_generation == self.mmu.generation()
            {
                return Ok(cached.pa_base | (pc & 0xFFF));
            }
        }
        // Only a translation that succeeded is memoised, so a faulting fetch
        // takes the fault every time it is retried.
        let pa = self.translate(bus, pc, AccessType::Fetch, self.mode)?;
        self.fetch_translation = Some(FetchTranslation {
            vpn,
            pa_base: pa & !0xFFF,
            mode: self.mode,
            mmu_generation: self.mmu.generation(),
        });
        Ok(pa)
    }

    /// Read the 32-bit instruction window at already-translated `pa`, handling
    /// a compressed instruction ending the page and a 4-byte instruction
    /// straddling pages.
    fn fetch_window(
        &mut self,
        bus: &mut Bus,
        pc: u64,
        pa: u64,
    ) -> Result<FetchedInstruction, Exception> {
        let low = bus
            .read(pa, 2)
            .map_err(|_| Exception::InstructionAccessFault(pc))? as u32;
        if low & 3 != 3 {
            return Ok(FetchedInstruction {
                raw: low,
                cache_page: ram_page_generation(bus, pa),
            });
        }
        // 32-bit instruction: fetch upper half (may live on the next page).
        let pc2 = pc.wrapping_add(2);
        let pa2 = if pc2 & 0xFFF == 0 {
            self.translate(bus, pc2, AccessType::Fetch, self.mode)?
        } else {
            pa + 2
        };
        let high = bus
            .read(pa2, 2)
            .map_err(|_| Exception::InstructionAccessFault(pc2))? as u32;
        // The block cache stays page-local: a straddling instruction is
        // uncommon and already needs a second, correctness-sensitive
        // translation, so it runs through the ordinary miss path.
        let cache_page = if pc >> 12 == pc2 >> 12 {
            match (ram_page_generation(bus, pa), ram_page_generation(bus, pa2)) {
                (Some(first), Some(second)) if first == second => Some(first),
                _ => None,
            }
        } else {
            None
        };
        Ok(FetchedInstruction {
            raw: low | (high << 16),
            cache_page,
        })
    }

    /// Cumulative decoded-block cache hit/miss counts. Flushes leave the
    /// counters intact.
    pub fn decode_cache_stats(&self) -> DecodeCacheStats {
        self.decode_cache.stats()
    }

    pub(crate) fn flush_decode_cache(&mut self) {
        self.decode_cache.flush();
    }

    /// Virtual-address load. Misaligned accesses that stay in one page are done
    /// physically; page-crossing misaligned accesses fall back to bytes.
    #[inline]
    pub fn load(&mut self, bus: &mut Bus, va: u64, size: u8) -> Result<u64, Exception> {
        let mode = self.data_mode();
        if size > 1 && (va & (size as u64 - 1)) != 0 && (va & 0xFFF) + size as u64 > 0x1000 {
            return self.load_cross_page(bus, va, size, mode);
        }
        let pa = self.translate(bus, va, AccessType::Load, mode)?;
        bus.read(pa, size)
            .map_err(|_| Exception::LoadAccessFault(va))
    }

    #[inline]
    pub fn store(&mut self, bus: &mut Bus, va: u64, val: u64, size: u8) -> Result<(), Exception> {
        let mode = self.data_mode();
        // Any store invalidates the LR reservation (conservative but correct).
        self.reservation = None;
        if size > 1 && (va & (size as u64 - 1)) != 0 && (va & 0xFFF) + size as u64 > 0x1000 {
            return self.store_cross_page(bus, va, val, size, mode);
        }
        let pa = self.translate(bus, va, AccessType::Store, mode)?;
        bus.write(pa, val, size)
            .map_err(|_| Exception::StoreAccessFault(va))
    }

    #[cold]
    fn load_cross_page(
        &mut self,
        bus: &mut Bus,
        va: u64,
        size: u8,
        mode: Mode,
    ) -> Result<u64, Exception> {
        // Resolve every byte's physical address, taking any fault, before
        // touching the bus: a fault on a later page must not pop device
        // state (a UART RBR read, say) from an earlier one.
        let mut pas = [0u64; 8];
        let count = usize::from(size);
        if count > pas.len() {
            return Err(Exception::LoadAccessFault(va));
        }
        for (i, slot) in pas[..count].iter_mut().enumerate() {
            let byte_va = va
                .checked_add(i as u64)
                .ok_or(Exception::LoadPageFault(va))?;
            *slot = self.translate(bus, byte_va, AccessType::Load, mode)?;
        }
        let mut v: u64 = 0;
        for (i, &pa) in pas[..count].iter().enumerate() {
            let b = bus
                .read(pa, 1)
                .map_err(|_| Exception::LoadAccessFault(va))?;
            v |= b << (i * 8);
        }
        Ok(v)
    }

    #[cold]
    fn store_cross_page(
        &mut self,
        bus: &mut Bus,
        va: u64,
        val: u64,
        size: u8,
        mode: Mode,
    ) -> Result<(), Exception> {
        // A store must not partially execute when it traps: translate
        // every spanned page first, then commit. Otherwise a fault on the
        // second page leaves bytes written to the first, and re-execution
        // duplicates their MMIO side effects.
        let mut pas = [0u64; 8];
        let count = usize::from(size);
        if count > pas.len() {
            return Err(Exception::StoreAccessFault(va));
        }
        for (i, slot) in pas[..count].iter_mut().enumerate() {
            let byte_va = va
                .checked_add(i as u64)
                .ok_or(Exception::StorePageFault(va))?;
            *slot = self.translate(bus, byte_va, AccessType::Store, mode)?;
        }
        if pas[..count].iter().any(|&pa| !bus.write_accessible(pa, 1)) {
            return Err(Exception::StoreAccessFault(va));
        }
        for (i, &pa) in pas[..count].iter().enumerate() {
            bus.write(pa, (val >> (i * 8)) & 0xFF, 1)
                .map_err(|_| Exception::StoreAccessFault(va))?;
        }
        Ok(())
    }

    /// Translate for an AMO/LR/SC. This hart reports access faults for
    /// misaligned atomics, as declared by its architectural profile.
    pub(crate) fn translate_amo(
        &mut self,
        bus: &mut Bus,
        va: u64,
        size: u8,
        is_store: bool,
    ) -> Result<u64, Exception> {
        if va & (size as u64 - 1) != 0 {
            return Err(if is_store {
                Exception::StoreAccessFault(va)
            } else {
                Exception::LoadAccessFault(va)
            });
        }
        let mode = self.data_mode();
        let acc = if is_store {
            AccessType::Store
        } else {
            AccessType::Load
        };
        self.translate(bus, va, acc, mode)
    }

    /// One instruction. Interrupt check happens in the machine loop.
    #[inline(always)]
    pub fn step(&mut self, bus: &mut Bus) -> StepResult {
        if self.wfi {
            // Wake when any enabled interrupt is pending (global masks ignored).
            if self.csr.load_raw(csr::MIP) & self.csr.load_raw(csr::MIE) != 0 {
                self.wfi = false;
            } else {
                return StepResult::WaitingForInterrupt;
            }
        }

        if let Err(error) = self.step_inner(bus) {
            self.take_exception(error);
        }
        self.regs[0] = 0;
        if self.csr.load_raw(csr::MCOUNTINHIBIT) & 1 == 0 {
            self.csr.cycle = self.csr.cycle.wrapping_add(1);
        }
        StepResult::Executed
    }

    fn step_inner(&mut self, bus: &mut Bus) -> Result<(), Exception> {
        let instruction_pc = self.pc;
        // Continuing the block the last instruction fell through from needs no
        // translation; see `decode_cache::ActiveLookup`.
        let (active_hit, append) = match self.decode_cache.lookup_active(instruction_pc, &bus.ram) {
            decode_cache::ActiveLookup::Hit(location) => (Some(location), None),
            decode_cache::ActiveLookup::Append { slot, index } => (None, Some((slot, index))),
            decode_cache::ActiveLookup::None => (None, None),
        };
        let (raw, inst, cache_location) = match active_hit {
            Some(location) => (
                location.instruction.raw,
                location.instruction.decoded,
                Some(location),
            ),
            None => {
                if instruction_pc & 1 != 0 {
                    return Err(Exception::InstructionAddressMisaligned(instruction_pc));
                }
                // Translating first is what makes a physically keyed block safe
                // to reuse across address spaces: an instruction page fault or
                // access fault that a fresh fetch would raise is raised here,
                // before any cached block can be reached.
                let pa = self.translate_fetch(bus, instruction_pc)?;
                let block_hit = if append.is_none() {
                    match self.decode_cache.lookup_physical(pa, &bus.ram) {
                        decode_cache::Lookup::Hit(location) => Some(location),
                        decode_cache::Lookup::Miss => None,
                    }
                } else {
                    None
                };
                match block_hit {
                    Some(location) => (
                        location.instruction.raw,
                        location.instruction.decoded,
                        Some(location),
                    ),
                    None => {
                        let fetched = self.fetch_window(bus, instruction_pc, pa)?;
                        let decoded = decode::decode(fetched.raw)
                            .map_err(|_| Exception::IllegalInstruction(fetched.raw as u64))?;
                        let cached = decode_cache::CachedInstruction {
                            pa,
                            raw: fetched.raw,
                            decoded,
                        };
                        let location = fetched.cache_page.map(|(page, generation)| {
                            self.decode_cache
                                .insert(pa, page, generation, cached, append)
                        });
                        (fetched.raw, decoded, location)
                    }
                }
            }
        };
        let writes_minstret = matches!(
            inst.op,
            decode::Op::Csr {
                kind: decode::CsrOp::Rw | decode::CsrOp::Rwi,
                csr: csr::MINSTRET,
                ..
            } | decode::Op::Csr {
                kind: decode::CsrOp::Rs
                    | decode::CsrOp::Rsi
                    | decode::CsrOp::Rc
                    | decode::CsrOp::Rci,
                rs1: 1..,
                csr: csr::MINSTRET,
                ..
            }
        );
        self.next_pc = self.pc.wrapping_add(inst.len);
        execute::execute(self, bus, raw, &inst.op)?;
        self.pc = self.next_pc;
        let instret_inhibited = self.csr.load_raw(csr::MCOUNTINHIBIT) & (1 << 2) != 0;
        if !writes_minstret && !instret_inhibited {
            self.csr.instret = self.csr.instret.wrapping_add(1);
        }
        if let Some(location) = cache_location {
            self.decode_cache.finish_instruction(
                location,
                instruction_pc,
                instruction_pc.wrapping_add(inst.len),
                self.pc,
                &bus.ram,
            );
        }
        Ok(())
    }

    /// Highest-priority pending+enabled interrupt, honoring delegation and
    /// per-mode global enables. Priority: MEI, MSI, MTI, SEI, SSI, STI.
    pub(crate) fn pending_interrupt(&self) -> Option<Interrupt> {
        let pending = self.csr.load_raw(csr::MIP) & self.csr.load_raw(csr::MIE);
        if pending == 0 {
            return None;
        }
        let mideleg = self.csr.load_raw(csr::MIDELEG);

        let m_enabled = self.mode != Mode::Machine || self.csr.mstatus_field(mstatus::MIE);
        let m_pending = pending & !mideleg;
        if m_enabled && m_pending != 0 {
            for (bit, i) in [
                (irq::MEIP, Interrupt::MachineExternal),
                (irq::MSIP, Interrupt::MachineSoftware),
                (irq::MTIP, Interrupt::MachineTimer),
                (irq::SEIP, Interrupt::SupervisorExternal),
                (irq::SSIP, Interrupt::SupervisorSoftware),
                (irq::STIP, Interrupt::SupervisorTimer),
            ] {
                if m_pending & bit != 0 {
                    return Some(i);
                }
            }
        }

        let s_enabled = self.mode == Mode::User
            || (self.mode == Mode::Supervisor && self.csr.mstatus_field(mstatus::SIE));
        let s_pending = pending & mideleg;
        if s_enabled && s_pending != 0 {
            for (bit, i) in [
                (irq::SEIP, Interrupt::SupervisorExternal),
                (irq::SSIP, Interrupt::SupervisorSoftware),
                (irq::STIP, Interrupt::SupervisorTimer),
            ] {
                if s_pending & bit != 0 {
                    return Some(i);
                }
            }
        }
        None
    }

    pub(crate) fn take_exception(&mut self, e: Exception) {
        self.reservation = None;
        let cause = e.cause();
        let deleg = self.csr.load_raw(csr::MEDELEG);
        let to_s = self.mode != Mode::Machine && (deleg >> cause) & 1 == 1;
        self.enter_trap(cause, e.tval(), self.pc, to_s, false);
    }

    pub(crate) fn take_interrupt(&mut self, i: Interrupt) {
        self.wfi = false;
        let deleg = self.csr.load_raw(csr::MIDELEG);
        let to_s = self.mode != Mode::Machine && (deleg & i.bit()) != 0;
        self.enter_trap(i.cause(), 0, self.pc, to_s, true);
    }

    fn enter_trap(&mut self, cause: u64, tval: u64, epc: u64, to_s: bool, is_interrupt: bool) {
        let status = self.csr.load_raw(csr::MSTATUS);
        if to_s {
            self.csr.store_raw(csr::SEPC, epc);
            self.csr.store_raw(csr::SCAUSE, cause);
            self.csr.store_raw(csr::STVAL, tval);
            // SPIE = SIE; SIE = 0; SPP = current mode.
            let sie = status & mstatus::SIE != 0;
            let mut s = status & !(mstatus::SPIE | mstatus::SIE | mstatus::SPP);
            if sie {
                s |= mstatus::SPIE;
            }
            if self.mode == Mode::Supervisor {
                s |= mstatus::SPP;
            }
            self.force_mstatus(s);
            self.mode = Mode::Supervisor;
            self.pc = self.tvec_target(self.csr.load_raw(csr::STVEC), cause, is_interrupt);
        } else {
            self.csr.store_raw(csr::MEPC, epc);
            self.csr.store_raw(csr::MCAUSE, cause);
            self.csr.store_raw(csr::MTVAL, tval);
            let mie = status & mstatus::MIE != 0;
            let mut s = status & !(mstatus::MPIE | mstatus::MIE | mstatus::MPP_MASK);
            if mie {
                s |= mstatus::MPIE;
            }
            s |= (self.mode as u64) << mstatus::MPP_SHIFT;
            self.force_mstatus(s);
            self.mode = Mode::Machine;
            self.pc = self.tvec_target(self.csr.load_raw(csr::MTVEC), cause, is_interrupt);
        }
    }

    fn tvec_target(&self, tvec: u64, cause: u64, is_interrupt: bool) -> u64 {
        let base = tvec & !3;
        if tvec & 3 == 1 && is_interrupt {
            base.wrapping_add(4u64.wrapping_mul(cause & !(1 << 63)))
        } else {
            base
        }
    }

    /// Write mstatus bypassing the CSR-instruction write mask (trap machinery
    /// owns fields like MPP/SPP unconditionally).
    fn force_mstatus(&mut self, val: u64) {
        self.csr.store_raw(csr::MSTATUS, val);
        // Only real modes are stored, so MPP is never 2 and the WARL coercion
        // in mstatus_write is harmless.
    }

    pub(crate) fn sret(&mut self) -> Result<(), Exception> {
        if self.mode == Mode::User
            || (self.mode == Mode::Supervisor && self.csr.mstatus_field(mstatus::TSR))
        {
            return Err(Exception::IllegalInstruction(0));
        }
        let status = self.csr.load_raw(csr::MSTATUS);
        let spie = status & mstatus::SPIE != 0;
        let spp_s = status & mstatus::SPP != 0;
        let mut s = status & !(mstatus::SIE | mstatus::SPP);
        if spie {
            s |= mstatus::SIE;
        }
        s |= mstatus::SPIE;
        let new_mode = if spp_s { Mode::Supervisor } else { Mode::User };
        if new_mode != Mode::Machine {
            s &= !mstatus::MPRV;
        }
        self.force_mstatus(s);
        self.mode = new_mode;
        self.next_pc = self.csr.load_raw(csr::SEPC);
        Ok(())
    }

    pub(crate) fn mret(&mut self) -> Result<(), Exception> {
        if self.mode != Mode::Machine {
            return Err(Exception::IllegalInstruction(0));
        }
        let status = self.csr.load_raw(csr::MSTATUS);
        let mpie = status & mstatus::MPIE != 0;
        let mpp = (status & mstatus::MPP_MASK) >> mstatus::MPP_SHIFT;
        let mut s = status & !(mstatus::MIE | mstatus::MPP_MASK);
        if mpie {
            s |= mstatus::MIE;
        }
        s |= mstatus::MPIE;
        let new_mode = Mode::from_bits(mpp);
        if new_mode != Mode::Machine {
            s &= !mstatus::MPRV;
        }
        self.force_mstatus(s);
        self.mode = new_mode;
        self.next_pc = self.csr.load_raw(csr::MEPC);
        Ok(())
    }
}

#[inline]
fn ram_page_generation(bus: &Bus, pa: u64) -> Option<(u64, u64)> {
    pa.checked_sub(DRAM_BASE)
        .and_then(|offset| bus.ram.page_generation(offset))
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// State snapshot
// ---------------------------------------------------------------------------

const SNAPSHOT_VERSION: u16 = 1;

impl Cpu {
    /// Drop every translation and decode cache.
    ///
    /// These are memos over guest RAM and the page tables, which a correct
    /// hart may discard at any instruction boundary; a restore rebuilds them
    /// from the restored memory rather than carrying stale entries across.
    pub fn invalidate_caches(&mut self) {
        self.mmu.flush_all();
        self.decode_cache.flush();
        self.fetch_translation = None;
    }

    pub(crate) fn snapshot(&self, out: &mut crate::snapshot::Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            for value in self.regs {
                w.u64(value);
            }
            for value in self.fregs {
                w.u64(value);
            }
            w.u64(self.pc);
            w.u64(self.next_pc);
            w.u8(self.mode as u8);
            match self.reservation {
                Some(address) => {
                    w.bool(true);
                    w.u64(address);
                }
                None => {
                    w.bool(false);
                    w.u64(0);
                }
            }
            w.bool(self.wfi);
        });
        self.csr.snapshot(out);
    }

    pub(crate) fn restore(
        &mut self,
        input: &mut crate::snapshot::Reader<'_>,
    ) -> Result<(), String> {
        input.section("cpu", SNAPSHOT_VERSION, |r| {
            for slot in self.regs.iter_mut() {
                *slot = r.u64()?;
            }
            for slot in self.fregs.iter_mut() {
                *slot = r.u64()?;
            }
            self.pc = r.u64()?;
            self.next_pc = r.u64()?;
            let mode = r.u8()?;
            self.mode = match mode {
                0 => Mode::User,
                1 => Mode::Supervisor,
                3 => Mode::Machine,
                other => return Err(format!("{other} is not a privilege mode")),
            };
            let held = r.bool()?;
            let address = r.u64()?;
            self.reservation = held.then_some(address);
            self.wfi = r.bool()?;
            Ok(())
        })?;
        self.csr.restore(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::DRAM_BASE;

    fn write_instruction(bus: &mut Bus, offset: u64, raw: u32) {
        bus.ram.load_blob(offset, &raw.to_le_bytes()).unwrap();
    }

    fn map_last_sv39_page(cpu: &mut Cpu, bus: &mut Bus) {
        const ROOT: u64 = DRAM_BASE + 0x1000;
        const L1: u64 = DRAM_BASE + 0x2000;
        const L0: u64 = DRAM_BASE + 0x3000;
        const FRAME: u64 = DRAM_BASE + 0x4000;
        const PTE_V: u64 = 1 << 0;
        const PTE_R: u64 = 1 << 1;
        const PTE_W: u64 = 1 << 2;
        const PTE_A: u64 = 1 << 6;
        const PTE_D: u64 = 1 << 7;

        let pte = |pa: u64, flags: u64| ((pa >> 12) << 10) | flags;
        bus.write(ROOT + 511 * 8, pte(L1, PTE_V), 8).unwrap();
        bus.write(L1 + 511 * 8, pte(L0, PTE_V), 8).unwrap();
        bus.write(
            L0 + 511 * 8,
            pte(FRAME, PTE_V | PTE_R | PTE_W | PTE_A | PTE_D),
            8,
        )
        .unwrap();
        cpu.csr.store_raw(csr::SATP, (8 << 60) | (ROOT >> 12));
        cpu.mode = Mode::Supervisor;
    }

    #[test]
    fn page_crossing_load_at_address_space_end_faults_instead_of_wrapping() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x6000);
        map_last_sv39_page(&mut cpu, &mut bus);
        let va = u64::MAX - 3;

        let result = cpu.load(&mut bus, va, 8);

        assert_eq!(result, Err(Exception::LoadPageFault(va)));
    }

    #[test]
    fn page_crossing_store_at_address_space_end_faults_instead_of_wrapping() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x6000);
        map_last_sv39_page(&mut cpu, &mut bus);
        let va = u64::MAX - 3;
        let frame_tail = 0x4ffc;
        bus.ram.load_blob(frame_tail, &[0xaa; 4]).unwrap();

        let result = cpu.store(&mut bus, va, 0x1122_3344_5566_7788, 8);

        assert_eq!(
            (result, bus.ram.read(frame_tail, 4).unwrap()),
            (Err(Exception::StorePageFault(va)), 0xaaaa_aaaa)
        );
    }

    #[test]
    fn from_bits_maps_reserved_encoding_two_to_user() {
        // Reserved MPP/SPP encoding 2 must fail closed to User, never Machine.
        assert_eq!(Mode::from_bits(0), Mode::User);
        assert_eq!(Mode::from_bits(1), Mode::Supervisor);
        assert_eq!(Mode::from_bits(2), Mode::User);
        assert_eq!(Mode::from_bits(3), Mode::Machine);
    }

    /// Map a single Sv39 4-KiB page at VA `0x1000` (vpn0 = 1) to FRAME with A/D
    /// preset; the next page (VA `0x2000`) is deliberately left unmapped.
    fn map_one_page_next_unmapped(cpu: &mut Cpu, bus: &mut Bus) -> u64 {
        const ROOT: u64 = DRAM_BASE + 0x1000;
        const L1: u64 = DRAM_BASE + 0x2000;
        const L0: u64 = DRAM_BASE + 0x3000;
        const FRAME: u64 = DRAM_BASE + 0x4000;
        const PTE_V: u64 = 1 << 0;
        const PTE_R: u64 = 1 << 1;
        const PTE_W: u64 = 1 << 2;
        const PTE_A: u64 = 1 << 6;
        const PTE_D: u64 = 1 << 7;

        let pte = |pa: u64, flags: u64| ((pa >> 12) << 10) | flags;
        bus.write(ROOT, pte(L1, PTE_V), 8).unwrap();
        bus.write(L1, pte(L0, PTE_V), 8).unwrap();
        bus.write(L0 + 8, pte(FRAME, PTE_V | PTE_R | PTE_W | PTE_A | PTE_D), 8)
            .unwrap();
        cpu.csr.store_raw(csr::SATP, (8 << 60) | (ROOT >> 12));
        cpu.mode = Mode::Supervisor;
        FRAME
    }

    #[test]
    fn page_crossing_store_faulting_on_second_page_leaves_first_page_untouched() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x6000);
        let frame = map_one_page_next_unmapped(&mut cpu, &mut bus);
        // va straddles the mapped page (0x1000..0x2000) into the unmapped one.
        let va = 0x2000u64 - 2;

        let result = cpu.store(&mut bus, va, u64::MAX, 8);

        assert_eq!(result, Err(Exception::StorePageFault(0x2000)));
        // The prepass must have faulted before committing any first-page bytes.
        assert_eq!(bus.read(frame + 0xFFE, 2).unwrap(), 0);
    }

    #[test]
    fn vectored_trap_target_wraps_at_xlen() {
        let cpu = Cpu::new();

        let target = cpu.tvec_target(u64::MAX - 2, 1, true);

        assert_eq!(target, 0);
    }

    #[test]
    fn decode_cache_lazily_builds_and_reuses_a_fallthrough_block() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(8192);
        cpu.pc = DRAM_BASE;
        // addi x1,x1,1; addi x2,x2,1; addi x3,x3,1
        for (index, raw) in [0x0010_8093, 0x0011_0113, 0x0011_8193]
            .into_iter()
            .enumerate()
        {
            write_instruction(&mut bus, index as u64 * 4, raw);
        }

        for _ in 0..3 {
            assert!(matches!(cpu.step(&mut bus), StepResult::Executed));
        }
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 0, misses: 3 }
        );

        cpu.pc = DRAM_BASE;
        for _ in 0..3 {
            assert!(matches!(cpu.step(&mut bus), StepResult::Executed));
        }
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 3, misses: 3 }
        );
        assert_eq!((cpu.reg(1), cpu.reg(2), cpu.reg(3)), (2, 2, 2));
    }

    #[test]
    fn decode_cache_keeps_four_same_set_blocks_resident() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(16 * 1024);
        for offset in [0, 4096, 8192, 12288] {
            write_instruction(&mut bus, offset, 0x0010_8093);
        }

        for offset in [0, 4096, 8192, 12288, 0, 4096, 8192, 12288] {
            cpu.pc = DRAM_BASE + offset;
            cpu.step(&mut bus);
        }

        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 4, misses: 4 }
        );
    }

    #[test]
    fn generation_invalidation_preserves_a_same_set_neighbor() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(8192);
        write_instruction(&mut bus, 0, 0x0010_8093);
        write_instruction(&mut bus, 4096, 0x0010_8093);

        for offset in [0, 4096] {
            cpu.pc = DRAM_BASE + offset;
            cpu.step(&mut bus);
        }
        write_instruction(&mut bus, 0, 0x0020_8093);
        for offset in [0, 4096] {
            cpu.pc = DRAM_BASE + offset;
            cpu.step(&mut bus);
        }

        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 1, misses: 3 }
        );
    }

    #[test]
    fn interrupt_between_cached_instructions_abandons_active_fallthrough() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(8192);
        cpu.pc = DRAM_BASE;
        for (index, raw) in [0x0010_8093, 0x0011_0113, 0x0011_8193]
            .into_iter()
            .enumerate()
        {
            write_instruction(&mut bus, index as u64 * 4, raw);
        }

        // Warm the three-instruction block, then enter it again.
        for _ in 0..3 {
            cpu.step(&mut bus);
        }
        cpu.regs[1..=3].fill(0);
        cpu.pc = DRAM_BASE;
        cpu.step(&mut bus);
        assert_eq!(cpu.reg(1), 1);

        // Redirect to the third instruction between Cpu::step calls, exactly
        // as Machine::run does when it accepts an interrupt.
        cpu.csr.store_raw(csr::MTVEC, DRAM_BASE + 8);
        cpu.take_interrupt(Interrupt::MachineTimer);
        cpu.step(&mut bus);

        assert_eq!(cpu.reg(2), 0, "stale active fallthrough executed");
        assert_eq!(cpu.reg(3), 1, "trap-vector instruction did not execute");
    }

    #[test]
    fn ram_write_generation_invalidates_cached_code_without_fence_i() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(8);
        cpu.pc = DRAM_BASE;
        write_instruction(&mut bus, 0, 0x0010_0093); // addi x1,x0,1

        cpu.step(&mut bus);
        assert_eq!(cpu.reg(1), 1);

        write_instruction(&mut bus, 0, 0x0020_0093); // addi x1,x0,2
        cpu.pc = DRAM_BASE;
        cpu.step(&mut bus);
        assert_eq!(cpu.reg(1), 2);
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 0, misses: 2 }
        );
    }

    #[test]
    fn fence_i_flushes_decoded_blocks() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(8);
        cpu.pc = DRAM_BASE;
        write_instruction(&mut bus, 0, 0x0000_100f); // fence.i

        cpu.step(&mut bus);
        cpu.pc = DRAM_BASE;
        cpu.step(&mut bus);

        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 0, misses: 2 }
        );
    }

    #[test]
    fn satp_write_and_sfence_vma_preserve_machine_decoded_blocks() {
        for raw in [0x1800_1073, 0x1200_0073] {
            let mut cpu = Cpu::new();
            let mut bus = Bus::new(8);
            cpu.pc = DRAM_BASE;
            write_instruction(&mut bus, 0, raw);

            cpu.step(&mut bus);
            cpu.pc = DRAM_BASE;
            cpu.step(&mut bus);

            assert_eq!(
                cpu.decode_cache_stats(),
                DecodeCacheStats { hits: 1, misses: 1 },
                "instruction {raw:#010x} discarded an untranslated M-mode block"
            );
        }
    }

    #[test]
    fn decode_cache_shares_one_physical_block_across_privilege_modes() {
        // Decoding depends on nothing but the instruction bits, so the same
        // physical address decodes identically in every mode. With bare
        // translation, M-mode and S-mode fetch the same PA and share a block.
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(8);
        cpu.pc = DRAM_BASE;
        write_instruction(&mut bus, 0, 0x0010_8093); // addi x1,x1,1

        cpu.step(&mut bus);
        cpu.pc = DRAM_BASE;
        cpu.mode = Mode::Supervisor;
        cpu.step(&mut bus);
        cpu.pc = DRAM_BASE;
        cpu.mode = Mode::Machine;
        cpu.step(&mut bus);

        assert_eq!(cpu.reg(1), 3);
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 2, misses: 1 }
        );
    }

    // --- Physically keyed block cache -----------------------------------
    //
    // Sv39 scratch space. Two independent root tables let one VA name two
    // different physical pages, and one physical page be named by two VAs.

    const PTE_V: u64 = 1 << 0;
    const PTE_R: u64 = 1 << 1;
    const PTE_W: u64 = 1 << 2;
    const PTE_X: u64 = 1 << 3;
    const PTE_U: u64 = 1 << 4;
    const PTE_A: u64 = 1 << 6;
    const PTE_D: u64 = 1 << 7;

    /// Root/intermediate tables of address space A and B, and two code frames.
    const ROOT_A: u64 = DRAM_BASE + 0x1000;
    const ROOT_B: u64 = DRAM_BASE + 0x4000;
    const CODE_X: u64 = DRAM_BASE + 0x7000;
    const CODE_Y: u64 = DRAM_BASE + 0x8000;
    /// Virtual address both spaces map, and a second VA aliasing it in A.
    const VA: u64 = 0x9000;
    const VA_ALIAS: u64 = 0xa000;

    fn pte(pa: u64, flags: u64) -> u64 {
        ((pa >> 12) << 10) | flags
    }

    /// Map `va` (a 4-KiB page below 2 MiB) to `frame` in the space rooted at
    /// `root`, whose L1/L0 tables are the two pages following it.
    fn map_leaf(bus: &mut Bus, root: u64, va: u64, frame: u64, flags: u64) {
        let (l1, l0) = (root + 0x1000, root + 0x2000);
        bus.write(root + ((va >> 30) & 0x1ff) * 8, pte(l1, PTE_V), 8)
            .unwrap();
        bus.write(l1 + ((va >> 21) & 0x1ff) * 8, pte(l0, PTE_V), 8)
            .unwrap();
        bus.write(
            l0 + ((va >> 12) & 0x1ff) * 8,
            pte(frame, flags | PTE_V | PTE_A | PTE_D),
            8,
        )
        .unwrap();
    }

    fn activate(cpu: &mut Cpu, root: u64) {
        cpu.csr.store_raw(csr::SATP, (8 << 60) | (root >> 12));
        // Mirrors what the CSR write path does architecturally: the TLB is
        // flushed, the decoded blocks are not.
        cpu.mmu.flush_all();
        cpu.mode = Mode::Supervisor;
    }

    #[test]
    fn same_va_in_two_address_spaces_runs_the_right_physical_code() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x10000);
        write_instruction(&mut bus, CODE_X - DRAM_BASE, 0x0010_8093); // addi x1,x1,1
        write_instruction(&mut bus, CODE_Y - DRAM_BASE, 0x0011_0113); // addi x2,x2,1
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R | PTE_X);
        map_leaf(&mut bus, ROOT_B, VA, CODE_Y, PTE_R | PTE_X);

        activate(&mut cpu, ROOT_A);
        cpu.pc = VA;
        cpu.step(&mut bus);
        activate(&mut cpu, ROOT_B);
        cpu.pc = VA;
        cpu.step(&mut bus);

        // No aliasing: space B must not have run space A's cached block.
        assert_eq!((cpu.reg(1), cpu.reg(2)), (1, 1));
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 0, misses: 2 }
        );
    }

    #[test]
    fn two_virtual_aliases_of_one_physical_page_share_a_block() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x10000);
        write_instruction(&mut bus, CODE_X - DRAM_BASE, 0x0010_8093); // addi x1,x1,1
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R | PTE_X);
        map_leaf(&mut bus, ROOT_A, VA_ALIAS, CODE_X, PTE_R | PTE_X);

        activate(&mut cpu, ROOT_A);
        cpu.pc = VA;
        cpu.step(&mut bus);
        cpu.pc = VA_ALIAS;
        cpu.step(&mut bus);

        assert_eq!(cpu.reg(1), 2);
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 1, misses: 1 }
        );
    }

    #[test]
    fn store_through_one_alias_invalidates_the_block_seen_through_the_other() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x10000);
        write_instruction(&mut bus, CODE_X - DRAM_BASE, 0x0010_8093); // addi x1,x1,1
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R | PTE_X);
        map_leaf(&mut bus, ROOT_A, VA_ALIAS, CODE_X, PTE_R | PTE_W | PTE_X);

        activate(&mut cpu, ROOT_A);
        cpu.pc = VA;
        cpu.step(&mut bus);
        assert_eq!(cpu.reg(1), 1);

        // addi x1,x1,2 written through the writable alias, with no FENCE.I.
        cpu.store(&mut bus, VA_ALIAS, 0x0020_8093, 4).unwrap();
        cpu.pc = VA;
        cpu.step(&mut bus);

        assert_eq!(cpu.reg(1), 3);
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 0, misses: 2 }
        );
    }

    #[test]
    fn cached_block_still_faults_when_its_page_becomes_non_executable() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x10000);
        write_instruction(&mut bus, CODE_X - DRAM_BASE, 0x0010_8093);
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R | PTE_X);

        activate(&mut cpu, ROOT_A);
        cpu.pc = VA;
        cpu.step(&mut bus);
        assert_eq!(cpu.reg(1), 1);

        // Drop X from the leaf PTE; the block stays cached at its PA.
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R);
        cpu.mmu.flush_all();
        cpu.pc = VA;

        assert_eq!(
            cpu.step_inner(&mut bus),
            Err(Exception::InstructionPageFault(VA))
        );
        assert_eq!(cpu.reg(1), 1, "a cached block bypassed a fetch page fault");
    }

    #[test]
    fn cached_supervisor_block_still_faults_when_fetched_from_user_mode() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x10000);
        write_instruction(&mut bus, CODE_X - DRAM_BASE, 0x0010_8093);
        // Executable, but not a user page.
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R | PTE_X);

        activate(&mut cpu, ROOT_A);
        cpu.pc = VA;
        cpu.step(&mut bus);
        assert_eq!(cpu.reg(1), 1);

        cpu.mode = Mode::User;
        cpu.pc = VA;

        assert_eq!(
            cpu.step_inner(&mut bus),
            Err(Exception::InstructionPageFault(VA))
        );
        assert_eq!(cpu.reg(1), 1, "U-mode ran a cached supervisor-page block");
    }

    #[test]
    fn cached_user_block_still_faults_when_fetched_from_supervisor_mode() {
        // The mirror of the case above: S-mode may never fetch a U page, so a
        // block warmed in U-mode must not be reachable from S-mode either.
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x10000);
        write_instruction(&mut bus, CODE_X - DRAM_BASE, 0x0010_8093);
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R | PTE_X | PTE_U);

        activate(&mut cpu, ROOT_A);
        cpu.mode = Mode::User;
        cpu.pc = VA;
        cpu.step(&mut bus);
        assert_eq!(cpu.reg(1), 1);

        cpu.mode = Mode::Supervisor;
        cpu.pc = VA;

        assert_eq!(
            cpu.step_inner(&mut bus),
            Err(Exception::InstructionPageFault(VA))
        );
        assert_eq!(cpu.reg(1), 1, "S-mode ran a cached user-page block");
    }

    #[test]
    fn sfence_vma_preserves_translated_decoded_blocks() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x10000);
        let code = CODE_X - DRAM_BASE;
        write_instruction(&mut bus, code, 0x0010_8093); // addi x1,x1,1
        write_instruction(&mut bus, code + 4, 0x1200_0073); // sfence.vma
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R | PTE_X);

        activate(&mut cpu, ROOT_A);
        cpu.pc = VA;
        cpu.step(&mut bus); // miss: block start
        cpu.step(&mut bus); // miss: appends sfence.vma, which flushes the TLB
        cpu.pc = VA;
        cpu.step(&mut bus); // hit: same physical block start
        cpu.step(&mut bus); // hit: active fallthrough

        assert_eq!(cpu.reg(1), 2);
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 2, misses: 2 }
        );
    }

    #[test]
    fn satp_write_preserves_translated_decoded_blocks() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x10000);
        let code = CODE_X - DRAM_BASE;
        write_instruction(&mut bus, code, 0x0010_8093); // addi x1,x1,1
        write_instruction(&mut bus, code + 4, 0x1805_1073); // csrw satp,a0
                                                            // Both spaces map VA to the same physical code page.
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R | PTE_X);
        map_leaf(&mut bus, ROOT_B, VA, CODE_X, PTE_R | PTE_X);

        activate(&mut cpu, ROOT_A);
        cpu.set_reg(10, (8 << 60) | (ROOT_B >> 12));
        cpu.pc = VA;
        cpu.step(&mut bus); // miss: block start
        cpu.step(&mut bus); // miss: appends csrw satp, switching address space
        assert_eq!(cpu.csr.load_raw(csr::SATP), (8 << 60) | (ROOT_B >> 12));
        cpu.pc = VA;
        cpu.step(&mut bus); // hit: the switch invalidated no block
        cpu.step(&mut bus); // hit

        assert_eq!(cpu.reg(1), 2);
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 2, misses: 2 }
        );
    }

    #[test]
    fn satp_write_ends_the_active_block_it_sits_in() {
        // The instructions after `csrw satp` must be fetched through the new
        // address space, not continued out of the old space's physical block.
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(0x10000);
        write_instruction(&mut bus, CODE_X - DRAM_BASE, 0x1805_1073); // csrw satp,a0
        write_instruction(&mut bus, CODE_X - DRAM_BASE + 4, 0x0010_8093); // addi x1,x1,1
        write_instruction(&mut bus, CODE_Y - DRAM_BASE + 4, 0x0011_0113); // addi x2,x2,1
        map_leaf(&mut bus, ROOT_A, VA, CODE_X, PTE_R | PTE_X);
        map_leaf(&mut bus, ROOT_B, VA, CODE_Y, PTE_R | PTE_X);

        activate(&mut cpu, ROOT_A);
        cpu.set_reg(10, (8 << 60) | (ROOT_B >> 12));
        cpu.pc = VA;
        cpu.step(&mut bus);
        cpu.step(&mut bus);

        assert_eq!(
            (cpu.reg(1), cpu.reg(2)),
            (0, 1),
            "ran the old address space's instruction after switching satp"
        );
    }

    #[test]
    fn page_straddling_instruction_uses_uncached_fetch_path() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(8192);
        let offset = 0xffe;
        cpu.pc = DRAM_BASE + offset;
        write_instruction(&mut bus, offset, 0x0010_8093); // addi x1,x1,1

        cpu.step(&mut bus);
        cpu.pc = DRAM_BASE + offset;
        cpu.step(&mut bus);

        assert_eq!(cpu.reg(1), 2);
        assert_eq!(
            cpu.decode_cache_stats(),
            DecodeCacheStats { hits: 0, misses: 2 }
        );
    }

    #[test]
    fn step_suppresses_instret_increment_when_instruction_writes_minstret() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(8);
        cpu.pc = DRAM_BASE;
        // csrwi minstret, 0; csrr a0, minstret
        bus.ram.load_blob(0, &0xB020_5073u32.to_le_bytes()).unwrap();
        bus.ram.load_blob(4, &0xB020_2573u32.to_le_bytes()).unwrap();

        cpu.step(&mut bus);
        cpu.step(&mut bus);

        assert_eq!((cpu.reg(10), cpu.csr.instret), (0, 1));
    }

    #[test]
    fn step_does_not_increment_instret_when_mcountinhibit_ir_is_set() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(4);
        cpu.pc = DRAM_BASE;
        cpu.csr.store_raw(csr::MCOUNTINHIBIT, 1 << 2);
        cpu.csr.instret = 7;
        // addi x0, x0, 0
        bus.ram.load_blob(0, &0x0000_0013u32.to_le_bytes()).unwrap();

        cpu.step(&mut bus);

        assert_eq!(cpu.csr.instret, 7);
    }

    #[test]
    fn step_does_not_increment_cycle_when_mcountinhibit_cy_is_set() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new(4);
        cpu.pc = DRAM_BASE;
        bus.ram.load_blob(0, &0x0000_0013u32.to_le_bytes()).unwrap();
        cpu.csr.store_raw(csr::MCOUNTINHIBIT, 1);
        cpu.csr.cycle = 7;

        cpu.step(&mut bus);

        assert_eq!(cpu.csr.cycle, 7);
    }
}
