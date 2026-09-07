//! PLIC (platform-level interrupt controller), QEMU-virt-compatible subset.
//!
//! Sources 1..=31 supported (we only use UART_IRQ = 10). Two contexts:
//! context 0 = hart 0 M-mode, context 1 = hart 0 S-mode.
//!
//! Layout (offsets from PLIC_BASE):
//! - 0x000000 + irq*4:      priority for source irq (0 = never interrupt)
//! - 0x001000 + word*4:     pending bits (read-only)
//! - 0x002000 + ctx*0x80:   enable bits for context (word 0 = sources 0..31)
//! - 0x200000 + ctx*0x1000: threshold
//! - 0x200004 + ctx*0x1000: claim (read) / complete (write)
//!
//! Semantics: level-triggered sources via `set_irq(id, level)`; a source is
//! claimable when pending && enabled && priority > threshold. Claim returns
//! the highest-priority such source and clears its pending bit; complete
//! re-arms it (re-raises pending if the level is still high). `eip(ctx)`
//! reports whether an interrupt should be signaled to that context.

use crate::snapshot::{Reader, Writer};

pub(crate) const NUM_SOURCES: usize = 32;
pub(crate) const NUM_CONTEXTS: usize = 2;

const PRIORITY_BASE: u64 = 0x0000;
const PENDING_BASE: u64 = 0x1000;
const ENABLE_BASE: u64 = 0x2000;
const ENABLE_STRIDE: u64 = 0x80;
const CONTEXT_BASE: u64 = 0x20_0000;
const CONTEXT_STRIDE: u64 = 0x1000;

pub struct Plic {
    pub(crate) priority: [u32; NUM_SOURCES],
    pub(crate) pending: u32,
    /// Current level of each (level-triggered) source line.
    pub(crate) level: u32,
    pub(crate) enable: [u32; NUM_CONTEXTS],
    pub(crate) threshold: [u32; NUM_CONTEXTS],
    /// Per-context sources claimed and not yet completed (gated from
    /// re-pending). Tracking this per context means an M-mode claim of one
    /// source does not block an S-mode claim of another, as on real hardware.
    pub(crate) claimed: [u32; NUM_CONTEXTS],
    /// Bit `ctx` is set when that context has at least one claimable source.
    /// Recomputed on input changes so `eip`, sampled twice per instruction, is
    /// a constant-time lookup.
    ready_mask: u8,
}

impl Plic {
    pub fn new() -> Self {
        Plic {
            priority: [0; NUM_SOURCES],
            pending: 0,
            level: 0,
            enable: [0; NUM_CONTEXTS],
            threshold: [0; NUM_CONTEXTS],
            claimed: [0; NUM_CONTEXTS],
            ready_mask: 0,
        }
    }

    fn recompute_ready(&mut self) {
        let mut ready_mask = 0;
        for ctx in 0..NUM_CONTEXTS {
            let mut candidates = self.pending & self.enable[ctx];
            let threshold = self.threshold[ctx];
            while candidates != 0 {
                let id = candidates.trailing_zeros() as usize;
                if self.priority[id] > threshold {
                    ready_mask |= 1 << ctx;
                    break;
                }
                candidates &= candidates - 1;
            }
        }
        self.ready_mask = ready_mask;
    }

    /// A source is mid-claim if any context has claimed and not completed it.
    #[inline]
    fn any_claimed(&self) -> u32 {
        self.claimed.iter().fold(0, |a, c| a | c)
    }

    /// Drive a source line level (edge -> pending latch handled inside).
    pub fn set_irq(&mut self, id: u32, level: bool) {
        if id == 0 || id >= NUM_SOURCES as u32 {
            return;
        }
        let bit = 1u32 << id;
        if (self.level & bit != 0) == level {
            return;
        }
        if level {
            self.level |= bit;
            // Level high latches pending unless the source is mid-claim.
            if self.any_claimed() & bit == 0 {
                self.pending |= bit;
            }
        } else {
            self.level &= !bit;
            // Level-triggered: dropping the line clears pending.
            self.pending &= !bit;
        }
        self.recompute_ready();
    }

    /// Should an external interrupt be signaled to context `ctx`? Reads the
    /// cached claimability bitmap, so this hot path never scans sources.
    #[inline]
    pub fn eip(&self, ctx: usize) -> bool {
        ctx < NUM_CONTEXTS && self.ready_mask & (1 << ctx) != 0
    }

    /// Claim the highest-priority pending+enabled source for `ctx`
    /// (ties: lowest id). Returns 0 if none.
    fn claim(&mut self, ctx: usize) -> u32 {
        let candidates = self.pending & self.enable[ctx];
        let mut best = 0u32;
        let mut best_prio = 0u32;
        for id in 1..NUM_SOURCES as u32 {
            // Only claim a source above this context's threshold, so claim and
            // eip agree on what the context can see (ties: lowest id).
            if candidates & (1 << id) != 0
                && self.priority[id as usize] > self.threshold[ctx]
                && self.priority[id as usize] > best_prio
            {
                best_prio = self.priority[id as usize];
                best = id;
            }
        }
        if best != 0 {
            let bit = 1u32 << best;
            self.pending &= !bit;
            self.claimed[ctx] |= bit;
            self.recompute_ready();
        }
        best
    }

    /// Signal completion of a previously claimed source for `ctx`.
    fn complete(&mut self, ctx: usize, id: u32) {
        if ctx >= NUM_CONTEXTS || id == 0 || id >= NUM_SOURCES as u32 {
            return;
        }
        let bit = 1u32 << id;
        // Ignore a completion for a source this context never claimed: a guest
        // must not re-pend an unowned line by writing an arbitrary id.
        if self.claimed[ctx] & bit == 0 {
            return;
        }
        self.claimed[ctx] &= !bit;
        if self.level & bit != 0 {
            self.pending |= bit;
        }
        self.recompute_ready();
    }

    pub fn read(&mut self, offset: u64, size: u8) -> Result<u64, ()> {
        // Registers are 32-bit; be permissive about size (an aligned wider
        // read just returns the 32-bit value zero-extended).
        let _ = size;
        let val: u32 = match offset {
            PRIORITY_BASE..=0x0FFF => {
                let irq = (offset / 4) as usize;
                if offset.is_multiple_of(4) && irq < NUM_SOURCES {
                    self.priority[irq]
                } else {
                    0
                }
            }
            PENDING_BASE => self.pending,
            0x1004..=0x1FFF => 0, // higher pending words: no such sources
            ENABLE_BASE..=0x1F_FFFF => {
                let rel = offset - ENABLE_BASE;
                let ctx = (rel / ENABLE_STRIDE) as usize;
                let word = rel % ENABLE_STRIDE;
                if ctx < NUM_CONTEXTS && word == 0 {
                    self.enable[ctx]
                } else {
                    0
                }
            }
            CONTEXT_BASE.. => {
                let rel = offset - CONTEXT_BASE;
                let ctx = (rel / CONTEXT_STRIDE) as usize;
                let reg = rel % CONTEXT_STRIDE;
                if ctx < NUM_CONTEXTS {
                    match reg {
                        0 => self.threshold[ctx],
                        4 => self.claim(ctx),
                        _ => 0,
                    }
                } else {
                    0
                }
            }
            _ => 0,
        };
        Ok(val as u64)
    }

    pub fn write(&mut self, offset: u64, val: u64, size: u8) -> Result<(), ()> {
        let _ = size;
        let val = val as u32;
        match offset {
            PRIORITY_BASE..=0x0FFF => {
                let irq = (offset / 4) as usize;
                if offset.is_multiple_of(4) && irq < NUM_SOURCES {
                    self.priority[irq] = val;
                    self.recompute_ready();
                }
            }
            PENDING_BASE..=0x1FFF => {} // pending is read-only
            ENABLE_BASE..=0x1F_FFFF => {
                let rel = offset - ENABLE_BASE;
                let ctx = (rel / ENABLE_STRIDE) as usize;
                let word = rel % ENABLE_STRIDE;
                if ctx < NUM_CONTEXTS && word == 0 {
                    self.enable[ctx] = val;
                    self.recompute_ready();
                }
            }
            CONTEXT_BASE.. => {
                let rel = offset - CONTEXT_BASE;
                let ctx = (rel / CONTEXT_STRIDE) as usize;
                let reg = rel % CONTEXT_STRIDE;
                if ctx < NUM_CONTEXTS {
                    match reg {
                        0 => {
                            self.threshold[ctx] = val;
                            self.recompute_ready();
                        }
                        4 => self.complete(ctx, val),
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
}

impl Default for Plic {
    fn default() -> Self {
        Self::new()
    }
}

const SNAPSHOT_VERSION: u16 = 1;

impl Plic {
    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            w.u32(NUM_SOURCES as u32);
            w.u32(NUM_CONTEXTS as u32);
            for priority in self.priority {
                w.u32(priority);
            }
            w.u32(self.pending);
            w.u32(self.level);
            for context in 0..NUM_CONTEXTS {
                w.u32(self.enable[context]);
                w.u32(self.threshold[context]);
                w.u32(self.claimed[context]);
            }
        });
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("plic", SNAPSHOT_VERSION, |r| {
            let sources = r.u32()? as usize;
            let contexts = r.u32()? as usize;
            if sources != NUM_SOURCES || contexts != NUM_CONTEXTS {
                return Err(format!(
                    "{sources} sources / {contexts} contexts, this build has \
                     {NUM_SOURCES} / {NUM_CONTEXTS}"
                ));
            }
            for slot in self.priority.iter_mut() {
                *slot = r.u32()?;
            }
            self.pending = r.u32()?;
            self.level = r.u32()?;
            for context in 0..NUM_CONTEXTS {
                self.enable[context] = r.u32()?;
                self.threshold[context] = r.u32()?;
                self.claimed[context] = r.u32()?;
            }
            Ok(())
        })?;
        // `ready_mask` is a memo over the fields above, never independent state.
        self.recompute_ready();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UART: u32 = 10;

    /// Configure priority + enable + threshold via MMIO for context 0.
    fn setup(plic: &mut Plic) {
        plic.write((UART as u64) * 4, 3, 4).unwrap(); // priority 3
        plic.write(0x2000, 1 << UART, 4).unwrap(); // enable ctx 0
        plic.write(0x20_0000, 0, 4).unwrap(); // threshold 0
    }

    #[test]
    fn mmio_config_roundtrip() {
        let mut p = Plic::new();
        setup(&mut p);
        assert_eq!(p.read((UART as u64) * 4, 4).unwrap(), 3);
        assert_eq!(p.read(0x2000, 4).unwrap(), (1 << UART) as u64);
        assert_eq!(p.read(0x20_0000, 4).unwrap(), 0);
        // Context 1 registers are independent.
        p.write(0x20_1000, 5, 4).unwrap();
        assert_eq!(p.read(0x20_1000, 4).unwrap(), 5);
        assert_eq!(p.threshold[1], 5);
        p.write(0x2080, 0xFF, 4).unwrap();
        assert_eq!(p.enable[1], 0xFF);
        // Higher enable words read 0 / ignore writes.
        p.write(0x2004, 0xFFFF_FFFF, 4).unwrap();
        assert_eq!(p.read(0x2004, 4).unwrap(), 0);
    }

    #[test]
    fn set_irq_pending_and_eip() {
        let mut p = Plic::new();
        setup(&mut p);
        p.set_irq(UART, true);
        assert_eq!(p.read(0x1000, 4).unwrap(), (1 << UART) as u64);
        assert!(p.eip(0));
        assert!(!p.eip(1)); // ctx 1 not enabled
                            // Level drop clears pending.
        p.set_irq(UART, false);
        assert_eq!(p.pending, 0);
        assert!(!p.eip(0));
        // Writes to the pending region are ignored.
        p.write(0x1000, 0xFFFF_FFFF, 4).unwrap();
        assert_eq!(p.pending, 0);
    }

    #[test]
    fn claim_highest_priority_then_none() {
        let mut p = Plic::new();
        setup(&mut p);
        // Second, higher-priority source 7.
        p.write(7 * 4, 5, 4).unwrap();
        p.write(0x2000, (1 << UART) | (1 << 7), 4).unwrap();
        p.set_irq(UART, true);
        p.set_irq(7, true);
        // Claim returns highest priority first (7 @ prio 5 over 10 @ prio 3).
        assert_eq!(p.read(0x20_0004, 4).unwrap(), 7);
        assert_eq!(p.pending & (1 << 7), 0);
        assert_eq!(p.claimed[0], 1 << 7);
        // Next claim gets the UART.
        assert_eq!(p.read(0x20_0004, 4).unwrap(), UART as u64);
        // Nothing left: claim returns 0.
        assert_eq!(p.read(0x20_0004, 4).unwrap(), 0);
    }

    #[test]
    fn complete_repends_while_level_high() {
        let mut p = Plic::new();
        setup(&mut p);
        p.set_irq(UART, true);
        assert_eq!(p.read(0x20_0004, 4).unwrap(), UART as u64);
        // While claimed, a still-high level does not re-pend.
        p.set_irq(UART, true);
        assert_eq!(p.pending, 0);
        assert!(!p.eip(0));
        // Complete with level still high -> pending again.
        p.write(0x20_0004, UART as u64, 4).unwrap();
        assert_eq!(p.pending, 1 << UART);
        assert!(p.eip(0));
        // Claim, drop the level, complete -> stays clear.
        assert_eq!(p.read(0x20_0004, 4).unwrap(), UART as u64);
        p.set_irq(UART, false);
        p.write(0x20_0004, UART as u64, 4).unwrap();
        assert_eq!(p.pending, 0);
    }

    #[test]
    fn eip_gated_by_threshold() {
        let mut p = Plic::new();
        setup(&mut p);
        p.set_irq(UART, true);
        assert!(p.eip(0)); // priority 3 > threshold 0
        p.write(0x20_0000, 3, 4).unwrap(); // threshold == priority
        assert!(!p.eip(0));
        p.write(0x20_0000, 2, 4).unwrap();
        assert!(p.eip(0));
        // Priority 0 sources never signal.
        p.write((UART as u64) * 4, 0, 4).unwrap();
        p.write(0x20_0000, 0, 4).unwrap();
        assert!(!p.eip(0));
    }

    #[test]
    fn claim_respects_threshold_like_eip() {
        let mut p = Plic::new();
        setup(&mut p); // priority 3, threshold 0
        p.set_irq(UART, true);
        // Raise threshold to priority: eip masks it, so claim must too.
        p.write(0x20_0000, 3, 4).unwrap();
        assert!(!p.eip(0));
        assert_eq!(p.read(0x20_0004, 4).unwrap(), 0); // claim yields nothing
        assert_eq!(p.claimed[0], 0); // and nothing was marked claimed
        assert_eq!(p.pending, 1 << UART); // pending untouched
                                          // Lower threshold below priority: now claimable, agreeing with eip.
        p.write(0x20_0000, 2, 4).unwrap();
        assert!(p.eip(0));
        assert_eq!(p.read(0x20_0004, 4).unwrap(), UART as u64);
    }

    #[test]
    fn complete_unclaimed_id_is_noop() {
        let mut p = Plic::new();
        setup(&mut p);
        p.set_irq(UART, true);
        // Complete a source that was never claimed: must not touch pending.
        assert_eq!(p.pending, 1 << UART);
        p.write(0x20_0004, UART as u64, 4).unwrap();
        assert_eq!(p.pending, 1 << UART);
        assert_eq!(p.claimed[0], 0);
        // A bogus id completion is also inert.
        p.write(0x20_0004, 7, 4).unwrap();
        assert_eq!(p.claimed[0], 0);
    }

    #[test]
    fn completion_from_wrong_context_does_not_release_source() {
        let mut p = Plic::new();
        setup(&mut p);
        p.set_irq(UART, true);
        assert_eq!(p.read(0x20_0004, 4).unwrap(), UART as u64);

        p.write(0x20_1004, UART as u64, 4).unwrap();
        assert_eq!(p.claimed[0], 1 << UART);
        assert_eq!(p.pending, 0);

        p.write(0x20_0004, UART as u64, 4).unwrap();
        assert_eq!(p.claimed[0], 0);
        assert_eq!(p.pending, 1 << UART);
    }

    #[test]
    fn per_context_claim_does_not_couple_sources() {
        let mut p = Plic::new();
        // Source 10 (UART) prio 3, source 7 prio 5; both enabled on both ctxs.
        p.write((UART as u64) * 4, 3, 4).unwrap();
        p.write(7 * 4, 5, 4).unwrap();
        let mask = (1 << UART) | (1 << 7);
        p.write(0x2000, mask, 4).unwrap(); // enable ctx 0
        p.write(0x2080, mask, 4).unwrap(); // enable ctx 1
        p.write(0x20_0000, 0, 4).unwrap(); // ctx0 threshold 0
        p.write(0x20_1000, 0, 4).unwrap(); // ctx1 threshold 0
        p.set_irq(UART, true);
        p.set_irq(7, true);
        // M-context (ctx 0) claims the highest-priority source (7).
        assert_eq!(p.read(0x20_0004, 4).unwrap(), 7);
        assert_eq!(p.claimed[0], 1 << 7);
        // S-context (ctx 1) can still claim the other source (UART).
        assert_eq!(p.read(0x20_1004, 4).unwrap(), UART as u64);
        assert_eq!(p.claimed[1], 1 << UART);
        // Each context completes only what it owns.
        p.set_irq(7, false);
        p.set_irq(UART, false);
        p.write(0x20_0004, 7, 4).unwrap();
        p.write(0x20_1004, UART as u64, 4).unwrap();
        assert_eq!(p.claimed[0], 0);
        assert_eq!(p.claimed[1], 0);
    }
}
