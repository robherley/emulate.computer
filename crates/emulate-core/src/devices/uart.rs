//! 16550-compatible UART (byte-wide registers at 1-byte stride, like the
//! QEMU virt `ns16550a`).
//!
//! Registers (offset from UART_BASE):
//! - 0: RBR (read) / THR (write) / DLL when DLAB=1
//! - 1: IER / DLM when DLAB=1
//! - 2: IIR (read) / FCR (write)
//! - 3: LCR (bit 7 = DLAB)
//! - 4: MCR
//! - 5: LSR (bit 0 = data ready, bits 5/6 = THR empty / transmitter idle)
//! - 6: MSR
//! - 7: SCR
//!
//! TX is always ready (bytes append to `tx`); RX comes from `rx` (host pushes
//! with `push_rx`). Interrupts: RX-data-available and THR-empty per IER;
//! `irq_pending()` reflects the IIR state and drives the PLIC line (level).

use crate::snapshot::{Reader, Writer};

use std::collections::VecDeque;

/// IER bit 0: received-data-available interrupt enable.
const IER_RDA: u8 = 0x01;
/// IER bit 1: THR-empty interrupt enable.
const IER_THRE: u8 = 0x02;
/// LCR bit 7: divisor latch access.
const LCR_DLAB: u8 = 0x80;

/// LSR bit 1: overrun error (a byte arrived while the RX FIFO was full).
const LSR_OE: u8 = 0x02;

/// RX FIFO depth. A real 16550 holds 16 bytes, which drops interactive pastes
/// whenever the guest has RX interrupts masked; 4096 survives a paste while
/// still bounding host input. Overflow sets the LSR overrun bit.
const RX_FIFO_CAP: usize = 4096;

pub struct Uart {
    pub rx: VecDeque<u8>,
    pub tx: Vec<u8>,
    pub ier: u8,
    pub lcr: u8,
    pub mcr: u8,
    pub scr: u8,
    pub dll: u8,
    pub dlm: u8,
    /// THR-empty interrupt latched (cleared on IIR read / THR write).
    pub thre_ip: bool,
    /// Overrun error: a byte arrived at a full RX FIFO. Surfaced in LSR bit 1
    /// and cleared on an LSR read, per 16550 semantics.
    pub oe: bool,
}

impl Uart {
    pub fn new() -> Self {
        Uart {
            rx: VecDeque::new(),
            tx: Vec::new(),
            ier: 0,
            lcr: 0,
            mcr: 0,
            scr: 0,
            dll: 0,
            dlm: 0,
            thre_ip: false,
            oe: false,
        }
    }

    /// Host: feed a byte typed by the user. On a full FIFO the arriving byte
    /// is dropped and overrun is flagged, per 16550 semantics.
    pub fn push_rx(&mut self, b: u8) {
        if self.rx.len() >= RX_FIFO_CAP {
            self.oe = true;
            return;
        }
        self.rx.push_back(b);
    }

    /// Host: drain output bytes for the terminal.
    pub fn take_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }

    #[inline]
    fn dlab(&self) -> bool {
        self.lcr & LCR_DLAB != 0
    }

    /// Level of the UART interrupt line into the PLIC.
    pub fn irq_pending(&self) -> bool {
        (self.ier & IER_RDA != 0 && !self.rx.is_empty())
            || (self.ier & IER_THRE != 0 && self.thre_ip)
    }

    pub fn read(&mut self, offset: u64, size: u8) -> Result<u64, ()> {
        // Registers are byte-wide; wider accesses act on the low byte.
        let _ = size;
        let b: u8 = match offset {
            0 if self.dlab() => self.dll,
            0 => self.rx.pop_front().unwrap_or(0), // RBR
            1 if self.dlab() => self.dlm,
            1 => self.ier,
            2 => {
                // IIR: FIFO-enabled bits always set; highest priority first.
                if self.ier & IER_RDA != 0 && !self.rx.is_empty() {
                    0xC4
                } else if self.thre_ip && self.ier & IER_THRE != 0 {
                    // Reading IIR acknowledges the THRE interrupt.
                    self.thre_ip = false;
                    0xC2
                } else {
                    0xC1 // no interrupt pending
                }
            }
            3 => self.lcr,
            4 => self.mcr,
            5 => {
                // LSR: THR-empty/idle (0x60), data-ready (bit 0), overrun
                // (bit 1). Reading LSR clears the overrun latch.
                let mut lsr = 0x60;
                if !self.rx.is_empty() {
                    lsr |= 0x01;
                }
                if self.oe {
                    lsr |= LSR_OE;
                    self.oe = false;
                }
                lsr
            }
            6 => 0xB0, // MSR: CTS|DSR|CD
            7 => self.scr,
            _ => 0,
        };
        Ok(b as u64)
    }

    pub fn write(&mut self, offset: u64, val: u64, size: u8) -> Result<(), ()> {
        let _ = size;
        let b = val as u8;
        match offset {
            0 if self.dlab() => self.dll = b,
            0 => {
                // THR: transmit is instantaneous, so THR is empty again.
                self.tx.push(b);
                self.thre_ip = true;
            }
            1 if self.dlab() => self.dlm = b,
            1 => {
                let new = b & 0x0F;
                // Transmission is instant, so the transmitter is always empty:
                // enabling IER_THRI (0->1) must assert THRE right away rather
                // than wait for the next THR write.
                if new & IER_THRE != 0 && self.ier & IER_THRE == 0 {
                    self.thre_ip = true;
                }
                self.ier = new;
            }
            2 => {} // FCR: FIFOs are always on; ignore
            3 => self.lcr = b,
            4 => self.mcr = b,
            5 | 6 => {} // LSR/MSR read-only
            7 => self.scr = b,
            _ => {}
        }
        Ok(())
    }
}

impl Default for Uart {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// State snapshot
// ---------------------------------------------------------------------------

const SNAPSHOT_VERSION: u16 = 1;

impl Uart {
    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            let rx: Vec<u8> = self.rx.iter().copied().collect();
            w.bytes(&rx);
            w.bytes(&self.tx);
            w.u8(self.ier);
            w.u8(self.lcr);
            w.u8(self.mcr);
            w.u8(self.scr);
            w.u8(self.dll);
            w.u8(self.dlm);
            w.bool(self.thre_ip);
            w.bool(self.oe);
        });
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("uart", SNAPSHOT_VERSION, |r| {
            let rx = r.bytes()?;
            if rx.len() > RX_FIFO_CAP {
                return Err(format!("{} queued RX bytes exceed the FIFO", rx.len()));
            }
            self.rx = rx.iter().copied().collect();
            self.tx = r.bytes()?.to_vec();
            self.ier = r.u8()?;
            self.lcr = r.u8()?;
            self.mcr = r.u8()?;
            self.scr = r.u8()?;
            self.dll = r.u8()?;
            self.dlm = r.u8()?;
            self.thre_ip = r.bool()?;
            self.oe = r.bool()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rx_path() {
        let mut u = Uart::new();
        assert_eq!(u.read(5, 1).unwrap(), 0x60); // LSR: THR empty, no data
        u.push_rx(b'a');
        u.push_rx(b'b');
        assert_eq!(u.read(5, 1).unwrap(), 0x61); // data ready
        assert_eq!(u.read(0, 1).unwrap(), b'a' as u64);
        assert_eq!(u.read(0, 1).unwrap(), b'b' as u64);
        assert_eq!(u.read(5, 1).unwrap(), 0x60); // drained
        assert_eq!(u.read(0, 1).unwrap(), 0); // empty RBR reads 0
    }

    #[test]
    fn tx_path() {
        let mut u = Uart::new();
        u.write(0, b'h' as u64, 1).unwrap();
        u.write(0, b'i' as u64, 1).unwrap();
        assert!(u.thre_ip);
        assert_eq!(u.take_tx(), b"hi");
        assert!(u.take_tx().is_empty());
    }

    #[test]
    fn dlab_redirect() {
        let mut u = Uart::new();
        u.write(3, 0x80, 1).unwrap(); // LCR: DLAB=1
        u.write(0, 0x23, 1).unwrap();
        u.write(1, 0x45, 1).unwrap();
        assert_eq!(u.dll, 0x23);
        assert_eq!(u.dlm, 0x45);
        assert!(u.tx.is_empty()); // did not hit THR
        assert_eq!(u.read(0, 1).unwrap(), 0x23);
        assert_eq!(u.read(1, 1).unwrap(), 0x45);
        u.write(3, 0x03, 1).unwrap(); // DLAB=0, 8n1
        u.write(1, 0x03, 1).unwrap(); // now IER
        assert_eq!(u.ier, 0x03);
        assert_eq!(u.read(1, 1).unwrap(), 0x03);
    }

    #[test]
    fn iir_priority_and_thre_ack() {
        let mut u = Uart::new();
        assert_eq!(u.read(2, 1).unwrap(), 0xC1); // nothing pending
        u.write(1, 0x03, 1).unwrap(); // enable RDA + THRE
        u.write(0, b'x' as u64, 1).unwrap(); // sets thre_ip
        u.push_rx(b'y');
        // RX data outranks THRE.
        assert_eq!(u.read(2, 1).unwrap(), 0xC4);
        assert!(u.thre_ip); // not acknowledged by an RDA-identified read
        u.rx.clear();
        // Now THRE is reported and reading IIR acknowledges it.
        assert_eq!(u.read(2, 1).unwrap(), 0xC2);
        assert!(!u.thre_ip);
        assert_eq!(u.read(2, 1).unwrap(), 0xC1);
    }

    #[test]
    fn thre_reenable_relatches_only_after_disable() {
        let mut u = Uart::new();
        u.write(1, IER_THRE as u64, 1).unwrap();
        assert_eq!(u.read(2, 1).unwrap(), 0xC2);

        u.write(1, IER_THRE as u64, 1).unwrap();
        assert_eq!(u.read(2, 1).unwrap(), 0xC1);

        u.write(1, 0, 1).unwrap();
        u.write(1, IER_THRE as u64, 1).unwrap();
        assert_eq!(u.read(2, 1).unwrap(), 0xC2);
    }

    #[test]
    fn irq_pending_combinations() {
        let mut u = Uart::new();
        assert!(!u.irq_pending());
        // RX data but interrupts disabled.
        u.push_rx(1);
        assert!(!u.irq_pending());
        u.write(1, 0x01, 1).unwrap(); // enable RDA
        assert!(u.irq_pending());
        u.read(0, 1).unwrap(); // pop -> line drops
        assert!(!u.irq_pending());
        // THRE latched but not enabled.
        u.write(0, 0, 1).unwrap();
        assert!(u.thre_ip);
        assert!(!u.irq_pending());
        u.write(1, 0x03, 1).unwrap();
        assert!(u.irq_pending());
        u.read(2, 1).unwrap(); // IIR read acks THRE
        assert!(!u.irq_pending());
    }

    #[test]
    fn misc_registers_and_wide_access() {
        let mut u = Uart::new();
        u.write(4, 0x0B, 1).unwrap();
        assert_eq!(u.read(4, 1).unwrap(), 0x0B); // MCR
        u.write(7, 0x5A, 1).unwrap();
        assert_eq!(u.read(7, 1).unwrap(), 0x5A); // SCR
        assert_eq!(u.read(6, 1).unwrap(), 0xB0); // MSR
                                                 // Wider accesses act on the low byte.
        u.write(0, 0x1234_5641, 4).unwrap();
        assert_eq!(u.take_tx(), vec![0x41]);
    }

    #[test]
    fn thre_asserts_on_ier_enable_while_idle() {
        let mut u = Uart::new();
        // Transmitter is idle (empty) and nothing was written to THR.
        assert!(!u.thre_ip);
        // Enabling IER_THRI (bit 1) 0->1 must assert THRE immediately.
        u.write(1, IER_THRE as u64, 1).unwrap();
        assert!(u.thre_ip);
        assert!(u.irq_pending());
        // IIR reflects THRE and reading it acknowledges.
        assert_eq!(u.read(2, 1).unwrap(), 0xC2);
        assert!(!u.thre_ip);
        // Re-writing IER with bit 1 already set is not a 0->1 edge: no re-arm.
        u.write(1, IER_THRE as u64, 1).unwrap();
        assert!(!u.thre_ip);
    }

    #[test]
    fn rx_overflow_sets_oe_and_drops_byte() {
        let mut u = Uart::new();
        for _ in 0..RX_FIFO_CAP {
            u.push_rx(b'a');
        }
        assert_eq!(u.rx.len(), RX_FIFO_CAP);
        assert!(!u.oe);
        // One more byte overflows: dropped, FIFO unchanged, OE set.
        u.push_rx(b'Z');
        assert_eq!(u.rx.len(), RX_FIFO_CAP);
        assert!(u.oe);
        // LSR reports overrun (bit 1) and reading it clears the latch.
        let lsr = u.read(5, 1).unwrap() as u8;
        assert_eq!(lsr & LSR_OE, LSR_OE);
        assert!(!u.oe);
        assert_eq!(u.read(5, 1).unwrap() as u8 & LSR_OE, 0);
    }
}
