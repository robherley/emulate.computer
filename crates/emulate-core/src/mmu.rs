//! Sv39 address translation with a TLB, per the privileged spec.
//!
//! Bare mode and Machine-mode accesses are identity-mapped; otherwise a
//! 3-level walk with leaf PTEs at any level (1 GB / 2 MB / 4 KB). A/D bits are
//! set in memory when `menvcfg.ADUE` is enabled, and page-fault when it is not.
//!
//! The TLB is direct-mapped by 4-KiB VPN (a superpage leaf is cached as the
//! 4-KiB frame it resolves to) and stores the leaf PTE flags, so permissions
//! are re-checked on every hit and SUM/MXR/privilege changes need no flush.
//! `flush*` is called on SFENCE.VMA and satp writes.

use crate::bus::Bus;
use crate::cpu::Mode;
use crate::trap::Exception;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    Fetch,
    Load,
    Store,
}

pub(crate) const TLB_ENTRIES: usize = 256;

// PTE flag bits.
const PTE_V: u64 = 1 << 0;
const PTE_R: u64 = 1 << 1;
const PTE_W: u64 = 1 << 2;
const PTE_X: u64 = 1 << 3;
const PTE_U: u64 = 1 << 4;
const PTE_A: u64 = 1 << 6;
const PTE_D: u64 = 1 << 7;

/// PTE ppn field: bits 53:10 (44 bits).
const PPN_MASK: u64 = 0xFFF_FFFF_FFFF;
/// Sv39 bits 63:54 are reserved when Svnapot and Svpbmt are not implemented.
const PTE_RESERVED_HIGH: u64 = 0x3FF << 54;
/// D, A, and U are reserved in non-leaf PTEs.
const PTE_RESERVED_NONLEAF: u64 = PTE_D | PTE_A | PTE_U;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TlbEntry {
    /// Exact-match tag: va >> 12 (full 4-KiB VPN, superpages are cached per
    /// 4-KiB frame). u64::MAX = invalid.
    pub(crate) vpn: u64,
    /// Physical base of the 4-KiB frame.
    pub(crate) pa_base: u64,
    /// Leaf PTE flag bits (V/R/W/X/U/G/A/D), with A/D as updated in memory.
    pub(crate) flags: u64,
}

/// Neither satp nor ASID is part of the TLB tag. That is only safe because the
/// CPU flushes on every satp write and SFENCE.VMA, so an entry never outlives
/// the page table it was walked under. (The *decoded block* cache is keyed on
/// physical addresses and deliberately survives both; only translation state
/// is discarded here.)
pub struct Mmu {
    pub(crate) tlb: [TlbEntry; TLB_ENTRIES],
    /// Bumped by every `flush_all`, so a caller that memoised a translation
    /// can retire it with a single comparison.
    generation: u64,
}

impl Mmu {
    pub fn new() -> Self {
        Mmu {
            tlb: [TlbEntry {
                vpn: u64::MAX,
                pa_base: 0,
                flags: 0,
            }; TLB_ENTRIES],
            generation: 0,
        }
    }

    /// Counter of TLB flushes. Two translations of the same VA taken at the
    /// same generation, in the same mode, are guaranteed to agree.
    #[inline]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub fn flush_all(&mut self) {
        for e in self.tlb.iter_mut() {
            e.vpn = u64::MAX;
        }
        self.generation = self.generation.wrapping_add(1);
    }

    /// Translate `va` for `acc` at effective privilege `mode`.
    ///
    /// `mode` is the effective privilege with MPRV already applied, so
    /// `Machine` here means "no translation".
    #[expect(
        clippy::too_many_arguments,
        reason = "the translation inputs (satp, mode, mxr, sum, adue) are CSR-derived \
                  and read once by the caller's fast path, so passing them explicitly \
                  avoids re-reading the CSR file on every translation"
    )]
    #[inline]
    pub fn translate(
        &mut self,
        bus: &mut Bus,
        satp: u64,
        mode: Mode,
        mxr: bool,
        sum: bool,
        adue: bool,
        acc: AccessType,
        va: u64,
    ) -> Result<u64, Exception> {
        if satp >> 60 != 8 || mode == Mode::Machine {
            return Ok(va);
        }

        // A non-canonical VA can never hit: entries are only filled from
        // canonical VAs and the tag is the full va >> 12. So the canonicality
        // check lives on the walk path only.
        let vpn = va >> 12;
        let e = &self.tlb[(vpn as usize) & (TLB_ENTRIES - 1)];
        if e.vpn == vpn {
            let flags = e.flags;
            if perm_ok(flags, mode, mxr, sum, acc) {
                // A store also needs the cached D bit; without it, re-walk so
                // D gets set in the in-memory PTE.
                if acc != AccessType::Store || flags & PTE_D != 0 {
                    return Ok(e.pa_base | (va & 0xFFF));
                }
            } else {
                // The entry came from a walk of the current page table, so a
                // re-walk would reach the same PTE and the same conclusion.
                return Err(page_fault(acc, va));
            }
        }

        self.walk(bus, satp, mode, mxr, sum, adue, acc, va)
    }

    /// Full Sv39 page-table walk; fills the TLB on success.
    #[expect(
        clippy::too_many_arguments,
        reason = "the private walk mirrors the explicit translation inputs"
    )]
    #[cold]
    fn walk(
        &mut self,
        bus: &mut Bus,
        satp: u64,
        mode: Mode,
        mxr: bool,
        sum: bool,
        adue: bool,
        acc: AccessType,
        va: u64,
    ) -> Result<u64, Exception> {
        // Canonical check: bits 63:39 must all equal bit 38.
        let hi = (va as i64) >> 38;
        if hi != 0 && hi != -1 {
            return Err(page_fault(acc, va));
        }

        let mut a = (satp & PPN_MASK) << 12;
        let mut level: u32 = 2;
        loop {
            let vpn_i = (va >> (12 + 9 * level)) & 0x1FF;
            let pte_addr = a + vpn_i * 8;
            if !bus.is_main_memory(pte_addr, 8) {
                return Err(access_fault(acc, va));
            }
            let pte = bus.read(pte_addr, 8).map_err(|_| access_fault(acc, va))?;

            if pte & PTE_RESERVED_HIGH != 0 {
                return Err(page_fault(acc, va));
            }

            let r = pte & PTE_R != 0;
            let w = pte & PTE_W != 0;
            let x = pte & PTE_X != 0;
            if pte & PTE_V == 0 || (w && !r) {
                return Err(page_fault(acc, va));
            }

            if r || x {
                // Leaf PTE at this level.
                let pte_ppn = (pte >> 10) & PPN_MASK;
                if level > 0 && pte_ppn & ((1 << (9 * level)) - 1) != 0 {
                    // Misaligned superpage.
                    return Err(page_fault(acc, va));
                }
                if !perm_ok(pte, mode, mxr, sum, acc) {
                    return Err(page_fault(acc, va));
                }

                // Hardware A/D update, written back to memory.
                let mut new_pte = pte | PTE_A;
                if acc == AccessType::Store {
                    new_pte |= PTE_D;
                }
                if new_pte != pte {
                    // Svadu: with menvcfg.ADUE clear hardware must not set
                    // A/D, so the access faults and software updates it.
                    // Faulting before the TLB fill keeps stale flags out.
                    if !adue {
                        return Err(page_fault(acc, va));
                    }
                    if !bus.is_main_memory(pte_addr, 8) {
                        return Err(access_fault(acc, va));
                    }
                    bus.write(pte_addr, new_pte, 8)
                        .map_err(|_| access_fault(acc, va))?;
                }

                // High bits from the PTE ppn, low 12 + 9*level bits from the
                // VA, so a superpage takes its low ppn bits from the VA.
                let off_mask = (1u64 << (12 + 9 * level)) - 1;
                let pa = ((pte_ppn << 12) & !off_mask) | (va & off_mask);

                // Cache the resolved 4-KiB frame with the flags as they now
                // stand in memory (A/D updated).
                let vpn = va >> 12;
                self.tlb[(vpn as usize) & (TLB_ENTRIES - 1)] = TlbEntry {
                    vpn,
                    pa_base: pa & !0xFFF,
                    flags: new_pte & 0xFF,
                };
                return Ok(pa);
            }

            // Pointer to the next level.
            if level == 0 {
                return Err(page_fault(acc, va));
            }
            if pte & PTE_RESERVED_NONLEAF != 0 {
                return Err(page_fault(acc, va));
            }
            level -= 1;
            a = ((pte >> 10) & PPN_MASK) << 12;
        }
    }
}

/// Leaf permission check from PTE flag bits. Never reached in Machine mode,
/// which is identity-mapped earlier.
#[inline(always)]
fn perm_ok(flags: u64, mode: Mode, mxr: bool, sum: bool, acc: AccessType) -> bool {
    let u_page = flags & PTE_U != 0;
    match mode {
        Mode::User => {
            if !u_page {
                return false;
            }
        }
        Mode::Supervisor => {
            if u_page {
                // S-mode never executes from U pages; loads/stores only
                // with SUM.
                if acc == AccessType::Fetch || !sum {
                    return false;
                }
            }
        }
        Mode::Machine => {}
    }
    match acc {
        AccessType::Fetch => flags & PTE_X != 0,
        AccessType::Load => flags & PTE_R != 0 || (mxr && flags & PTE_X != 0),
        AccessType::Store => flags & PTE_W != 0,
    }
}

#[cold]
fn page_fault(acc: AccessType, va: u64) -> Exception {
    match acc {
        AccessType::Fetch => Exception::InstructionPageFault(va),
        AccessType::Load => Exception::LoadPageFault(va),
        AccessType::Store => Exception::StorePageFault(va),
    }
}

#[cold]
fn access_fault(acc: AccessType, va: u64) -> Exception {
    match acc {
        AccessType::Fetch => Exception::InstructionAccessFault(va),
        AccessType::Load => Exception::LoadAccessFault(va),
        AccessType::Store => Exception::StoreAccessFault(va),
    }
}

impl Default for Mmu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{DRAM_BASE, UART_BASE};

    // Physical layout used by the tests (all inside 4 MiB of RAM).
    const ROOT: u64 = DRAM_BASE + 0x1000; // level-2 (root) table
    const L1: u64 = DRAM_BASE + 0x2000; // level-1 table
    const L0: u64 = DRAM_BASE + 0x3000; // level-0 table
    const FRAME: u64 = DRAM_BASE + 0x4000; // a 4-KiB data frame
    const FRAME2: u64 = DRAM_BASE + 0x5000; // an alternative 4-KiB frame

    const SATP: u64 = (8 << 60) | (ROOT >> 12);

    fn pte(pa: u64, flags: u64) -> u64 {
        ((pa >> 12) << 10) | flags
    }

    fn setup() -> (Mmu, Bus) {
        (Mmu::new(), Bus::new(4 << 20))
    }

    /// Wire root -> L1 -> L0 pointers for VA 0 region (vpn2 = vpn1 = 0) and
    /// install `leaf` at L0 slot `vpn0`.
    fn map_4k(bus: &mut Bus, vpn0: u64, leaf: u64) {
        bus.write(ROOT, pte(L1, PTE_V), 8).unwrap();
        bus.write(L1, pte(L0, PTE_V), 8).unwrap();
        bus.write(L0 + vpn0 * 8, leaf, 8).unwrap();
    }

    #[test]
    fn bare_and_machine_are_identity() {
        let (mut mmu, mut bus) = setup();
        // satp.MODE == 0 -> identity even in S/U mode.
        assert_eq!(
            mmu.translate(
                &mut bus,
                0,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0xdead_b000
            ),
            Ok(0xdead_b000)
        );
        assert_eq!(
            mmu.translate(
                &mut bus,
                0,
                Mode::User,
                false,
                false,
                true,
                AccessType::Fetch,
                0x42
            ),
            Ok(0x42)
        );
        // Machine mode -> identity even with Sv39 enabled.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Machine,
                false,
                false,
                true,
                AccessType::Store,
                0x1234_5678
            ),
            Ok(0x1234_5678)
        );
    }

    #[test]
    fn basic_4k_mapping_s_and_u() {
        let (mut mmu, mut bus) = setup();
        // va 0x5000 -> FRAME, RWX, user-accessible, A/D preset.
        let va = 0x5000u64;
        map_4k(
            &mut bus,
            5,
            pte(FRAME, PTE_V | PTE_R | PTE_W | PTE_X | PTE_U | PTE_A | PTE_D),
        );

        let pa = mmu
            .translate(
                &mut bus,
                SATP,
                Mode::User,
                false,
                false,
                true,
                AccessType::Load,
                va + 0x123,
            )
            .unwrap();
        assert_eq!(pa, FRAME + 0x123);
        let pa = mmu
            .translate(
                &mut bus,
                SATP,
                Mode::User,
                false,
                false,
                true,
                AccessType::Fetch,
                va,
            )
            .unwrap();
        assert_eq!(pa, FRAME);
        // S-mode to a U page: needs SUM for data...
        let pa = mmu
            .translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                true,
                true,
                AccessType::Store,
                va + 8,
            )
            .unwrap();
        assert_eq!(pa, FRAME + 8);
    }

    #[test]
    fn permission_faults() {
        let (mut mmu, mut bus) = setup();
        // slot 1: execute-only; slot 2: read-only.
        map_4k(&mut bus, 1, pte(FRAME, PTE_V | PTE_X | PTE_A | PTE_D));
        bus.write(L0 + 2 * 8, pte(FRAME2, PTE_V | PTE_R | PTE_A | PTE_D), 8)
            .unwrap();

        // Load from X-only without MXR -> LoadPageFault.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0x1000
            ),
            Err(Exception::LoadPageFault(0x1000))
        );
        // MXR allows the load from an X-only page.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                true,
                false,
                true,
                AccessType::Load,
                0x1004
            ),
            Ok(FRAME + 4)
        );
        // Fetch from a page without X -> InstructionPageFault.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Fetch,
                0x2000
            ),
            Err(Exception::InstructionPageFault(0x2000))
        );
        // Store to a page without W -> StorePageFault.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Store,
                0x2008
            ),
            Err(Exception::StorePageFault(0x2008))
        );
        // Load is fine on the R page.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0x2010
            ),
            Ok(FRAME2 + 0x10)
        );
    }

    #[test]
    fn u_bit_rules() {
        let (mut mmu, mut bus) = setup();
        // slot 3: U page RWX; slot 4: S-only page RWX.
        map_4k(
            &mut bus,
            3,
            pte(FRAME, PTE_V | PTE_R | PTE_W | PTE_X | PTE_U | PTE_A | PTE_D),
        );
        bus.write(
            L0 + 4 * 8,
            pte(FRAME2, PTE_V | PTE_R | PTE_W | PTE_X | PTE_A | PTE_D),
            8,
        )
        .unwrap();

        // S-mode load from U page without SUM -> fault.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0x3000
            ),
            Err(Exception::LoadPageFault(0x3000))
        );
        // SUM allows the load...
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                true,
                true,
                AccessType::Load,
                0x3000
            ),
            Ok(FRAME)
        );
        // ...but never the fetch, even with SUM.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                true,
                true,
                AccessType::Fetch,
                0x3000
            ),
            Err(Exception::InstructionPageFault(0x3000))
        );
        // U-mode access to a non-U page -> fault.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::User,
                false,
                false,
                true,
                AccessType::Load,
                0x4000
            ),
            Err(Exception::LoadPageFault(0x4000))
        );
    }

    #[test]
    fn reserved_and_invalid_ptes() {
        let (mut mmu, mut bus) = setup();
        // W without R is reserved.
        map_4k(&mut bus, 6, pte(FRAME, PTE_V | PTE_W | PTE_A | PTE_D));
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Store,
                0x6000
            ),
            Err(Exception::StorePageFault(0x6000))
        );
        // V == 0 -> fault (slot 7 was never written; RAM is zeroed).
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0x7000
            ),
            Err(Exception::LoadPageFault(0x7000))
        );
        // Non-leaf chain deeper than level 0 (L0 entry is a pointer) -> fault.
        bus.write(L0 + 8 * 8, pte(FRAME, PTE_V), 8).unwrap();
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0x8000
            ),
            Err(Exception::LoadPageFault(0x8000))
        );
    }

    fn assert_nonleaf_reserved_bit_faults(bit: u64) {
        let (mut mmu, mut bus) = setup();
        bus.write(ROOT, pte(L1, PTE_V | bit), 8).unwrap();

        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0,
            ),
            Err(Exception::LoadPageFault(0))
        );
    }

    #[test]
    fn nonleaf_dirty_bit_faults() {
        assert_nonleaf_reserved_bit_faults(PTE_D);
    }

    #[test]
    fn nonleaf_accessed_bit_faults() {
        assert_nonleaf_reserved_bit_faults(PTE_A);
    }

    #[test]
    fn nonleaf_user_bit_faults() {
        assert_nonleaf_reserved_bit_faults(PTE_U);
    }

    fn assert_reserved_high_pte_bit_faults(bit: u64) {
        let (mut mmu, mut bus) = setup();
        map_4k(&mut bus, 1, pte(FRAME, PTE_V | PTE_R | PTE_A | PTE_D) | bit);

        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0x1000,
            ),
            Err(Exception::LoadPageFault(0x1000))
        );
    }

    #[test]
    fn unsupported_svnapot_bit_faults() {
        assert_reserved_high_pte_bit_faults(1 << 63);
    }

    #[test]
    fn unsupported_svpbmt_bits_fault() {
        assert_reserved_high_pte_bit_faults(1 << 61);
    }

    #[test]
    fn reserved_pte_bit_60_faults() {
        assert_reserved_high_pte_bit_faults(1 << 60);
    }

    #[test]
    fn non_canonical_va_faults() {
        let (mut mmu, mut bus) = setup();
        map_4k(&mut bus, 0, pte(FRAME, PTE_V | PTE_R | PTE_A | PTE_D));
        // Bit 39 set but bits 63:40 clear -> non-canonical.
        let bad = 1u64 << 39;
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                bad
            ),
            Err(Exception::LoadPageFault(bad))
        );
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Fetch,
                bad
            ),
            Err(Exception::InstructionPageFault(bad))
        );
        // Properly sign-extended VA walks normally (here: V==0 page fault,
        // not a canonicality fault, proving the check passed).
        let hi_va = 0xFFFF_FFC0_0000_0000u64; // bit 38 set, 63:39 all ones
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                hi_va
            ),
            Err(Exception::LoadPageFault(hi_va))
        );
    }

    #[test]
    fn walk_read_outside_ram_is_access_fault() {
        let (mut mmu, mut bus) = setup();
        // Root table placed beyond the 4 MiB of RAM: the PTE read fails.
        let satp = (8 << 60) | ((DRAM_BASE + (16 << 20)) >> 12);
        assert_eq!(
            mmu.translate(
                &mut bus,
                satp,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0x1000
            ),
            Err(Exception::LoadAccessFault(0x1000))
        );
        assert_eq!(
            mmu.translate(
                &mut bus,
                satp,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Fetch,
                0x1000
            ),
            Err(Exception::InstructionAccessFault(0x1000))
        );
    }

    #[test]
    fn walk_does_not_read_a_pte_from_mmio() {
        let (mut mmu, mut bus) = setup();
        bus.uart.push_rx(PTE_V as u8);
        let satp = (8 << 60) | (UART_BASE >> 12);

        let result = mmu.translate(
            &mut bus,
            satp,
            Mode::Supervisor,
            false,
            false,
            true,
            AccessType::Load,
            0,
        );

        assert_eq!(result, Err(Exception::LoadAccessFault(0)));
        assert_eq!(bus.uart.rx.front(), Some(&(PTE_V as u8)));
    }

    #[test]
    fn superpages_2m_and_1g() {
        let (mut mmu, mut bus) = setup();
        // 2 MiB page: vpn2 = 0, vpn1 = 1 -> va 0x20_0000, mapped to a
        // 2 MiB-aligned pa (DRAM_BASE + 2 MiB).
        bus.write(ROOT, pte(L1, PTE_V), 8).unwrap();
        let pa_2m = DRAM_BASE + (2 << 20);
        bus.write(L1 + 8, pte(pa_2m, PTE_V | PTE_R | PTE_W | PTE_A | PTE_D), 8)
            .unwrap();
        let va = 0x20_0000u64 + 0x1_2345;
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                va
            ),
            Ok(pa_2m + 0x1_2345)
        );

        // 1 GiB page: vpn2 = 2 -> va 0x8000_0000, mapped to pa 0x4000_0000
        // (1 GiB-aligned; only translated, never accessed).
        bus.write(
            ROOT + 2 * 8,
            pte(0x4000_0000, PTE_V | PTE_R | PTE_X | PTE_A | PTE_D),
            8,
        )
        .unwrap();
        let va = 0x8000_0000u64 + 0x123_4567;
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Fetch,
                va
            ),
            Ok(0x4000_0000 + 0x123_4567)
        );

        // Misaligned 2 MiB superpage (pa only 4 KiB-aligned) -> page fault.
        bus.write(L1 + 2 * 8, pte(FRAME, PTE_V | PTE_R | PTE_A | PTE_D), 8)
            .unwrap();
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                0x40_0000
            ),
            Err(Exception::LoadPageFault(0x40_0000))
        );

        // Misaligned 1 GiB superpage.
        bus.write(ROOT + 3 * 8, pte(pa_2m, PTE_V | PTE_R | PTE_A | PTE_D), 8)
            .unwrap();
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                3 << 30
            ),
            Err(Exception::LoadPageFault(3 << 30))
        );
    }

    #[test]
    fn a_d_hardware_update() {
        let (mut mmu, mut bus) = setup();
        // A = 0, D = 0 leaf.
        map_4k(&mut bus, 9, pte(FRAME, PTE_V | PTE_R | PTE_W));
        let pte_addr = L0 + 9 * 8;

        // Load sets A (but not D) in memory.
        mmu.translate(
            &mut bus,
            SATP,
            Mode::Supervisor,
            false,
            false,
            true,
            AccessType::Load,
            0x9000,
        )
        .unwrap();
        let in_mem = bus.read(pte_addr, 8).unwrap();
        assert_eq!(in_mem & PTE_A, PTE_A);
        assert_eq!(in_mem & PTE_D, 0);

        // Store sets D too.
        mmu.translate(
            &mut bus,
            SATP,
            Mode::Supervisor,
            false,
            false,
            true,
            AccessType::Store,
            0x9000,
        )
        .unwrap();
        let in_mem = bus.read(pte_addr, 8).unwrap();
        assert_eq!(in_mem & (PTE_A | PTE_D), PTE_A | PTE_D);
    }

    #[test]
    fn adue_clear_faults_instead_of_setting_accessed() {
        let (mut mmu, mut bus) = setup();
        let va = 0xD000;
        map_4k(&mut bus, 13, pte(FRAME, PTE_V | PTE_R));
        let pte_addr = L0 + 13 * 8;

        let result = mmu.translate(
            &mut bus,
            SATP,
            Mode::Supervisor,
            false,
            false,
            false,
            AccessType::Load,
            va,
        );

        assert_eq!(
            (result, bus.read(pte_addr, 8).unwrap() & PTE_A),
            (Err(Exception::LoadPageFault(va)), 0)
        );
    }

    #[test]
    fn adue_clear_faults_instead_of_setting_dirty() {
        let (mut mmu, mut bus) = setup();
        let va = 0xE000;
        map_4k(&mut bus, 14, pte(FRAME, PTE_V | PTE_R | PTE_W | PTE_A));
        let pte_addr = L0 + 14 * 8;

        let result = mmu.translate(
            &mut bus,
            SATP,
            Mode::Supervisor,
            false,
            false,
            false,
            AccessType::Store,
            va,
        );

        assert_eq!(
            (result, bus.read(pte_addr, 8).unwrap() & PTE_D),
            (Err(Exception::StorePageFault(va)), 0)
        );
    }

    #[test]
    fn store_after_load_rewalks_to_set_d() {
        let (mut mmu, mut bus) = setup();
        map_4k(&mut bus, 10, pte(FRAME, PTE_V | PTE_R | PTE_W));
        let pte_addr = L0 + 10 * 8;
        let va = 0xA000u64;

        // Load caches the entry with D = 0.
        let pa = mmu
            .translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                va,
            )
            .unwrap();
        assert_eq!(pa, FRAME);
        assert_eq!(bus.read(pte_addr, 8).unwrap() & PTE_D, 0);

        // Store hits the TLB but D is clear -> full walk, D set in memory.
        let pa = mmu
            .translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Store,
                va + 4,
            )
            .unwrap();
        assert_eq!(pa, FRAME + 4);
        assert_eq!(bus.read(pte_addr, 8).unwrap() & PTE_D, PTE_D);
    }

    #[test]
    fn tlb_hit_and_flush() {
        let (mut mmu, mut bus) = setup();
        let va = 0xB000u64;
        map_4k(&mut bus, 11, pte(FRAME, PTE_V | PTE_R | PTE_A | PTE_D));

        // First translate walks and caches; second must return the same pa.
        let pa1 = mmu
            .translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                va,
            )
            .unwrap();
        let pa2 = mmu
            .translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                va + 0x40,
            )
            .unwrap();
        assert_eq!(pa1, FRAME);
        assert_eq!(pa2, FRAME + 0x40);

        // Change the mapping under the TLB: the stale translation is still
        // served (proving the hit path does not re-walk)...
        bus.write(L0 + 11 * 8, pte(FRAME2, PTE_V | PTE_R | PTE_A | PTE_D), 8)
            .unwrap();
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                va
            ),
            Ok(FRAME)
        );
        // ...until an explicit flush picks up the new mapping.
        mmu.flush_all();
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                va
            ),
            Ok(FRAME2)
        );
    }

    #[test]
    fn tlb_hit_reevaluates_permissions() {
        let (mut mmu, mut bus) = setup();
        let va = 0xC000u64;
        // U page, R only.
        map_4k(
            &mut bus,
            12,
            pte(FRAME, PTE_V | PTE_R | PTE_U | PTE_A | PTE_D),
        );

        // Fill the TLB from U mode.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::User,
                false,
                false,
                true,
                AccessType::Load,
                va
            ),
            Ok(FRAME)
        );
        // Same entry, S mode without SUM: the cached flags must fault
        // (no flush in between; the mode change alone flips the answer).
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                false,
                true,
                AccessType::Load,
                va
            ),
            Err(Exception::LoadPageFault(va))
        );
        // And with SUM the same cached entry allows the load again.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::Supervisor,
                false,
                true,
                true,
                AccessType::Load,
                va
            ),
            Ok(FRAME)
        );
        // Store on the cached R-only entry faults too.
        assert_eq!(
            mmu.translate(
                &mut bus,
                SATP,
                Mode::User,
                false,
                false,
                true,
                AccessType::Store,
                va
            ),
            Err(Exception::StorePageFault(va))
        );
    }
}
