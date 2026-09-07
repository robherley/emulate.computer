//! VirtIO-MMIO registers, queue completion bookkeeping, and transport snapshots.

use super::virtqueue::{self, ChainOutcome, Queue, QueueLayout};
use crate::ram::Ram;
use crate::snapshot::{Reader, Writer};

pub(crate) const MAGIC: u32 = 0x7472_6976;
pub(crate) const VERSION: u32 = 2;
pub(crate) const VENDOR_ID_QEMU: u32 = 0x554d_4551;

/// Owns a queue copy so its handler can borrow device state during the drain.
#[must_use = "commit drained queue state with Transport::complete_queue"]
pub(crate) struct QueueWork {
    index: usize,
    queue: Queue,
    published: bool,
}

impl QueueWork {
    pub(crate) fn drain(
        mut self,
        ram: &mut Ram,
        handler: impl FnMut(&mut Ram, QueueLayout, u16) -> ChainOutcome,
    ) -> Self {
        self.published |= virtqueue::drain(&mut self.queue, ram, handler);
        self
    }
}

/// What a transport register write asked the device to do.
pub(crate) enum TransportWrite {
    /// Fully handled by the transport.
    Handled,
    /// QueueNotify: the value is the guest-supplied queue index.
    Notify(u32),
    /// Status was written zero: the transport reset itself and the device
    /// should clear any state of its own.
    Reset,
    /// Not a transport register (device configuration space).
    Unhandled,
}

/// The VirtIO-MMIO register file shared by every device in this crate.
pub(crate) struct Transport {
    queues: Vec<Queue>,
    queue_size_max: u16,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: [u32; 2],
    queue_sel: u32,
    status: u32,
    interrupt_status: u32,
}

impl Transport {
    pub(crate) fn new(queues: usize, queue_size_max: u16) -> Self {
        Self {
            queues: vec![Queue::default(); queues],
            queue_size_max,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: [0; 2],
            queue_sel: 0,
            status: 0,
            interrupt_status: 0,
        }
    }

    pub(crate) fn queue(&self, index: usize) -> QueueWork {
        QueueWork {
            index,
            queue: self.queues[index],
            published: false,
        }
    }

    pub(crate) fn complete_queue(&mut self, work: QueueWork) {
        self.queues[work.index] = work.queue;
        if work.published {
            self.interrupt_status |= 1;
        }
    }

    pub(crate) fn irq_pending(&self) -> bool {
        self.interrupt_status != 0
    }

    /// Reads a transport register. `features` is the device's offered feature
    /// bitmap split into 32-bit words, selected by DeviceFeaturesSel.
    /// Returns `None` for offsets the device owns (configuration space).
    pub(crate) fn read(&self, offset: u64, device_id: u32, features: [u32; 2]) -> Option<u64> {
        let value = match offset {
            0x000 => u64::from(MAGIC),
            0x004 => u64::from(VERSION),
            0x008 => u64::from(device_id),
            0x00c => u64::from(VENDOR_ID_QEMU),
            0x010 => u64::from(
                features
                    .get(self.device_features_sel as usize)
                    .copied()
                    .unwrap_or(0),
            ),
            0x034 => {
                if (self.queue_sel as usize) < self.queues.len() {
                    u64::from(self.queue_size_max)
                } else {
                    0
                }
            }
            0x044 => u64::from(
                self.queues
                    .get(self.queue_sel as usize)
                    .is_some_and(|queue| queue.ready),
            ),
            0x060 => u64::from(self.interrupt_status),
            0x070 => u64::from(self.status),
            _ => return None,
        };
        Some(value)
    }

    /// Applies a transport register write.
    pub(crate) fn write(&mut self, offset: u64, val: u32) -> TransportWrite {
        match offset {
            0x014 => self.device_features_sel = val,
            0x020 => {
                if let Some(slot) = self
                    .driver_features
                    .get_mut(self.driver_features_sel as usize)
                {
                    *slot = val;
                }
            }
            0x024 => self.driver_features_sel = val,
            0x030 => self.queue_sel = val,
            0x038 => {
                let max = self.queue_size_max;
                if let Some(queue) = self.queues.get_mut(self.queue_sel as usize) {
                    queue.num = (val as u16).min(max);
                }
            }
            0x044 => {
                if let Some(queue) = self.queues.get_mut(self.queue_sel as usize) {
                    queue.ready = val & 1 != 0;
                }
            }
            0x050 => return TransportWrite::Notify(val),
            0x064 => self.interrupt_status &= !val,
            0x070 => {
                if val == 0 {
                    self.reset();
                    return TransportWrite::Reset;
                }
                self.status = val;
            }
            0x080 => self.with_queue(|queue| set_low(&mut queue.desc, val)),
            0x084 => self.with_queue(|queue| set_high(&mut queue.desc, val)),
            0x090 => self.with_queue(|queue| set_low(&mut queue.avail, val)),
            0x094 => self.with_queue(|queue| set_high(&mut queue.avail, val)),
            0x0a0 => self.with_queue(|queue| set_low(&mut queue.used, val)),
            0x0a4 => self.with_queue(|queue| set_high(&mut queue.used, val)),
            _ => return TransportWrite::Unhandled,
        }
        TransportWrite::Handled
    }

    fn with_queue(&mut self, apply: impl FnOnce(&mut Queue)) {
        if let Some(queue) = self.queues.get_mut(self.queue_sel as usize) {
            apply(queue);
        }
    }

    pub(crate) fn reset(&mut self) {
        self.device_features_sel = 0;
        self.driver_features_sel = 0;
        self.driver_features = [0; 2];
        self.queue_sel = 0;
        self.status = 0;
        self.interrupt_status = 0;
        for queue in &mut self.queues {
            *queue = Queue::default();
        }
    }
}

fn set_low(dst: &mut u64, val: u32) {
    *dst = (*dst & 0xffff_ffff_0000_0000) | u64::from(val);
}

fn set_high(dst: &mut u64, val: u32) {
    *dst = (*dst & 0x0000_0000_ffff_ffff) | (u64::from(val) << 32);
}

const SNAPSHOT_VERSION: u16 = 1;

impl Transport {
    /// Serialize the MMIO register file and every queue's guest-programmed
    /// state. The descriptor/available/used rings themselves live in guest RAM
    /// and are carried by the RAM section, so only the pointers are here.
    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            w.u32(self.queues.len() as u32);
            w.u16(self.queue_size_max);
            for queue in &self.queues {
                w.u16(queue.num);
                w.bool(queue.ready);
                w.u64(queue.desc);
                w.u64(queue.avail);
                w.u64(queue.used);
                w.u16(queue.last_avail_idx);
            }
            w.u32(self.device_features_sel);
            w.u32(self.driver_features_sel);
            w.u32(self.driver_features[0]);
            w.u32(self.driver_features[1]);
            w.u32(self.queue_sel);
            w.u32(self.status);
            w.u32(self.interrupt_status);
        });
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("virtio-transport", SNAPSHOT_VERSION, |r| {
            let queues = r.u32()? as usize;
            let queue_size_max = r.u16()?;
            if queues != self.queues.len() || queue_size_max != self.queue_size_max {
                return Err(format!(
                    "{queues} queues of max {queue_size_max}, this device has {} of max {}",
                    self.queues.len(),
                    self.queue_size_max
                ));
            }
            for queue in self.queues.iter_mut() {
                queue.num = r.u16()?;
                queue.ready = r.bool()?;
                queue.desc = r.u64()?;
                queue.avail = r.u64()?;
                queue.used = r.u64()?;
                queue.last_avail_idx = r.u16()?;
            }
            self.device_features_sel = r.u32()?;
            self.driver_features_sel = r.u32()?;
            self.driver_features = [r.u32()?, r.u32()?];
            self.queue_sel = r.u32()?;
            self.status = r.u32()?;
            self.interrupt_status = r.u32()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::DRAM_BASE;

    const DESC: u64 = DRAM_BASE + 0x1000;
    const AVAIL: u64 = DRAM_BASE + 0x2000;
    const USED: u64 = DRAM_BASE + 0x3000;

    fn configure(transport: &mut Transport, index: u32) {
        for (offset, value) in [
            (0x030, index),
            (0x038, 4),
            (0x080, DESC as u32),
            (0x090, AVAIL as u32),
            (0x0a0, USED as u32),
            (0x044, 1),
        ] {
            transport.write(offset, value);
        }
    }

    #[test]
    fn queue_completion_preserves_retry_progress_and_interrupts_the_correct_queue() {
        let mut transport = Transport::new(2, 8);
        configure(&mut transport, 1);
        let mut ram = Ram::new(0x4000);
        virtqueue::write_u16(&mut ram, AVAIL + 2, 3).unwrap();
        for head in 0..3 {
            virtqueue::write_u16(&mut ram, AVAIL + 4 + u64::from(head) * 2, head).unwrap();
        }
        let work = transport.queue(1).drain(&mut ram, |_, _, head| match head {
            0 => ChainOutcome::Skip,
            1 => ChainOutcome::Used(8),
            _ => ChainOutcome::Retry,
        });
        transport.write(0x030, 0);
        transport.complete_queue(work);
        assert_eq!(transport.queues[0].last_avail_idx, 0);
        assert_eq!(transport.queues[1].last_avail_idx, 2);
        assert_eq!(virtqueue::read_u16(&ram, USED + 2), Some(1));
        assert_eq!(virtqueue::read_u32(&ram, USED + 4), Some(1));
        assert!(transport.irq_pending());
        transport.write(0x064, 1);
        let work = transport.queue(1).drain(&mut ram, |_, _, head| {
            assert_eq!(head, 2);
            ChainOutcome::Used(16)
        });
        transport.complete_queue(work);
        assert_eq!(transport.queues[1].last_avail_idx, 3);
        assert_eq!(virtqueue::read_u16(&ram, USED + 2), Some(2));
        assert!(transport.irq_pending());
        transport.write(0x064, 1);
        let work = transport
            .queue(1)
            .drain(&mut ram, |_, _, _| panic!("empty queue"));
        transport.complete_queue(work);
        assert!(!transport.irq_pending());
    }

    #[test]
    fn version_one_snapshot_preserves_negotiated_queue_state() {
        // Block transport section from a booted Linux desktop snapshot.
        let bytes = [
            0x01, 0x00, 0x3f, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x08, 0x00, 0x08, 0x00,
            0x01, 0x00, 0x00, 0x8b, 0x82, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x8b, 0x82, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x10, 0x8b, 0x82, 0x00, 0x00, 0x00, 0x00, 0x39, 0x09, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x02, 0x00, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut transport = Transport::new(1, 8);
        transport.restore(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(transport.read(0x070, 2, [0, 1]), Some(15));
        assert!(transport.queues[0].ready);
        assert_eq!(transport.queues[0].last_avail_idx, 2361);
        assert_eq!(transport.queues[0].desc, 0x828b_0000);
        let mut out = Writer::new();
        transport.snapshot(&mut out);
        assert_eq!(out.into_bytes(), bytes);
        for mut incompatible in [Transport::new(2, 8), Transport::new(1, 16)] {
            assert!(incompatible.restore(&mut Reader::new(&bytes)).is_err());
        }
    }

    #[test]
    fn invalid_queue_selection_and_reset_do_not_leak_queue_state() {
        let mut transport = Transport::new(2, 8);
        configure(&mut transport, 1);
        transport.write(0x030, u32::MAX);
        transport.write(0x038, 1);
        transport.write(0x080, 0);
        assert_eq!(transport.read(0x034, 4, [0, 1]), Some(0));
        assert_eq!(transport.queues[1].num, 4);
        assert_eq!(transport.queues[1].desc, DESC);
        assert!(matches!(transport.write(0x070, 0), TransportWrite::Reset));
        assert_eq!(transport.queue_sel, 0);
        for queue in &transport.queues {
            assert!(!queue.ready);
            assert_eq!(queue.desc, 0);
            assert_eq!(queue.last_avail_idx, 0);
        }
    }
}
