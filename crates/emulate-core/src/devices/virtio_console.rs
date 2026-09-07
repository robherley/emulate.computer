//! Single-port VirtIO console for the guest control service, separate from UART.

use std::collections::VecDeque;

use crate::devices::virtio_mmio::{Transport, TransportWrite};
use crate::devices::virtqueue::{
    self, ChainOutcome, Descriptor, QueueLayout, DESC_F_INDIRECT, DESC_F_WRITE,
};
use crate::ram::Ram;
use crate::snapshot::{Reader, Writer};

const CAPACITY: usize = 64 * 1024;

pub struct VirtioConsole {
    input: VecDeque<u8>,
    output: VecDeque<u8>,
    transport: Transport,
    chain: Vec<Descriptor>,
}

impl Default for VirtioConsole {
    fn default() -> Self {
        Self {
            input: VecDeque::new(),
            output: VecDeque::new(),
            transport: Transport::new(2, 128),
            chain: Vec::new(),
        }
    }
}

impl VirtioConsole {
    /// Enqueue a complete host message, or reject it without consuming any bytes.
    pub fn input(&mut self, bytes: &[u8], ram: &mut Ram) -> bool {
        if bytes.len() > CAPACITY - self.input.len() {
            return false;
        }
        self.input.extend(bytes);
        self.process_queue(0, ram);
        true
    }

    pub fn output(&mut self, ram: &mut Ram) -> Vec<u8> {
        let bytes = self.output.drain(..).collect();
        self.process_queue(1, ram);
        bytes
    }

    pub fn irq_pending(&self) -> bool {
        self.transport.irq_pending()
    }

    pub fn read(&self, offset: u64, size: u8) -> Result<u64, ()> {
        if size != 4 && size != 8 {
            return Err(());
        }
        Ok(self.transport.read(offset, 3, [0, 1]).unwrap_or(0))
    }

    pub fn write(&mut self, offset: u64, val: u64, size: u8, ram: &mut Ram) -> Result<(), ()> {
        if size != 4 {
            return Err(());
        }
        match self.transport.write(offset, val as u32) {
            TransportWrite::Notify(queue @ 0..=1) => self.process_queue(queue as usize, ram),
            TransportWrite::Reset => {
                self.input.clear();
                self.output.clear();
            }
            TransportWrite::Unhandled if offset == 0x108 && self.output.len() < CAPACITY => {
                self.output.push_back(val as u8)
            }
            _ => {}
        }
        Ok(())
    }

    fn process_queue(&mut self, queue: usize, ram: &mut Ram) {
        let work = self.transport.queue(queue).drain(ram, |ram, layout, head| {
            let mut chain = std::mem::take(&mut self.chain);
            let result = self.process_chain(queue, ram, layout, head, &mut chain);
            chain.clear();
            self.chain = chain;
            result
        });
        self.transport.complete_queue(work);
    }

    fn process_chain(
        &mut self,
        queue: usize,
        ram: &mut Ram,
        layout: QueueLayout,
        head: u16,
        chain: &mut Vec<Descriptor>,
    ) -> ChainOutcome {
        if !virtqueue::collect_chain(ram, layout, head, chain)
            || chain.is_empty()
            || chain.iter().any(|d| {
                d.flags & DESC_F_INDIRECT != 0
                    || (d.flags & DESC_F_WRITE != 0) != (queue == 0)
                    || virtqueue::ram_offset(ram, d.addr, u64::from(d.len)).is_none()
            })
        {
            return ChainOutcome::Skip;
        }
        let Some(total) = virtqueue::total_len(chain).filter(|n| *n > 0 && *n <= CAPACITY) else {
            return ChainOutcome::Skip;
        };
        if (queue == 0 && self.input.is_empty())
            || (queue == 1 && total > CAPACITY - self.output.len())
        {
            return ChainOutcome::Retry;
        }
        let count = if queue == 0 {
            total.min(self.input.len())
        } else {
            total
        };
        let mut remaining = count;
        for d in chain {
            let len = remaining.min(d.len as usize);
            let Some(offset) = virtqueue::ram_offset(ram, d.addr, len as u64) else {
                return ChainOutcome::Skip;
            };
            if queue == 0 {
                let bytes = &self.input.make_contiguous()[..len];
                if ram.write_slice(offset, bytes).is_err() {
                    return ChainOutcome::Skip;
                }
                self.input.drain(..len);
            } else {
                let Ok(bytes) = ram.read_slice(offset, len) else {
                    return ChainOutcome::Skip;
                };
                self.output.extend(bytes);
            }
            remaining -= len;
            if remaining == 0 {
                break;
            }
        }
        ChainOutcome::Used(if queue == 0 { count as u32 } else { 0 })
    }

    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(1, |w| {
            w.bytes(&self.input.iter().copied().collect::<Vec<_>>());
            w.bytes(&self.output.iter().copied().collect::<Vec<_>>());
        });
        self.transport.snapshot(out);
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("virtio-console", 1, |r| {
            let incoming = r.bytes()?;
            let outgoing = r.bytes()?;
            if incoming.len() > CAPACITY || outgoing.len() > CAPACITY {
                return Err("console buffer exceeds capacity".into());
            }
            self.input = incoming.iter().copied().collect();
            self.output = outgoing.iter().copied().collect();
            Ok(())
        })?;
        self.chain.clear();
        self.transport.restore(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::DRAM_BASE;

    fn configure(dev: &mut VirtioConsole, ram: &mut Ram, queue: u64, flags: u16, length: u32) {
        for (reg, value) in [
            (0x030, queue),
            (0x038, 8),
            (0x080, DRAM_BASE + 0x1000),
            (0x090, DRAM_BASE + 0x2000),
            (0x0a0, DRAM_BASE + 0x3000),
            (0x044, 1),
        ] {
            dev.write(reg, value, 4, ram).unwrap();
        }
        ram.write(0x1000, DRAM_BASE + 0x4000, 8).unwrap();
        ram.write(0x1008, length.into(), 4).unwrap();
        ram.write(0x100c, flags.into(), 2).unwrap();
        ram.write(0x2002, 1, 2).unwrap();
    }

    #[test]
    fn receive_waits_for_input_then_writes_and_interrupts() {
        let mut dev = VirtioConsole::default();
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, 0, DESC_F_WRITE, 4);
        dev.write(0x050, 0, 4, &mut ram).unwrap();
        assert_eq!(ram.read(0x3002, 2), Ok(0));
        assert!(dev.input(b"hello", &mut ram));
        assert_eq!(ram.read_slice(0x4000, 4).unwrap(), b"hell");
        assert_eq!(ram.read(0x3008, 4), Ok(4));
        assert!(dev.irq_pending());
        assert_eq!(dev.input.len(), 1);
    }

    #[test]
    fn transmit_retries_after_host_drains_full_buffer() {
        let mut dev = VirtioConsole::default();
        let mut ram = Ram::new(0x10000);
        configure(&mut dev, &mut ram, 1, 0, 4);
        ram.write_slice(0x4000, b"okay").unwrap();
        dev.output.resize(CAPACITY, 0);
        dev.write(0x050, 1, 4, &mut ram).unwrap();
        assert_eq!(ram.read(0x3002, 2), Ok(0));
        assert_eq!(dev.output(&mut ram).len(), CAPACITY);
        assert_eq!(dev.output(&mut ram), b"okay");
        assert_eq!(ram.read(0x3002, 2), Ok(1));
        assert_eq!(ram.read(0x3008, 4), Ok(0));
    }

    #[test]
    fn invalid_chains_cannot_consume_input_or_read_mmio() {
        for (queue, flags, address) in [
            (0, 0, DRAM_BASE + 0x4000),
            (1, DESC_F_WRITE, DRAM_BASE + 0x4000),
            (1, 0, 0x1000_0000),
        ] {
            let mut dev = VirtioConsole::default();
            let mut ram = Ram::new(0x10000);
            configure(&mut dev, &mut ram, queue, flags, 4);
            ram.write(0x1000, address, 8).unwrap();
            dev.input.extend(b"keep");
            dev.write(0x050, queue, 4, &mut ram).unwrap();
            assert_eq!(dev.input.len(), 4);
            assert!(dev.output(&mut ram).is_empty());
            assert_eq!(ram.read(0x4000, 4), Ok(0));
        }
    }

    #[test]
    fn snapshot_preserves_pending_bytes_and_reset_discards_them() {
        let mut dev = VirtioConsole::default();
        let mut ram = Ram::new(0x10000);
        assert!(dev.input(b"request", &mut ram));
        assert!(!dev.input(&vec![0; CAPACITY], &mut ram));
        dev.output.extend(b"reply");
        let mut writer = Writer::new();
        dev.snapshot(&mut writer);
        let bytes = writer.into_bytes();
        let mut restored = VirtioConsole::default();
        restored.restore(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(restored.input, dev.input);
        assert_eq!(restored.output(&mut ram), b"reply");
        restored.write(0x070, 0, 4, &mut ram).unwrap();
        assert!(restored.input.is_empty());
        assert!(restored.output(&mut ram).is_empty());
    }
}
