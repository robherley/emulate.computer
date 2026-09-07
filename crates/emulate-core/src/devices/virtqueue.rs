//! Split-virtqueue traversal, descriptor validation, and used-ring publication.

use crate::bus::DRAM_BASE;
use crate::ram::Ram;

pub(crate) const DESC_F_NEXT: u16 = 1;
pub(crate) const DESC_F_WRITE: u16 = 2;
pub(crate) const DESC_F_INDIRECT: u16 = 4;

#[derive(Clone, Copy)]
pub(crate) struct Descriptor {
    pub(crate) addr: u64,
    pub(crate) len: u32,
    pub(crate) flags: u16,
    pub(crate) next: u16,
}

/// Guest-programmed state of one split virtqueue.
///
/// `Copy` so device code can lift the queue out of the transport for a
/// [`drain`], letting the chain handler borrow the rest of the device mutably,
/// then store the advanced copy back.
#[derive(Clone, Copy, Default)]
pub(crate) struct Queue {
    pub(crate) num: u16,
    pub(crate) ready: bool,
    pub(crate) desc: u64,
    pub(crate) avail: u64,
    pub(crate) used: u64,
    pub(crate) last_avail_idx: u16,
}

impl Queue {
    /// The immutable half of the queue a chain handler needs.
    pub(crate) fn layout(&self) -> QueueLayout {
        QueueLayout {
            desc: self.desc,
            num: self.num,
        }
    }
}

/// Descriptor-table location and size, handed to chain handlers by value so a
/// handler may borrow its device mutably while the queue is being drained.
#[derive(Clone, Copy)]
pub(crate) struct QueueLayout {
    pub(crate) desc: u64,
    pub(crate) num: u16,
}

/// What a device decided about one available chain.
pub(crate) enum ChainOutcome {
    /// Publish a used entry of this length and raise the interrupt.
    Used(u32),
    /// Consume the chain without publishing anything: nothing could be
    /// reported to the driver, but the queue must not wedge on it either.
    Skip,
    /// Leave the chain available and stop draining; a later call retries it
    /// (the entropy device parks requests this way until bytes arrive).
    Retry,
}

/// Consumes available chains from `queue`, bounded to one ring's worth of
/// work, calling `handler` once per chain and publishing used entries for the
/// outcomes that ask for one. Returns true when at least one used entry was
/// published, i.e. when the caller should raise its interrupt.
///
/// `avail_idx` is guest-controlled, and per the virtio spec a driver may never
/// have more than `queue_num` chains outstanding. A larger backlog is a driver
/// protocol error, so nothing is processed and `last_avail_idx` is left alone
/// for a later sane notify; replaying up to 65,535 requests inside one MMIO
/// store would hang the browser worker. The loop is separately capped at
/// `queue_num` iterations.
pub(crate) fn drain(
    queue: &mut Queue,
    ram: &mut Ram,
    mut handler: impl FnMut(&mut Ram, QueueLayout, u16) -> ChainOutcome,
) -> bool {
    let qn = queue.num;
    if !queue.ready || qn == 0 || queue.desc == 0 || queue.avail == 0 || queue.used == 0 {
        return false;
    }
    // Validate complete spans before adding guest-controlled ring offsets.
    if ram_offset(ram, queue.desc, u64::from(qn) * 16).is_none()
        || ram_offset(ram, queue.avail, 4 + u64::from(qn) * 2).is_none()
        || ram_offset(ram, queue.used, 4 + u64::from(qn) * 8).is_none()
    {
        return false;
    }
    let Some(avail_idx) = read_u16(ram, queue.avail + 2) else {
        return false;
    };
    if avail_idx.wrapping_sub(queue.last_avail_idx) > qn {
        return false;
    }
    let layout = queue.layout();
    let mut published = false;
    for _ in 0..qn {
        if queue.last_avail_idx == avail_idx {
            break;
        }
        let slot = u64::from(queue.last_avail_idx % qn);
        let Some(head) = read_u16(ram, queue.avail + 4 + slot * 2) else {
            return published;
        };
        // Check the used entry is writable before doing any work: a device
        // must never perform an operation whose completion it cannot report.
        let Some(used_idx) = read_u16(ram, queue.used + 2) else {
            return published;
        };
        let used_slot = u64::from(used_idx % qn);
        if ram_offset(ram, queue.used + 4 + used_slot * 8, 8).is_none() {
            return published;
        }
        match handler(ram, layout, head) {
            ChainOutcome::Retry => break,
            ChainOutcome::Skip => {
                queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
            }
            ChainOutcome::Used(used_len) => {
                if write_u32(ram, queue.used + 4 + used_slot * 8, u32::from(head)).is_none()
                    || write_u32(ram, queue.used + 8 + used_slot * 8, used_len).is_none()
                    || write_u16(ram, queue.used + 2, used_idx.wrapping_add(1)).is_none()
                {
                    return published;
                }
                queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
                published = true;
            }
        }
    }
    published
}

pub(crate) fn ram_offset(ram: &Ram, addr: u64, len: u64) -> Option<u64> {
    let offset = addr.checked_sub(DRAM_BASE)?;
    (offset.checked_add(len)? <= ram.size()).then_some(offset)
}

pub(crate) fn read_u16(ram: &Ram, addr: u64) -> Option<u16> {
    Some(ram.read(ram_offset(ram, addr, 2)?, 2).ok()? as u16)
}

pub(crate) fn read_u32(ram: &Ram, addr: u64) -> Option<u32> {
    Some(ram.read(ram_offset(ram, addr, 4)?, 4).ok()? as u32)
}

pub(crate) fn read_u64(ram: &Ram, addr: u64) -> Option<u64> {
    ram.read(ram_offset(ram, addr, 8)?, 8).ok()
}

pub(crate) fn write_u16(ram: &mut Ram, addr: u64, val: u16) -> Option<()> {
    ram.write(ram_offset(ram, addr, 2)?, u64::from(val), 2).ok()
}

pub(crate) fn write_u32(ram: &mut Ram, addr: u64, val: u32) -> Option<()> {
    ram.write(ram_offset(ram, addr, 4)?, u64::from(val), 4).ok()
}

pub(crate) fn descriptor(ram: &Ram, table: u64, queue_num: u16, index: u16) -> Option<Descriptor> {
    if index >= queue_num {
        return None;
    }
    let addr = table.checked_add(u64::from(index) * 16)?;
    ram_offset(ram, addr, 16)?;
    Some(Descriptor {
        addr: read_u64(ram, addr)?,
        len: read_u32(ram, addr + 8)?,
        flags: read_u16(ram, addr + 12)?,
        next: read_u16(ram, addr + 14)?,
    })
}

/// Walks a descriptor chain from `head` into `chain`, returning false if the
/// chain contains an out-of-range index or fails to terminate within
/// `queue_num` descriptors (a cycle, or a chain longer than the queue).
pub(crate) fn collect_chain(
    ram: &Ram,
    layout: QueueLayout,
    head: u16,
    chain: &mut Vec<Descriptor>,
) -> bool {
    chain.clear();
    let mut index = head;
    for _ in 0..layout.num {
        let Some(desc) = descriptor(ram, layout.desc, layout.num, index) else {
            return false;
        };
        chain.push(desc);
        if desc.flags & DESC_F_NEXT == 0 {
            return true;
        }
        index = desc.next;
    }
    false
}

/// Total byte length of a descriptor list, or `None` on overflow.
pub(crate) fn total_len(chain: &[Descriptor]) -> Option<usize> {
    chain
        .iter()
        .try_fold(0usize, |total, desc| total.checked_add(desc.len as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_ring_spans_do_not_execute_requests() {
        let mut ram = Ram::new(4096);
        write_u16(&mut ram, DRAM_BASE + 66, 1).unwrap();
        let valid = Queue {
            num: 1,
            ready: true,
            desc: DRAM_BASE,
            avail: DRAM_BASE + 64,
            used: DRAM_BASE + 128,
            last_avail_idx: 0,
        };
        for address in [u64::MAX, u64::MAX - 7, DRAM_BASE - 1, DRAM_BASE + 4095] {
            for field in 0..3 {
                let mut queue = valid;
                match field {
                    0 => queue.desc = address,
                    1 => queue.avail = address,
                    _ => queue.used = address,
                }
                assert!(!drain(&mut queue, &mut ram, |_, _, _| panic!(
                    "invalid DMA"
                )));
                assert_eq!(queue.last_avail_idx, 0);
            }
            assert!(descriptor(&ram, address, 1, 0).is_none());
        }
        let mut queue = valid;
        assert!(drain(&mut queue, &mut ram, |_, _, _| ChainOutcome::Used(7)));
        assert_eq!(read_u16(&ram, valid.used + 2), Some(1));
    }
}
