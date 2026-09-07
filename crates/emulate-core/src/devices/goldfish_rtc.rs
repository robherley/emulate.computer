//! Goldfish real-time clock used by the QEMU `virt` machine.

use crate::snapshot::{Reader, Writer};

const TIME_LOW: u64 = 0x00;
const TIME_HIGH: u64 = 0x04;
const ALARM_LOW: u64 = 0x08;
const ALARM_HIGH: u64 = 0x0c;
const IRQ_ENABLED: u64 = 0x10;
const CLEAR_ALARM: u64 = 0x14;
const ALARM_STATUS: u64 = 0x18;
const CLEAR_INTERRUPT: u64 = 0x1c;

/// Nanoseconds in one 10 MHz platform timer tick.
const NS_PER_MTIME_TICK: u64 = 100;

/// Minimal Goldfish RTC register model sufficient for Linux wall-clock setup.
pub struct GoldfishRtc {
    time_ns: u64,
    alarm_ns: u64,
    irq_enabled: bool,
    alarm_armed: bool,
    interrupt_pending: bool,
}

impl GoldfishRtc {
    pub fn new() -> Self {
        Self {
            time_ns: 0,
            alarm_ns: 0,
            irq_enabled: false,
            alarm_armed: false,
            interrupt_pending: false,
        }
    }

    pub fn set_time_ns(&mut self, time_ns: u64) {
        self.time_ns = time_ns;
        self.update_alarm();
    }

    pub fn advance_mtime_ticks(&mut self, ticks: u64) {
        self.time_ns = self
            .time_ns
            .wrapping_add(ticks.wrapping_mul(NS_PER_MTIME_TICK));
        self.update_alarm();
    }

    pub fn irq_pending(&self) -> bool {
        self.irq_enabled && self.interrupt_pending
    }

    pub fn read(&self, offset: u64, size: u8) -> Result<u64, ()> {
        if size != 4 {
            return Err(());
        }
        match offset {
            TIME_LOW => Ok(self.time_ns as u32 as u64),
            TIME_HIGH => Ok((self.time_ns >> 32) as u32 as u64),
            ALARM_LOW => Ok(self.alarm_ns as u32 as u64),
            ALARM_HIGH => Ok((self.alarm_ns >> 32) as u32 as u64),
            IRQ_ENABLED => Ok(u64::from(self.irq_enabled)),
            ALARM_STATUS => Ok(u64::from(self.alarm_armed)),
            CLEAR_ALARM | CLEAR_INTERRUPT => Ok(0),
            _ => Err(()),
        }
    }

    pub fn write(&mut self, offset: u64, value: u64, size: u8) -> Result<(), ()> {
        if size != 4 {
            return Err(());
        }
        let value = value as u32 as u64;
        let update_alarm = match offset {
            TIME_LOW | ALARM_LOW => true,
            TIME_HIGH | ALARM_HIGH => false,
            IRQ_ENABLED | CLEAR_ALARM | ALARM_STATUS | CLEAR_INTERRUPT => false,
            _ => return Err(()),
        };
        match offset {
            TIME_LOW => self.time_ns = (self.time_ns & !u32::MAX as u64) | value,
            TIME_HIGH => self.time_ns = (self.time_ns & u32::MAX as u64) | (value << 32),
            ALARM_LOW => {
                self.alarm_ns = (self.alarm_ns & !u32::MAX as u64) | value;
                self.alarm_armed = true;
                self.interrupt_pending = false;
            }
            ALARM_HIGH => self.alarm_ns = (self.alarm_ns & u32::MAX as u64) | (value << 32),
            IRQ_ENABLED => self.irq_enabled = value != 0,
            CLEAR_ALARM => {
                self.alarm_armed = false;
                self.interrupt_pending = false;
            }
            CLEAR_INTERRUPT => self.interrupt_pending = false,
            ALARM_STATUS => {}
            _ => unreachable!(),
        }
        if update_alarm {
            self.update_alarm();
        }
        Ok(())
    }

    fn update_alarm(&mut self) {
        if self.alarm_armed && self.time_ns >= self.alarm_ns {
            self.interrupt_pending = true;
        }
    }
}

impl Default for GoldfishRtc {
    fn default() -> Self {
        Self::new()
    }
}

const SNAPSHOT_VERSION: u16 = 1;

impl GoldfishRtc {
    /// Guest wall clock, in Unix epoch nanoseconds. Recorded in a snapshot's
    /// header for diagnostics; a restore re-seeds it from the host instead.
    pub fn time_ns(&self) -> u64 {
        self.time_ns
    }

    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            w.u64(self.time_ns);
            w.u64(self.alarm_ns);
            w.bool(self.irq_enabled);
            w.bool(self.alarm_armed);
            w.bool(self.interrupt_pending);
        });
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("rtc", SNAPSHOT_VERSION, |r| {
            self.time_ns = r.u64()?;
            self.alarm_ns = r.u64()?;
            self.irq_enabled = r.bool()?;
            self.alarm_armed = r.bool()?;
            self.interrupt_pending = r.bool()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_registers_expose_unix_nanoseconds() {
        let mut rtc = GoldfishRtc::new();
        rtc.set_time_ns(0x1122_3344_5566_7788);

        assert_eq!(rtc.read(TIME_LOW, 4), Ok(0x5566_7788));
        assert_eq!(rtc.read(TIME_HIGH, 4), Ok(0x1122_3344));
    }

    #[test]
    fn mtime_ticks_advance_realtime_at_ten_megahertz() {
        let mut rtc = GoldfishRtc::new();
        rtc.set_time_ns(1_000);
        rtc.advance_mtime_ticks(25);

        assert_eq!(rtc.read(TIME_LOW, 4), Ok(3_500));
    }

    #[test]
    fn armed_alarm_raises_and_clears_interrupt() {
        let mut rtc = GoldfishRtc::new();
        rtc.write(ALARM_HIGH, 0, 4).unwrap();
        rtc.write(ALARM_LOW, 1_000, 4).unwrap();
        rtc.write(IRQ_ENABLED, 1, 4).unwrap();
        rtc.advance_mtime_ticks(10);
        assert!(rtc.irq_pending());

        rtc.write(CLEAR_INTERRUPT, 1, 4).unwrap();
        assert!(!rtc.irq_pending());
    }
}
