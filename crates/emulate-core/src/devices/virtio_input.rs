//! VirtIO 1.x input device (spec §5.8) over the MMIO transport.
//!
//! Two of these are instantiated: a keyboard advertising `EV_KEY`, and a
//! *tablet* advertising `EV_ABS` `ABS_X`/`ABS_Y` over 0..[`ABS_MAX`] plus the
//! three mouse buttons and a wheel. Absolute coordinates are what a browser
//! hands us (`pointermove` gives a position, not a delta), so the tablet needs
//! no pointer lock, no capture and no relative-motion accumulation — see
//! `docs/display.md`.
//!
//! The device is host→guest on the eventq and guest→host on the statusq. Host
//! events queue here until the guest posts buffers; the backlog is bounded and
//! drops oldest, because a guest that never drains (no `evdev` loaded, say)
//! must not grow the emulator's memory with keystrokes nobody will read.

use std::collections::VecDeque;

use crate::devices::virtio_mmio::{Transport, TransportWrite};
use crate::devices::virtqueue::{
    self, ChainOutcome, Descriptor, QueueLayout, DESC_F_INDIRECT, DESC_F_WRITE,
};
use crate::ram::Ram;
use crate::snapshot::{Reader, Writer};

const DEVICE_ID_INPUT: u32 = 18;
const QUEUE_SIZE_MAX: u16 = 64;
/// Host → guest events.
const QUEUE_EVENT: usize = 0;
/// Guest → host status (LEDs); drained and ignored.
const QUEUE_STATUS: usize = 1;

/// `struct virtio_input_event` is 8 bytes: `le16 type, le16 code, le32 value`.
const EVENT_BYTES: u32 = 8;

/// Largest host event backlog. Two full seconds of frantic typing; beyond it
/// the oldest events are dropped, since a guest that is not reading is not
/// going to want them.
const MAX_PENDING_EVENTS: usize = 256;

// ---- configuration space (spec 5.8.4) ----

const CFG_UNSET: u8 = 0x00;
const CFG_ID_NAME: u8 = 0x01;
const CFG_ID_SERIAL: u8 = 0x02;
const CFG_ID_DEVIDS: u8 = 0x03;
const CFG_PROP_BITS: u8 = 0x10;
const CFG_EV_BITS: u8 = 0x11;
const CFG_ABS_INFO: u8 = 0x12;

/// Where device configuration space starts in the MMIO window.
const CONFIG_BASE: u64 = 0x100;
/// `select`, `subsel`, `size`, then five reserved bytes.
const CONFIG_UNION_OFFSET: usize = 8;
const CONFIG_UNION_BYTES: usize = 128;
const CONFIG_BYTES: usize = CONFIG_UNION_OFFSET + CONFIG_UNION_BYTES;

// ---- evdev constants (linux/input-event-codes.h) ----

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;

pub const SYN_REPORT: u16 = 0;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const REL_WHEEL: u16 = 0x08;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

/// Highest key code the keyboard advertises. Covers the whole US layout plus
/// the function, navigation and modifier keys a browser can report.
const KEY_BITS: usize = 256;

/// Absolute axis range reported to the guest, and the range
/// [`crate::machine::Machine::input_pointer_abs`] takes. A browser gives
/// fractions of a viewport, so the exact number only has to be big enough to
/// address every pixel of any plausible display.
pub const ABS_MAX: u32 = 32767;

/// Which of the two devices this is; the only thing that differs is what the
/// configuration space advertises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputKind {
    Keyboard,
    Tablet,
}

impl InputKind {
    /// The `name` the guest publishes as `/sys/class/input/*/device/name`.
    pub fn device_name(self) -> &'static str {
        match self {
            InputKind::Keyboard => "emulate keyboard",
            InputKind::Tablet => "emulate tablet",
        }
    }

    fn product_id(self) -> u16 {
        match self {
            InputKind::Keyboard => 1,
            InputKind::Tablet => 2,
        }
    }
}

#[derive(Clone, Copy)]
struct InputEvent {
    kind: u16,
    code: u16,
    value: u32,
}

impl InputEvent {
    fn to_bytes(self) -> [u8; EVENT_BYTES as usize] {
        let mut out = [0u8; EVENT_BYTES as usize];
        out[0..2].copy_from_slice(&self.kind.to_le_bytes());
        out[2..4].copy_from_slice(&self.code.to_le_bytes());
        out[4..8].copy_from_slice(&self.value.to_le_bytes());
        out
    }
}

/// One VirtIO-MMIO input device.
pub struct VirtioInput {
    kind: InputKind,
    transport: Transport,
    chain: Vec<Descriptor>,
    pending: VecDeque<InputEvent>,
    dropped: u64,
    select: u8,
    subsel: u8,
}

impl VirtioInput {
    pub fn new(kind: InputKind) -> Self {
        Self {
            kind,
            transport: Transport::new(2, QUEUE_SIZE_MAX),
            chain: Vec::new(),
            pending: VecDeque::new(),
            dropped: 0,
            select: CFG_UNSET,
            subsel: 0,
        }
    }

    pub fn kind(&self) -> InputKind {
        self.kind
    }

    pub fn irq_pending(&self) -> bool {
        self.transport.irq_pending()
    }

    /// Events queued for a guest that has not taken them yet.
    pub fn pending_events(&self) -> usize {
        self.pending.len()
    }

    /// Events dropped because the backlog was full.
    pub fn dropped_events(&self) -> u64 {
        self.dropped
    }

    /// Queue one event. Nothing is delivered until [`Self::flush`] runs, so a
    /// caller can build a whole report (position, buttons, `SYN_REPORT`) and
    /// hand it to the guest as one batch.
    pub fn queue_event(&mut self, kind: u16, code: u16, value: u32) {
        if self.pending.len() >= MAX_PENDING_EVENTS {
            self.pending.pop_front();
            self.dropped += 1;
        }
        self.pending.push_back(InputEvent { kind, code, value });
    }

    /// Close the current report and deliver everything queued into whatever
    /// buffers the guest has posted, raising the interrupt if any landed.
    pub fn flush(&mut self, ram: &mut Ram) {
        self.queue_event(EV_SYN, SYN_REPORT, 0);
        self.process_event_queue(ram);
    }

    pub fn read(&self, offset: u64, size: u8) -> Result<u64, ()> {
        if !matches!(size, 1 | 2 | 4 | 8) {
            return Err(());
        }
        // VIRTIO_F_VERSION_1 (bit 32) is the only offered feature.
        if let Some(value) = self.transport.read(offset, DEVICE_ID_INPUT, [0, 1]) {
            return Ok(value);
        }
        Ok(self.read_config(offset, size))
    }

    pub fn write(&mut self, offset: u64, val: u64, size: u8, ram: &mut Ram) -> Result<(), ()> {
        if offset >= CONFIG_BASE {
            // `select`/`subsel` are single-byte registers the driver pokes
            // before every configuration read.
            if !matches!(size, 1 | 2 | 4) {
                return Err(());
            }
            let index = (offset - CONFIG_BASE) as usize;
            for byte in 0..size as usize {
                let value = (val >> (byte * 8)) as u8;
                match index + byte {
                    0 => self.select = value,
                    1 => self.subsel = value,
                    // `size` and the union are read-only; the rest is reserved.
                    _ => {}
                }
            }
            return Ok(());
        }
        if size != 4 {
            return Err(());
        }
        match self.transport.write(offset, val as u32) {
            TransportWrite::Notify(QUEUE_EVENT_NOTIFY) => self.process_event_queue(ram),
            TransportWrite::Notify(QUEUE_STATUS_NOTIFY) => self.drain_status_queue(ram),
            TransportWrite::Reset => {
                self.pending.clear();
                self.select = CFG_UNSET;
                self.subsel = 0;
            }
            _ => {}
        }
        Ok(())
    }

    // ---- configuration space ----

    fn read_config(&self, offset: u64, size: u8) -> u64 {
        let Some(index) = offset.checked_sub(CONFIG_BASE) else {
            return 0;
        };
        let (union, len) = self.config_union();
        let mut image = [0u8; CONFIG_BYTES];
        image[0] = self.select;
        image[1] = self.subsel;
        image[2] = len;
        image[CONFIG_UNION_OFFSET..].copy_from_slice(&union);

        let index = index as usize;
        let mut value = 0u64;
        for byte in 0..size as usize {
            let at = index + byte;
            let b = image.get(at).copied().unwrap_or(0);
            value |= u64::from(b) << (byte * 8);
        }
        value
    }

    /// The union body for the current `select`/`subsel`, and its `size`.
    fn config_union(&self) -> ([u8; CONFIG_UNION_BYTES], u8) {
        let mut buf = [0u8; CONFIG_UNION_BYTES];
        let len = match self.select {
            CFG_ID_NAME => {
                let name = self.kind.device_name().as_bytes();
                buf[..name.len()].copy_from_slice(name);
                name.len()
            }
            // No serial: a device with one identical instance per machine has
            // nothing to distinguish.
            CFG_ID_SERIAL => 0,
            CFG_ID_DEVIDS => {
                // bustype BUS_VIRTUAL, the Red Hat VirtIO vendor, our product.
                buf[0..2].copy_from_slice(&0x06u16.to_le_bytes());
                buf[2..4].copy_from_slice(&0x1af4u16.to_le_bytes());
                buf[4..6].copy_from_slice(&self.kind.product_id().to_le_bytes());
                buf[6..8].copy_from_slice(&1u16.to_le_bytes());
                8
            }
            // No INPUT_PROP_* bits: the tablet is a plain absolute pointer.
            CFG_PROP_BITS => 0,
            CFG_EV_BITS => self.event_bits(&mut buf),
            CFG_ABS_INFO => self.abs_info(&mut buf),
            _ => 0,
        };
        (buf, len as u8)
    }

    /// Which codes of event type `subsel` this device can report. A zero
    /// length is how the driver learns a type is not supported at all.
    fn event_bits(&self, buf: &mut [u8; CONFIG_UNION_BYTES]) -> usize {
        match (self.kind, self.subsel as u16) {
            (_, EV_SYN) => {
                set_bit(buf, SYN_REPORT as usize);
                1
            }
            (InputKind::Keyboard, EV_KEY) => {
                // Every code below KEY_BITS: the browser's `KeyboardEvent.code`
                // table maps into exactly this range.
                for bit in 1..KEY_BITS {
                    set_bit(buf, bit);
                }
                KEY_BITS / 8
            }
            (InputKind::Tablet, EV_KEY) => {
                for button in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] {
                    set_bit(buf, button as usize);
                }
                bitmap_len(BTN_MIDDLE as usize)
            }
            (InputKind::Tablet, EV_ABS) => {
                set_bit(buf, ABS_X as usize);
                set_bit(buf, ABS_Y as usize);
                bitmap_len(ABS_Y as usize)
            }
            (InputKind::Tablet, EV_REL) => {
                set_bit(buf, REL_WHEEL as usize);
                bitmap_len(REL_WHEEL as usize)
            }
            _ => 0,
        }
    }

    /// `struct virtio_input_absinfo`: min, max, fuzz, flat, res.
    fn abs_info(&self, buf: &mut [u8; CONFIG_UNION_BYTES]) -> usize {
        if self.kind != InputKind::Tablet {
            return 0;
        }
        if self.subsel as u16 != ABS_X && self.subsel as u16 != ABS_Y {
            return 0;
        }
        buf[0..4].copy_from_slice(&0u32.to_le_bytes());
        buf[4..8].copy_from_slice(&ABS_MAX.to_le_bytes());
        // No fuzz, no flat, no resolution: the coordinates are exact.
        20
    }

    // ---- queues ----

    fn process_event_queue(&mut self, ram: &mut Ram) {
        if self.pending.is_empty() {
            return;
        }
        let completed = self
            .transport
            .queue(QUEUE_EVENT)
            .drain(ram, |ram, layout, head| {
                if self.pending.is_empty() {
                    // No more to say; leave the buffer available for next time.
                    return ChainOutcome::Retry;
                }
                let mut chain = std::mem::take(&mut self.chain);
                let outcome = self.fill_event_buffer(ram, layout, head, &mut chain);
                chain.clear();
                self.chain = chain;
                outcome
            });
        self.transport.complete_queue(completed);
    }

    fn fill_event_buffer(
        &mut self,
        ram: &mut Ram,
        layout: QueueLayout,
        head: u16,
        chain: &mut Vec<Descriptor>,
    ) -> ChainOutcome {
        if !virtqueue::collect_chain(ram, layout, head, chain) {
            return ChainOutcome::Skip;
        }
        let Some(first) = chain.first().copied() else {
            return ChainOutcome::Skip;
        };
        if first.flags & DESC_F_WRITE == 0
            || first.flags & DESC_F_INDIRECT != 0
            || first.len < EVENT_BYTES
        {
            return ChainOutcome::Skip;
        }
        let Some(offset) = virtqueue::ram_offset(ram, first.addr, u64::from(EVENT_BYTES)) else {
            return ChainOutcome::Skip;
        };
        let Some(event) = self.pending.pop_front() else {
            return ChainOutcome::Retry;
        };
        if ram.write_slice(offset, &event.to_bytes()).is_err() {
            // Put it back: the buffer was unusable, not the event.
            self.pending.push_front(event);
            return ChainOutcome::Skip;
        }
        ChainOutcome::Used(EVENT_BYTES)
    }

    /// Consume whatever the guest posts on the statusq (LED and repeat-rate
    /// updates) without acting on it: there is no keyboard to light up.
    fn drain_status_queue(&mut self, ram: &mut Ram) {
        let mut chain = std::mem::take(&mut self.chain);
        let completed = self
            .transport
            .queue(QUEUE_STATUS)
            .drain(ram, |ram, layout, head| {
                if virtqueue::collect_chain(ram, layout, head, &mut chain) {
                    ChainOutcome::Used(0)
                } else {
                    ChainOutcome::Skip
                }
            });
        chain.clear();
        self.chain = chain;
        self.transport.complete_queue(completed);
    }
}

/// `TransportWrite::Notify` carries the guest's queue index; naming the two
/// keeps the match arms in [`VirtioInput::write`] readable.
const QUEUE_EVENT_NOTIFY: u32 = QUEUE_EVENT as u32;
const QUEUE_STATUS_NOTIFY: u32 = QUEUE_STATUS as u32;

fn set_bit(buf: &mut [u8; CONFIG_UNION_BYTES], bit: usize) {
    if let Some(byte) = buf.get_mut(bit / 8) {
        *byte |= 1 << (bit % 8);
    }
}

/// Bytes needed to carry bits 0..=`highest`.
fn bitmap_len(highest: usize) -> usize {
    (highest / 8 + 1).min(CONFIG_UNION_BYTES)
}

const SNAPSHOT_VERSION: u16 = 1;

impl VirtioInput {
    pub(crate) fn snapshot(&self, out: &mut Writer) {
        out.section(SNAPSHOT_VERSION, |w| {
            w.u8(self.select);
            w.u8(self.subsel);
            w.u32(self.pending.len() as u32);
            for event in &self.pending {
                w.u16(event.kind);
                w.u16(event.code);
                w.u32(event.value);
            }
            w.u64(self.dropped);
        });
        self.transport.snapshot(out);
    }

    pub(crate) fn restore(&mut self, input: &mut Reader<'_>) -> Result<(), String> {
        input.section("virtio-input", SNAPSHOT_VERSION, |r| {
            self.select = r.u8()?;
            self.subsel = r.u8()?;
            let count = r.u32()? as usize;
            if count > MAX_PENDING_EVENTS {
                return Err(format!(
                    "{count} queued input events exceed the {MAX_PENDING_EVENTS} the backlog holds"
                ));
            }
            self.pending.clear();
            for _ in 0..count {
                let kind = r.u16()?;
                let code = r.u16()?;
                let value = r.u32()?;
                self.pending.push_back(InputEvent { kind, code, value });
            }
            self.dropped = r.u64()?;
            Ok(())
        })?;
        self.transport.restore(input)?;
        self.chain.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::DRAM_BASE;
    use crate::devices::virtio_mmio::MAGIC;

    const DESC: u64 = DRAM_BASE + 0x1000;
    const AVAIL: u64 = DRAM_BASE + 0x2000;
    const USED: u64 = DRAM_BASE + 0x3000;
    const DATA: u64 = DRAM_BASE + 0x4000;

    /// A guest driver in miniature: programs one queue's rings and posts
    /// `count` 8-byte writable buffers on it.
    struct Loopback {
        device: VirtioInput,
        ram: Ram,
    }

    impl Loopback {
        fn new(kind: InputKind) -> Self {
            let mut loopback = Loopback {
                device: VirtioInput::new(kind),
                ram: Ram::new(0x10000),
            };
            loopback.configure_queue(QUEUE_EVENT);
            loopback
        }

        fn configure_queue(&mut self, index: usize) {
            let device = &mut self.device;
            let ram = &mut self.ram;
            device.write(0x030, index as u64, 4, ram).unwrap();
            device.write(0x038, 8, 4, ram).unwrap();
            for (low, high, address) in [
                (0x080, 0x084, DESC),
                (0x090, 0x094, AVAIL),
                (0x0a0, 0x0a4, USED),
            ] {
                device.write(low, address as u32 as u64, 4, ram).unwrap();
                device.write(high, address >> 32, 4, ram).unwrap();
            }
            device.write(0x044, 1, 4, ram).unwrap();
        }

        /// Post `count` buffers, each 8 bytes, starting at DATA.
        fn post_buffers(&mut self, count: u16) {
            for i in 0..count {
                let at = DESC + u64::from(i) * 16 - DRAM_BASE;
                self.ram
                    .write(at, DATA + u64::from(i) * 8, 8)
                    .expect("descriptor address");
                self.ram
                    .write(at + 8, u64::from(EVENT_BYTES), 4)
                    .expect("descriptor length");
                self.ram
                    .write(at + 12, u64::from(DESC_F_WRITE), 2)
                    .expect("descriptor flags");
                self.ram
                    .write(AVAIL + 4 + u64::from(i) * 2 - DRAM_BASE, u64::from(i), 2)
                    .expect("available ring slot");
            }
            self.ram
                .write(AVAIL + 2 - DRAM_BASE, u64::from(count), 2)
                .expect("available index");
        }

        fn used_index(&self) -> u64 {
            self.ram.read(USED + 2 - DRAM_BASE, 2).unwrap()
        }

        /// The event written into the `n`th posted buffer.
        fn event(&self, n: u64) -> (u16, u16, u32) {
            let base = DATA + n * 8 - DRAM_BASE;
            (
                self.ram.read(base, 2).unwrap() as u16,
                self.ram.read(base + 2, 2).unwrap() as u16,
                self.ram.read(base + 4, 4).unwrap() as u32,
            )
        }

        fn select(&mut self, select: u8, subsel: u8) -> (u8, Vec<u8>) {
            self.device
                .write(CONFIG_BASE, u64::from(select), 1, &mut self.ram)
                .unwrap();
            self.device
                .write(CONFIG_BASE + 1, u64::from(subsel), 1, &mut self.ram)
                .unwrap();
            let size = self.device.read(CONFIG_BASE + 2, 1).unwrap() as u8;
            let body = (0..size as u64)
                .map(|i| {
                    self.device
                        .read(CONFIG_BASE + CONFIG_UNION_OFFSET as u64 + i, 1)
                        .unwrap() as u8
                })
                .collect();
            (size, body)
        }
    }

    #[test]
    fn identity_is_a_virtio_1_input_device() {
        let device = VirtioInput::new(InputKind::Keyboard);
        assert_eq!(device.read(0, 4).unwrap(), u64::from(MAGIC));
        assert_eq!(device.read(8, 4).unwrap(), u64::from(DEVICE_ID_INPUT));
        assert_eq!(device.read(0x044, 4).unwrap(), 0);
    }

    #[test]
    fn configuration_space_names_both_devices() {
        for (kind, expected) in [
            (InputKind::Keyboard, "emulate keyboard"),
            (InputKind::Tablet, "emulate tablet"),
        ] {
            let mut loopback = Loopback::new(kind);
            let (size, body) = loopback.select(CFG_ID_NAME, 0);
            assert_eq!(size as usize, expected.len());
            assert_eq!(String::from_utf8(body).unwrap(), expected);
        }
    }

    #[test]
    fn keyboard_advertises_key_events_and_no_absolute_axes() {
        let mut loopback = Loopback::new(InputKind::Keyboard);

        let (size, bits) = loopback.select(CFG_EV_BITS, EV_KEY as u8);
        assert_eq!(size as usize, KEY_BITS / 8);
        // KEY_A is 30; the whole low range is advertised.
        assert_ne!(bits[30 / 8] & (1 << (30 % 8)), 0);

        assert_eq!(loopback.select(CFG_EV_BITS, EV_ABS as u8).0, 0);
        assert_eq!(loopback.select(CFG_ABS_INFO, ABS_X as u8).0, 0);
    }

    #[test]
    fn tablet_advertises_absolute_axes_buttons_and_a_wheel() {
        let mut loopback = Loopback::new(InputKind::Tablet);

        let (_, abs) = loopback.select(CFG_EV_BITS, EV_ABS as u8);
        assert_eq!(abs[0] & 0b11, 0b11);

        let (size, info) = loopback.select(CFG_ABS_INFO, ABS_Y as u8);
        assert_eq!(size, 20);
        assert_eq!(u32::from_le_bytes(info[0..4].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(info[4..8].try_into().unwrap()), ABS_MAX);

        let (_, keys) = loopback.select(CFG_EV_BITS, EV_KEY as u8);
        assert_ne!(keys[BTN_LEFT as usize / 8] & 1, 0);

        let (_, rel) = loopback.select(CFG_EV_BITS, EV_REL as u8);
        assert_ne!(rel[REL_WHEEL as usize / 8] & (1 << (REL_WHEEL % 8)), 0);
    }

    #[test]
    fn an_unknown_select_reports_an_empty_body() {
        let mut loopback = Loopback::new(InputKind::Keyboard);
        assert_eq!(loopback.select(0x7f, 0).0, 0);
        assert_eq!(loopback.select(CFG_ID_SERIAL, 0).0, 0);
    }

    #[test]
    fn a_key_press_reaches_the_guest_as_key_then_syn() {
        let mut loopback = Loopback::new(InputKind::Keyboard);
        loopback.post_buffers(4);

        loopback.device.queue_event(EV_KEY, 30, 1);
        loopback.device.flush(&mut loopback.ram);

        assert_eq!(loopback.used_index(), 2);
        assert_eq!(loopback.event(0), (EV_KEY, 30, 1));
        assert_eq!(loopback.event(1), (EV_SYN, SYN_REPORT, 0));
        assert!(loopback.device.irq_pending());
        assert_eq!(loopback.device.pending_events(), 0);
    }

    #[test]
    fn events_wait_for_buffers_and_arrive_on_the_next_notify() {
        let mut loopback = Loopback::new(InputKind::Keyboard);

        loopback.device.queue_event(EV_KEY, 30, 1);
        loopback.device.flush(&mut loopback.ram);
        assert_eq!(loopback.used_index(), 0);
        assert_eq!(loopback.device.pending_events(), 2);

        loopback.post_buffers(4);
        let ram = &mut loopback.ram;
        loopback.device.write(0x050, 0, 4, ram).unwrap();

        assert_eq!(loopback.used_index(), 2);
        assert_eq!(loopback.event(0), (EV_KEY, 30, 1));
    }

    #[test]
    fn partial_delivery_leaves_the_rest_queued() {
        let mut loopback = Loopback::new(InputKind::Keyboard);
        loopback.post_buffers(1);

        loopback.device.queue_event(EV_KEY, 30, 1);
        loopback.device.flush(&mut loopback.ram);

        assert_eq!(loopback.used_index(), 1);
        assert_eq!(loopback.event(0), (EV_KEY, 30, 1));
        // The SYN_REPORT had nowhere to go and is still owed.
        assert_eq!(loopback.device.pending_events(), 1);
    }

    #[test]
    fn the_backlog_is_bounded_and_drops_oldest() {
        let mut device = VirtioInput::new(InputKind::Keyboard);
        for i in 0..(MAX_PENDING_EVENTS as u32 + 10) {
            device.queue_event(EV_KEY, 30, i);
        }

        assert_eq!(device.pending_events(), MAX_PENDING_EVENTS);
        assert_eq!(device.dropped_events(), 10);
        assert_eq!(device.pending.front().map(|e| e.value), Some(10));
    }

    #[test]
    fn a_read_only_buffer_is_skipped_without_losing_the_event() {
        let mut loopback = Loopback::new(InputKind::Keyboard);
        loopback.post_buffers(2);
        // Clear DESC_F_WRITE on the first descriptor: the guest posted a
        // buffer the device cannot fill.
        loopback.ram.write(DESC + 12 - DRAM_BASE, 0, 2).unwrap();

        loopback.device.queue_event(EV_KEY, 30, 1);
        loopback.device.flush(&mut loopback.ram);

        // One chain consumed without a used entry, the event delivered in the
        // second.
        assert_eq!(loopback.used_index(), 1);
        assert_eq!(loopback.event(1), (EV_KEY, 30, 1));
    }

    #[test]
    fn an_out_of_ram_buffer_does_not_consume_the_event() {
        let mut loopback = Loopback::new(InputKind::Keyboard);
        loopback.post_buffers(1);
        let past_ram = DRAM_BASE + loopback.ram.size();
        loopback.ram.write(DESC - DRAM_BASE, past_ram, 8).unwrap();

        loopback.device.queue_event(EV_KEY, 30, 1);
        loopback.device.flush(&mut loopback.ram);

        assert_eq!(loopback.used_index(), 0);
        assert_eq!(loopback.device.pending_events(), 2);
    }

    #[test]
    fn the_status_queue_is_drained_and_ignored() {
        let mut loopback = Loopback::new(InputKind::Tablet);
        loopback.configure_queue(QUEUE_STATUS);
        loopback.post_buffers(2);

        let ram = &mut loopback.ram;
        loopback.device.write(0x050, 1, 4, ram).unwrap();

        assert_eq!(loopback.used_index(), 2);
        assert!(loopback.device.irq_pending());
    }

    #[test]
    fn a_transport_reset_clears_queued_events_and_the_config_selector() {
        let mut loopback = Loopback::new(InputKind::Keyboard);
        loopback.device.queue_event(EV_KEY, 30, 1);
        loopback.select(CFG_ID_NAME, 0);

        let ram = &mut loopback.ram;
        loopback.device.write(0x070, 0, 4, ram).unwrap();

        assert_eq!(loopback.device.pending_events(), 0);
        assert_eq!(loopback.device.read(CONFIG_BASE + 2, 1).unwrap(), 0);
    }

    #[test]
    fn snapshot_round_trips_queued_events_and_the_selector() {
        let mut device = VirtioInput::new(InputKind::Tablet);
        device.queue_event(EV_ABS, ABS_X, 100);
        device.queue_event(EV_ABS, ABS_Y, 200);
        let mut ram = Ram::new(0x1000);
        device
            .write(CONFIG_BASE, u64::from(CFG_EV_BITS), 1, &mut ram)
            .unwrap();
        device
            .write(CONFIG_BASE + 1, u64::from(EV_ABS), 1, &mut ram)
            .unwrap();

        let mut out = Writer::new();
        device.snapshot(&mut out);
        let bytes = out.into_bytes();

        let mut restored = VirtioInput::new(InputKind::Tablet);
        let mut reader = Reader::new(&bytes);
        restored.restore(&mut reader).expect("restore");

        assert_eq!(restored.pending_events(), 2);
        assert_eq!(restored.select, CFG_EV_BITS);
        assert_eq!(restored.subsel as u16, EV_ABS);
    }
}
