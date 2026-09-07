//! Post-boot state snapshots: a versioned, dependency-free container holding
//! everything needed to resume a machine at an arbitrary instruction boundary.
//!
//! Restoring avoids repeating firmware, kernel, and userspace startup.
//! See `docs/disk.md` for capture and restore requirements.
//!
//! # Container
//!
//! All integers are little-endian. The file is a magic + header followed by a
//! sequence of length-prefixed sections:
//!
//! ```text
//! "EMUSNAP1"                       8 bytes
//! format_version                   u32
//! header section                   (see below)
//! cpu section
//! device sections (one per device)
//! ram section
//! disk-overlay section
//! ```
//!
//! Every section is `u16 version | u32 body_len | body`, so a reader can check
//! a device's own version independently and a body it does not understand is a
//! hard error rather than a silent misparse.
//!
//! # What is captured
//!
//! Architectural and device state only: GPRs, FPRs, `pc`, privilege, the whole
//! CSR file, the LR/SC reservation, WFI, plus CLINT / PLIC / UART / Goldfish
//! RTC / VirtIO block, net, entropy and the two input devices /
//! framebuffer blitter / test-finisher / ACT4 interrupt generator state, and the non-zero 4 KiB
//! pages of guest RAM. Drawing pixels ride along in RAM; the blitter section also
//! preserves the committed front buffer
//! (`docs/display.md`).
//!
//! Deliberately *not* captured, because they are caches that any correct
//! machine may drop at any instruction boundary: the Sv39 TLB, the decoded
//! block cache, and the one-entry fetch translation memo. [`Machine::restore`]
//! flushes all three.
//!
//! Also not captured: the host attachments. The block backend must be attached
//! *before* restoring (the snapshot carries a disk overlay it writes through
//! that backend); the network backend after, since a snapshot is always taken
//! with the NIC unplugged so no DHCP lease or relay flow is ever baked in.

use crate::machine::{Machine, TIMEBASE_FREQ};

/// Container magic. Bumping this is a format break, not a version bump.
pub const MAGIC: [u8; 8] = *b"EMUSNAP1";
/// Container format version. Any change to the section list bumps this.
pub const FORMAT_VERSION: u32 = 6;

/// Disk-overlay block granularity.
pub const DISK_BLOCK: usize = 4096;

// ---------------------------------------------------------------------------
// Byte writer / reader
// ---------------------------------------------------------------------------

/// Append-only little-endian byte writer. Deliberately not a serde
/// `Serializer`: `emulate-core` builds for `wasm32-unknown-unknown` and stays
/// free of dependencies.
pub struct Writer {
    buf: Vec<u8>,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    pub fn new() -> Self {
        Writer { buf: Vec::new() }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }

    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Length-prefixed bytes (`u32` count).
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.u32(bytes.len() as u32);
        self.raw(bytes);
    }

    /// Length-prefixed UTF-8 string.
    pub fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// Write one `u16 version | u32 len | body` section, with `body` produced
    /// into a nested writer so the length is exact.
    pub fn section(&mut self, version: u16, body: impl FnOnce(&mut Writer)) {
        let mut inner = Writer::new();
        body(&mut inner);
        let bytes = inner.into_bytes();
        self.u16(version);
        self.u32(bytes.len() as u32);
        self.raw(&bytes);
    }
}

/// Cursor over a snapshot body. Every accessor is checked: a truncated or
/// corrupt container produces an error, never a panic or a wrong value.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| String::from("snapshot: length overflow"))?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| format!("snapshot: truncated (wanted {n} bytes at {})", self.pos))?;
        self.pos = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool, String> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(format!("snapshot: {other} is not a boolean")),
        }
    }

    pub fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn raw(&mut self, n: usize) -> Result<&'a [u8], String> {
        self.take(n)
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], String> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    pub fn string(&mut self) -> Result<String, String> {
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| String::from("snapshot: invalid UTF-8"))
    }

    /// Read one section written by [`Writer::section`], check its version, and
    /// hand the body to `body` as its own reader. The body must be consumed
    /// exactly: leftover bytes mean the writer and reader disagree.
    pub fn section<T>(
        &mut self,
        name: &str,
        expected_version: u16,
        body: impl FnOnce(&mut Reader<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let version = self.u16()?;
        if version != expected_version {
            return Err(format!(
                "snapshot: {name} section is version {version}, this build reads {expected_version}"
            ));
        }
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        let mut inner = Reader::new(bytes);
        let value = body(&mut inner).map_err(|why| format!("snapshot: {name}: {why}"))?;
        if inner.remaining() != 0 {
            return Err(format!(
                "snapshot: {name} section has {} unread trailing bytes",
                inner.remaining()
            ));
        }
        Ok(value)
    }
}

// ---------------------------------------------------------------------------
// SHA-256 (identity hashing)
// ---------------------------------------------------------------------------

/// SHA-256 over `chunks`, each length-prefixed so concatenation is
/// unambiguous. Implemented here rather than pulled in: `emulate-core` has no
/// dependencies, and this runs a handful of times per build.
pub fn hash_images(chunks: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for chunk in chunks {
        hasher.update(&(chunk.len() as u64).to_le_bytes());
        hasher.update(chunk);
    }
    hasher.finish()
}

/// Lowercase hex of a 32-byte digest.
pub fn hex(digest: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Streaming SHA-256. Public so hosts can hash multi-hundred-megabyte disk
/// images without holding them in memory.
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let take = (64 - self.filled).min(data.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled == 64 {
                let block = self.block;
                self.compress(&block);
                self.filled = 0;
            }
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        // `update` counted the padding into `length`; the bit count captured
        // above is the real message length.
        self.block[56..64].copy_from_slice(&bits.to_be_bytes());
        let block = self.block;
        self.compress(&block);
        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

// ---------------------------------------------------------------------------
// Identity and header
// ---------------------------------------------------------------------------

/// What a snapshot was taken from. Both halves are recorded in the header and
/// re-checked at restore, so a rebuilt kernel or a different root filesystem
/// can never be resumed into somebody else's memory image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotIdentity {
    /// SHA-256 over the boot images the capture booted: firmware, kernel,
    /// initramfs and DTB, in that order, each length-prefixed. The DTB is
    /// hashed *unpatched* (`emulate.net=none`): a snapshot is always captured
    /// with the NIC unplugged, so the network flag never varies here.
    pub image_hash: [u8; 32],
    /// The root disk the capture ran on, named the way its host names it —
    /// the browser's `sha256:<hash>` identity of the uncompressed seed disk.
    pub disk_id: String,
}

/// Which header fields a restore insists on. `None` means "do not check":
/// the native runner has no versioned disk name to compare against, and says
/// so explicitly rather than inventing one.
#[derive(Clone, Debug, Default)]
pub struct RestoreCheck<'a> {
    pub image_hash: Option<[u8; 32]>,
    pub disk_id: Option<&'a str>,
}

/// The header, as read back from a container. Hosts inspect this before
/// deciding whether a snapshot is worth restoring.
#[derive(Clone, Debug)]
pub struct SnapshotHeader {
    pub format_version: u32,
    pub identity: SnapshotIdentity,
    /// Guest RAM size the snapshot was taken from; a restore target must match.
    pub ram_bytes: u64,
    /// `TIMEBASE_FREQ` at capture, so a rebuilt core with a different timebase
    /// is rejected instead of running the guest's timers at the wrong rate.
    pub timebase_hz: u64,
    /// Guest `mtime` at capture.
    pub mtime: u64,
    /// Guest wall clock (Goldfish RTC) at capture. Overwritten at restore with
    /// the host's own time; recorded for diagnostics.
    pub captured_unix_ns: u64,
    /// Non-zero RAM pages the container carries.
    pub ram_pages: u64,
    /// Disk blocks the container carries (see [`Machine::restore`]).
    pub disk_blocks: u64,
}

const HEADER_VERSION: u16 = 1;

impl SnapshotHeader {
    fn write(&self, out: &mut Writer) {
        out.section(HEADER_VERSION, |w| {
            w.raw(&self.identity.image_hash);
            w.string(&self.identity.disk_id);
            w.u64(self.ram_bytes);
            w.u64(self.timebase_hz);
            w.u64(self.mtime);
            w.u64(self.captured_unix_ns);
            w.u64(self.ram_pages);
            w.u64(self.disk_blocks);
        });
    }

    fn read(input: &mut Reader<'_>) -> Result<Self, String> {
        input.section("header", HEADER_VERSION, |r| {
            let mut image_hash = [0u8; 32];
            image_hash.copy_from_slice(r.raw(32)?);
            let disk_id = r.string()?;
            Ok(SnapshotHeader {
                format_version: FORMAT_VERSION,
                identity: SnapshotIdentity {
                    image_hash,
                    disk_id,
                },
                ram_bytes: r.u64()?,
                timebase_hz: r.u64()?,
                mtime: r.u64()?,
                captured_unix_ns: r.u64()?,
                ram_pages: r.u64()?,
                disk_blocks: r.u64()?,
            })
        })
    }
}

/// Read just the magic, format version and header of a container, without
/// touching the (much larger) RAM section. Hosts use this to decide whether a
/// snapshot applies before paying to decompress or restore the rest.
pub fn read_header(data: &[u8]) -> Result<SnapshotHeader, String> {
    let mut input = Reader::new(data);
    let magic = input.raw(8)?;
    if magic != MAGIC {
        return Err(String::from("snapshot: not an EMUSNAP container"));
    }
    let format_version = input.u32()?;
    if format_version != FORMAT_VERSION {
        return Err(format!(
            "snapshot: container is format version {format_version}, \
             this build reads {FORMAT_VERSION}"
        ));
    }
    SnapshotHeader::read(&mut input)
}

// ---------------------------------------------------------------------------
// Machine capture / restore
// ---------------------------------------------------------------------------

/// A snapshot's view of the root disk: absolute contents for every 4 KiB block
/// that differs from the pristine seed image the snapshot is meant to be
/// restored onto.
///
/// Capture runs against a scratch copy of the seed, so the shipped seed stays
/// byte-reproducible; the overlay is what makes the restored disk agree with
/// the RAM image's filesystem caches. Applying it is idempotent — the blocks
/// are absolute, not a diff — so a repeat restore onto an already-overlaid
/// disk is a no-op that leaves the same bytes.
#[derive(Clone, Debug, Default)]
pub struct DiskOverlay {
    /// `(block index, 4 KiB contents)`, ascending.
    pub blocks: Vec<(u64, Vec<u8>)>,
}

impl DiskOverlay {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Every 4 KiB block where `modified` differs from `original`. Both must
    /// be the same length.
    pub fn diff(original: &[u8], modified: &[u8]) -> Result<Self, String> {
        if original.len() != modified.len() {
            return Err(format!(
                "disk overlay: image lengths differ ({} vs {})",
                original.len(),
                modified.len()
            ));
        }
        let mut blocks = Vec::new();
        for (index, (before, after)) in original
            .chunks(DISK_BLOCK)
            .zip(modified.chunks(DISK_BLOCK))
            .enumerate()
        {
            if before != after {
                blocks.push((index as u64, after.to_vec()));
            }
        }
        Ok(DiskOverlay { blocks })
    }

    fn write(&self, out: &mut Writer) {
        out.section(1, |w| {
            w.u64(self.blocks.len() as u64);
            for (index, bytes) in &self.blocks {
                w.u64(*index);
                w.raw(bytes);
            }
        });
    }

    fn read(input: &mut Reader<'_>) -> Result<Self, String> {
        input.section("disk-overlay", 1, |r| {
            let count = r.u64()? as usize;
            let mut blocks = Vec::with_capacity(count.min(1 << 16));
            for _ in 0..count {
                let index = r.u64()?;
                blocks.push((index, r.raw(DISK_BLOCK)?.to_vec()));
            }
            Ok(DiskOverlay { blocks })
        })
    }
}

impl Machine {
    /// Serialize the machine into an `EMUSNAP1` container.
    ///
    /// `overlay` carries the root-disk blocks the capture changed; pass
    /// [`DiskOverlay::default`] for a diskless machine.
    ///
    /// The caller is responsible for the capture *conditions*: no network
    /// backend attached (so no lease or relay flow is baked in) and a guest
    /// that has been quiesced (`sync`, `drop_caches`) so the page set is small
    /// and the filesystem's on-disk state is current. See `docs/disk.md`.
    pub fn snapshot(&self, identity: &SnapshotIdentity, overlay: &DiskOverlay) -> Vec<u8> {
        let mut out = Writer::new();
        out.raw(&MAGIC);
        out.u32(FORMAT_VERSION);

        let ram_pages = self.bus.ram.nonzero_page_count();
        SnapshotHeader {
            format_version: FORMAT_VERSION,
            identity: identity.clone(),
            ram_bytes: self.bus.ram.size(),
            timebase_hz: TIMEBASE_FREQ,
            mtime: self.bus.clint.mtime,
            captured_unix_ns: self.bus.rtc.time_ns(),
            ram_pages,
            disk_blocks: overlay.blocks.len() as u64,
        }
        .write(&mut out);

        self.snapshot_cpu(&mut out);
        self.snapshot_devices(&mut out);
        self.bus.ram.snapshot(&mut out);
        overlay.write(&mut out);
        out.into_bytes()
    }

    /// Restore a machine from an `EMUSNAP1` container.
    ///
    /// Order of operations for the caller:
    ///
    /// 1. build the machine with [`Machine::new_system`] and the *same* RAM
    ///    size the snapshot was taken at (no blobs need loading — RAM comes
    ///    from the container),
    /// 2. attach the block backend, because the disk overlay is written
    ///    through it here,
    /// 3. call `restore`,
    /// 4. seed wall-clock time with [`Machine::set_unix_time_ns`] — the
    ///    captured RTC value is stale by however long the snapshot sat on a
    ///    CDN,
    /// 5. attach the network backend, if any.
    ///
    /// On error, machine state and disk blocks may already have changed. Discard
    /// the machine and reseed its disk before attempting a cold boot.
    pub fn restore(&mut self, data: &[u8], check: &RestoreCheck<'_>) -> Result<(), String> {
        let mut input = Reader::new(data);
        let magic = input.raw(8)?;
        if magic != MAGIC {
            return Err(String::from("snapshot: not an EMUSNAP container"));
        }
        let format_version = input.u32()?;
        if format_version != FORMAT_VERSION {
            return Err(format!(
                "snapshot: container is format version {format_version}, \
                 this build reads {FORMAT_VERSION}"
            ));
        }
        let header = SnapshotHeader::read(&mut input)?;

        if let Some(expected) = check.image_hash {
            if expected != header.identity.image_hash {
                return Err(format!(
                    "snapshot: taken from different boot images \
                     (snapshot {}, this host {})",
                    hex(&header.identity.image_hash),
                    hex(&expected)
                ));
            }
        }
        if let Some(expected) = check.disk_id {
            if expected != header.identity.disk_id {
                return Err(format!(
                    "snapshot: taken on root disk {:?}, this host has {:?}",
                    header.identity.disk_id, expected
                ));
            }
        }
        if header.ram_bytes != self.bus.ram.size() {
            return Err(format!(
                "snapshot: taken with {} bytes of RAM, this machine has {}",
                header.ram_bytes,
                self.bus.ram.size()
            ));
        }
        if header.timebase_hz != TIMEBASE_FREQ {
            return Err(format!(
                "snapshot: taken at a {} Hz timebase, this build runs at {TIMEBASE_FREQ} Hz",
                header.timebase_hz
            ));
        }

        self.restore_cpu(&mut input)?;
        self.restore_devices(&mut input)?;
        self.bus.ram.restore(&mut input)?;
        let overlay = DiskOverlay::read(&mut input)?;

        // Caches, not state: a machine may drop either at any instruction
        // boundary, so they are rebuilt from the restored RAM on demand.
        self.cpu.invalidate_caches();

        // The host's canvas is showing the picture from before the restore,
        // which has nothing to do with the RAM just loaded underneath it.
        self.framebuffer_mark_all_dirty();

        // The host's own mtime baseline restarts here; `advance_mtime_from_host`
        // takes deltas against the caller's zero, and the caller's clock is not
        // the captured guest's.
        self.reset_host_mtime_baseline();

        self.apply_disk_overlay(&overlay)?;
        Ok(())
    }

    /// Write an overlay's blocks through the attached block backend.
    fn apply_disk_overlay(&mut self, overlay: &DiskOverlay) -> Result<(), String> {
        if overlay.blocks.is_empty() {
            return Ok(());
        }
        for (index, bytes) in &overlay.blocks {
            let sector = index
                .checked_mul((DISK_BLOCK / 512) as u64)
                .ok_or_else(|| String::from("snapshot: disk overlay block index overflows"))?;
            self.bus
                .virtio_blk
                .host_write(sector, bytes)
                .map_err(|why| format!("snapshot: writing the disk overlay failed: {why:?}"))?;
        }
        self.bus
            .virtio_blk
            .flush()
            .map_err(|why| format!("snapshot: flushing the disk overlay failed: {why:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_test_vectors() {
        let mut empty = Sha256::new();
        empty.update(b"");
        assert_eq!(
            hex(&empty.finish()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let mut abc = Sha256::new();
        abc.update(b"abc");
        assert_eq!(
            hex(&abc.finish()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Two blocks plus padding, exercising the multi-compress path.
        let mut long = Sha256::new();
        long.update(&b"a".repeat(1_000_000));
        assert_eq!(
            hex(&long.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn streamed_and_single_shot_hashes_agree() {
        let data = (0u8..=255).cycle().take(4097).collect::<Vec<u8>>();
        let mut streamed = Sha256::new();
        for chunk in data.chunks(7) {
            streamed.update(chunk);
        }
        let mut one_shot = Sha256::new();
        one_shot.update(&data);
        assert_eq!(streamed.finish(), one_shot.finish());
    }

    #[test]
    fn image_hash_is_unambiguous_across_chunk_boundaries() {
        // Without the length prefix these two would hash identically.
        assert_ne!(
            hash_images(&[b"ab", b"c"]),
            hash_images(&[b"a", b"bc"]),
            "chunk boundaries must be part of the identity"
        );
    }

    #[test]
    fn reader_rejects_a_truncated_section() {
        let mut writer = Writer::new();
        writer.section(1, |w| w.u64(7));
        let mut bytes = writer.into_bytes();
        bytes.truncate(bytes.len() - 1);
        let error = Reader::new(&bytes)
            .section("thing", 1, |r| r.u64())
            .expect_err("truncated section");
        assert!(error.contains("truncated"), "{error}");
    }

    #[test]
    fn reader_rejects_a_section_version_it_does_not_know() {
        let mut writer = Writer::new();
        writer.section(2, |w| w.u64(7));
        let bytes = writer.into_bytes();
        let error = Reader::new(&bytes)
            .section("thing", 1, |r| r.u64())
            .expect_err("wrong version");
        assert!(error.contains("version 2"), "{error}");
    }

    #[test]
    fn reader_rejects_a_section_body_it_did_not_fully_consume() {
        let mut writer = Writer::new();
        writer.section(1, |w| {
            w.u64(7);
            w.u64(9);
        });
        let bytes = writer.into_bytes();
        let error = Reader::new(&bytes)
            .section("thing", 1, |r| r.u64())
            .expect_err("trailing bytes");
        assert!(error.contains("trailing"), "{error}");
    }

    #[test]
    fn disk_overlay_diff_records_only_changed_blocks() {
        let original = vec![0u8; DISK_BLOCK * 3];
        let mut modified = original.clone();
        modified[DISK_BLOCK + 10] = 0xab;
        let overlay = DiskOverlay::diff(&original, &modified).expect("diff");
        assert_eq!(overlay.len(), 1);
        assert_eq!(overlay.blocks[0].0, 1);
        assert_eq!(overlay.blocks[0].1[10], 0xab);
    }

    #[test]
    fn disk_overlay_diff_rejects_mismatched_images() {
        let error = DiskOverlay::diff(&[0u8; 8], &[0u8; 16]).expect_err("length mismatch");
        assert!(error.contains("lengths differ"), "{error}");
    }
}
