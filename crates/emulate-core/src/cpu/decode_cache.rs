//! Lazy basic-block cache for already-decoded guest instructions.
//!
//! Blocks grow only after an instruction executed and fell through to the next
//! PC, so building one never performs a speculative fetch that could raise an
//! early fault, set a PTE accessed bit, or read an MMIO register.
//!
//! # Why blocks are keyed on the physical address
//!
//! Physical keys let address spaces share decoded code. `satp` writes and
//! `SFENCE.VMA` change translation, not physical instruction bytes, so they
//! retain cached blocks. The fetch path still checks translation and execute
//! permissions before looking up a block.
//!
//! # Decode-time dependency audit
//!
//! `decode::decode(raw) -> Result<DecodedInst, ()>` is a pure function of the
//! 32-bit fetch window.  It takes no CPU state: not `misa` (which is WARL and
//! ignores writes here, so the C extension is always enabled), not `mstatus`,
//! and not the current privilege mode.  Everything privilege-dependent —
//! `ECALL`'s cause, `WFI`/`SFENCE.VMA` trapping via `mstatus.TW`/`TVM`, CSR
//! access permission — is decided in `execute`, from live CPU state, on every
//! execution of the instruction.  So the tag is *only* the physical address:
//! no mode, no CSR state.  If a future extension makes decoding depend on CPU
//! state (a `misa` bit that can actually be cleared, say), that state must be
//! added to the tag or written into `flush()`.

use super::csr;
use super::decode::{DecodedInst, Op};
use crate::ram::Ram;

// Linux boot measurements plateau at 1024 sets; larger caches cost memory
// without improving boot time.
const BLOCK_SETS: usize = 1024;
const BLOCK_WAYS: usize = 4;
const BLOCK_SLOTS: usize = BLOCK_SETS * BLOCK_WAYS;
const MAX_BLOCK_INSTRUCTIONS: usize = 16;
const INVALID_PA: u64 = u64::MAX;

#[derive(Clone, Copy)]
pub(super) struct CachedInstruction {
    /// Guest-physical address of this instruction.  Virtual addresses are
    /// deliberately not stored: a block is shared by every mapping of the page.
    pub pa: u64,
    pub raw: u32,
    pub decoded: DecodedInst,
}

#[derive(Clone, Copy)]
pub(super) struct CacheLocation {
    pub slot: usize,
    pub index: usize,
    pub instruction: CachedInstruction,
}

struct DecodedBlock {
    start_pa: u64,
    ram_page: u64,
    page_generation: u64,
    len: u8,
    instructions: [Option<CachedInstruction>; MAX_BLOCK_INSTRUCTIONS],
}

impl DecodedBlock {
    fn invalid() -> Self {
        Self {
            start_pa: INVALID_PA,
            ram_page: 0,
            page_generation: 0,
            len: 0,
            instructions: [None; MAX_BLOCK_INSTRUCTIONS],
        }
    }

    #[inline]
    fn generation_is_current(&self, ram: &Ram) -> bool {
        ram.generation_for_page(self.ram_page) == Some(self.page_generation)
    }

    fn invalidate(&mut self) {
        self.start_pa = INVALID_PA;
        self.len = 0;
    }
}

#[derive(Clone, Copy)]
struct ActiveInstruction {
    slot: usize,
    index: usize,
    expected_pc: u64,
    expected_pa: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeCacheStats {
    pub hits: u64,
    pub misses: u64,
}

/// Continuation of the block the previous instruction belonged to.  This is
/// the only lookup that needs no translation: the block is page-local and
/// every instruction that could change the fetch mapping or the privilege
/// mode ends a block, so the block start's translation still stands.
pub(super) enum ActiveLookup {
    Hit(CacheLocation),
    /// Grow the active block: decode the instruction and append it here.
    Append {
        slot: usize,
        index: usize,
    },
    /// No active block; the caller must translate the PC and look up by
    /// physical address.
    None,
}

pub(super) enum Lookup {
    Hit(CacheLocation),
    Miss,
}

pub(super) struct DecodeCache {
    blocks: Box<[DecodedBlock]>,
    next_victim: Box<[u8]>,
    active: Option<ActiveInstruction>,
    stats: DecodeCacheStats,
}

impl DecodeCache {
    pub fn new() -> Self {
        let blocks = (0..BLOCK_SLOTS)
            .map(|_| DecodedBlock::invalid())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            blocks,
            next_victim: vec![0; BLOCK_SETS].into_boxed_slice(),
            active: None,
            stats: DecodeCacheStats::default(),
        }
    }

    pub fn stats(&self) -> DecodeCacheStats {
        self.stats
    }

    /// Discard every block.  `FENCE.I` is the only caller: it is the
    /// architectural ordering point for self-modifying code, and it is rare
    /// enough that a full flush costs nothing measurable.  Ordinary stores to
    /// code are caught without it by the per-page write generations.
    pub fn flush(&mut self) {
        for block in &mut self.blocks {
            block.invalidate();
        }
        self.next_victim.fill(0);
        self.active = None;
    }

    fn invalidate_generation(&mut self, slot: usize) {
        self.blocks[slot].invalidate();
    }

    /// Continue the block the previous instruction fell through from.
    pub fn lookup_active(&mut self, pc: u64, ram: &Ram) -> ActiveLookup {
        let Some(active) = self.active.take() else {
            return ActiveLookup::None;
        };
        let block = &self.blocks[active.slot];
        if active.expected_pc == pc
            && block.start_pa != INVALID_PA
            && block.generation_is_current(ram)
        {
            let len = block.len as usize;
            if active.index < len {
                if let Some(instruction) = block.instructions[active.index] {
                    if instruction.pa == active.expected_pa {
                        self.stats.hits = self.stats.hits.wrapping_add(1);
                        return ActiveLookup::Hit(CacheLocation {
                            slot: active.slot,
                            index: active.index,
                            instruction,
                        });
                    }
                }
            } else if active.index == len && len < MAX_BLOCK_INSTRUCTIONS {
                self.stats.misses = self.stats.misses.wrapping_add(1);
                return ActiveLookup::Append {
                    slot: active.slot,
                    index: active.index,
                };
            }
        } else if block.start_pa != INVALID_PA && !block.generation_is_current(ram) {
            self.invalidate_generation(active.slot);
        }
        ActiveLookup::None
    }

    /// Look up a block start by guest-physical address.  The caller has
    /// already translated the PC, so fetch permission for this address has
    /// been checked against the *current* page tables.
    pub fn lookup_physical(&mut self, pa: u64, ram: &Ram) -> Lookup {
        let first_slot = set_for(pa) * BLOCK_WAYS;
        for slot in first_slot..first_slot + BLOCK_WAYS {
            let block = &self.blocks[slot];
            if block.start_pa == pa {
                if block.generation_is_current(ram) {
                    if let Some(instruction) = block.instructions[0] {
                        self.stats.hits = self.stats.hits.wrapping_add(1);
                        return Lookup::Hit(CacheLocation {
                            slot,
                            index: 0,
                            instruction,
                        });
                    }
                } else {
                    self.invalidate_generation(slot);
                }
            }
        }

        self.stats.misses = self.stats.misses.wrapping_add(1);
        Lookup::Miss
    }

    /// Insert a decoded instruction, appending it to the block currently being
    /// grown when its physical page still matches.  Returns its cache location.
    pub fn insert(
        &mut self,
        pa: u64,
        ram_page: u64,
        page_generation: u64,
        instruction: CachedInstruction,
        append: Option<(usize, usize)>,
    ) -> CacheLocation {
        if let Some((slot, index)) = append {
            let block = &mut self.blocks[slot];
            if block.start_pa != INVALID_PA
                && block.ram_page == ram_page
                && block.page_generation == page_generation
                && index == block.len as usize
                && index < MAX_BLOCK_INSTRUCTIONS
            {
                block.instructions[index] = Some(instruction);
                block.len += 1;
                return CacheLocation {
                    slot,
                    index,
                    instruction,
                };
            }
        }

        let set = set_for(pa);
        let first_slot = set * BLOCK_WAYS;
        let first_way = self.next_victim[set] as usize;
        let way = (0..BLOCK_WAYS)
            .map(|offset| (first_way + offset) % BLOCK_WAYS)
            .find(|&way| self.blocks[first_slot + way].start_pa == INVALID_PA)
            .unwrap_or(first_way);
        self.next_victim[set] = ((way + 1) % BLOCK_WAYS) as u8;
        let slot = first_slot + way;
        let block = &mut self.blocks[slot];
        block.start_pa = pa;
        block.ram_page = ram_page;
        block.page_generation = page_generation;
        block.len = 1;
        block.instructions[0] = Some(instruction);
        CacheLocation {
            slot,
            index: 0,
            instruction,
        }
    }

    /// Continue a block only after a successful architectural fallthrough.
    pub fn finish_instruction(
        &mut self,
        location: CacheLocation,
        instruction_pc: u64,
        next_pc: u64,
        actual_pc: u64,
        ram: &Ram,
    ) {
        let CacheLocation {
            slot,
            index,
            instruction,
        } = location;
        if ends_block(&instruction.decoded.op) || actual_pc != next_pc {
            self.active = None;
            return;
        }

        let block = &self.blocks[slot];
        if block.start_pa == INVALID_PA
            || !block.generation_is_current(ram)
            || block.instructions[index].map(|cached| cached.pa) != Some(instruction.pa)
            || index + 1 >= MAX_BLOCK_INSTRUCTIONS
            || next_pc >> 12 != instruction_pc >> 12
        {
            self.active = None;
            return;
        }
        // Page-local by the check above, so the next instruction's physical
        // address is its predecessor's plus the same delta as the virtual one.
        self.active = Some(ActiveInstruction {
            slot,
            index: index + 1,
            expected_pc: next_pc,
            expected_pa: instruction
                .pa
                .wrapping_add(next_pc.wrapping_sub(instruction_pc)),
        });
    }
}

#[inline]
fn set_for(pa: u64) -> usize {
    // Mix higher address bits so aligned block starts can use every set.
    (((pa >> 1) ^ (pa >> 11)) as usize) & (BLOCK_SETS - 1)
}

/// Instructions that can redirect execution, change fetch translation/cache
/// state, or deliberately yield to the host terminate a decoded block.
///
/// A `satp` access is in the list because it is the one non-branching
/// instruction that can change how the *rest of the block* translates. Blocks
/// are physically keyed and retained on a `satp` write, so without
/// this the instructions after `csrw satp` would keep running out of the old
/// address space's physical page. (Reads of `satp` end a block too; they are
/// rare enough that distinguishing them is not worth it.)
fn ends_block(op: &Op) -> bool {
    matches!(
        op,
        Op::Csr { csr: csr::SATP, .. }
            | Op::Jal { .. }
            | Op::Jalr { .. }
            | Op::Branch { .. }
            | Op::FenceI
            | Op::Ecall
            | Op::Ebreak
            | Op::Mret
            | Op::Sret
            | Op::Wfi
            | Op::SfenceVma { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::decode::CsrOp;

    #[test]
    fn csr_read_does_not_end_a_decoded_block() {
        let op = Op::Csr {
            kind: CsrOp::Rs,
            rd: 10,
            rs1: 0,
            csr: 0xc01,
        };

        assert!(!ends_block(&op));
    }
}
