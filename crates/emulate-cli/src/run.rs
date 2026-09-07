//! Machine construction for a system boot: image paths in, a loaded
//! [`Machine`] out. The console that drives it lives in the binary
//! (`src/console.rs`).

use crate::elfload;
use crate::net::{self, NetMode};
use emulate_core::bus::DRAM_BASE;
use emulate_core::devices::virtio_blk::{BlockBackend, BlockError};
use emulate_core::hostnet::{NatBackend, SocketSubstrate};
use emulate_core::machine::Machine;
use emulate_core::snapshot::{self, RestoreCheck};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Instructions per run slice.
pub const SLICE: u64 = 100_000;
/// Bytes of host entropy handed to the VirtIO RNG per refill.
pub const ENTROPY_REFILL_BYTES: usize = 64 * 1024;
/// Flat bios load address / OpenSBI entry.
const BIOS_ADDR: u64 = DRAM_BASE; // 0x8000_0000
/// Flat kernel (Linux Image) load address.
const KERNEL_ADDR: u64 = 0x8020_0000;
/// Default initrd load address (matches `just guest-dtb` for 128MB RAM).
const INITRD_ADDR: u64 = 0x8460_0000;
/// Default DTB cap address (2MB aligned, for 128MB RAM this is the default).
const DTB_ADDR_CAP: u64 = 0x8780_0000;
const TWO_MB: u64 = 2 << 20;

/// Where the boot images live on disk, plus the knobs that change how they are
/// loaded. [`BootImages::load`] turns this into the bytes `build_machine` wants.
pub struct BootPaths {
    pub ram_mb: u64,
    pub bios: Option<PathBuf>,
    pub kernel: Option<PathBuf>,
    pub initrd: Option<PathBuf>,
    pub dtb: Option<PathBuf>,
    pub disk: Option<PathBuf>,
    pub disk_volatile: bool,
    /// `None` means "decide from the bios filename" (see [`BootImages::load`]).
    pub fw_dynamic: Option<bool>,
    pub net: NetMode,
    /// The relay `--net user` dials, e.g. `ws://127.0.0.1:7654`.
    pub relay_url: String,
    /// Resume from an `EMUSNAP1` container instead of booting the images.
    /// The images are still required: they identify the snapshot, and a
    /// guest-requested reset falls back to a normal boot from them.
    pub snapshot: Option<PathBuf>,
    pub verbose: bool,
}

impl Default for BootPaths {
    fn default() -> Self {
        Self {
            ram_mb: 128,
            bios: None,
            kernel: None,
            initrd: None,
            dtb: None,
            disk: None,
            disk_volatile: false,
            fw_dynamic: None,
            net: NetMode::None,
            relay_url: net::DEFAULT_RELAY_URL.to_string(),
            snapshot: None,
            verbose: false,
        }
    }
}

/// The VirtIO block image, and whether guest writes reach the host.
pub enum Disk {
    /// Loaded into memory up front; writes are discarded when the guest exits.
    Volatile(Vec<u8>),
    /// Backed by the host file; writes land in it.
    Persistent(PathBuf),
}

/// Boot images in memory, ready for [`build_machine`].
///
/// Separate from building so a guest-requested reset rebuilds the machine from
/// the same bytes without re-reading (and re-patching) them.
pub struct BootImages {
    pub bios: Option<Vec<u8>>,
    pub kernel: Option<Vec<u8>>,
    pub initrd: Option<Vec<u8>>,
    pub dtb: Option<Vec<u8>>,
    pub disk: Option<Disk>,
    pub ram_mb: u64,
    pub fw_dynamic: bool,
    pub net: NetMode,
    pub relay_url: String,
    /// SHA-256 over firmware, kernel, initramfs and DTB, computed *before*
    /// the `emulate.net=` patch below: a snapshot is always captured with the
    /// NIC unplugged, so the network flag must not change the identity a
    /// restore checks against.
    pub image_hash: [u8; 32],
    /// An `EMUSNAP1` container to resume from, already read.
    pub snapshot: Option<Vec<u8>>,
    pub verbose: bool,
}

impl BootImages {
    /// Read every image named by `paths`.
    ///
    /// Two decisions happen here rather than per boot: the DTB's
    /// `emulate.net=` placeholder is flipped when a NIC will be attached, and
    /// `fw_dynamic` defaults to on when the bios filename contains "dynamic".
    pub fn load(paths: &BootPaths) -> Result<Self, String> {
        let read = |p: &Option<PathBuf>| -> Result<Option<Vec<u8>>, String> {
            match p {
                Some(p) => std::fs::read(p)
                    .map(Some)
                    .map_err(|e| format!("cannot read {}: {e}", p.display())),
                None => Ok(None),
            }
        };
        let mut dtb = read(&paths.dtb)?;
        // Hashed here, ahead of the cmdline patch and ahead of the ELF/flat
        // decisions: identity is "which bytes were on disk", nothing else.
        let empty: &[u8] = &[];
        let image_hash = snapshot::hash_images(&[
            read(&paths.bios)?.as_deref().unwrap_or(empty),
            read(&paths.kernel)?.as_deref().unwrap_or(empty),
            read(&paths.initrd)?.as_deref().unwrap_or(empty),
            dtb.as_deref().unwrap_or(empty),
        ]);
        if paths.net == NetMode::User {
            if let Some(dtb) = dtb.as_mut() {
                patch_net_cmdline(dtb);
            }
        }
        let bios = read(&paths.bios)?;
        let fw_dynamic = bios.is_some()
            && paths.fw_dynamic.unwrap_or_else(|| {
                paths
                    .bios
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().contains("dynamic"))
                    .unwrap_or(false)
            });
        let disk = match (&paths.disk, paths.disk_volatile) {
            (Some(path), true) => {
                Some(Disk::Volatile(std::fs::read(path).map_err(|e| {
                    format!("cannot read {}: {e}", path.display())
                })?))
            }
            (Some(path), false) => Some(Disk::Persistent(path.clone())),
            (None, _) => None,
        };
        Ok(Self {
            bios,
            kernel: read(&paths.kernel)?,
            initrd: read(&paths.initrd)?,
            dtb,
            disk,
            ram_mb: paths.ram_mb,
            fw_dynamic,
            net: paths.net,
            relay_url: paths.relay_url.clone(),
            image_hash,
            snapshot: match &paths.snapshot {
                Some(path) => Some(read_snapshot(path)?),
                None => None,
            },
            verbose: paths.verbose,
        })
    }

    /// The relay this boot will dial, or `None` with the NIC link down.
    pub fn relay(&self) -> Option<&str> {
        match self.net {
            NetMode::None => None,
            NetMode::User => Some(&self.relay_url),
        }
    }

    /// Fail before the machine is built if the relay is not up, rather than
    /// booting a guest whose network silently never works.
    pub fn check_relay(&self) -> Result<(), String> {
        match self.relay() {
            Some(url) => net::check_relay(url),
            None => Ok(()),
        }
    }
}

/// Flip the prebuilt DTBs' `emulate.net=none` bootarg to `emulate.net=dhcp`.
/// Same length, so the devicetree needs no rewriting; guest init sees the flag
/// and runs udhcpc, so the shell comes up with the lease already taken.
fn patch_net_cmdline(dtb: &mut [u8]) {
    const OFF: &[u8] = b"emulate.net=none";
    const ON: &[u8] = b"emulate.net=dhcp";
    if let Some(pos) = dtb.windows(OFF.len()).position(|w| w == OFF) {
        dtb[pos..pos + ON.len()].copy_from_slice(ON);
    }
}

struct FileBackend {
    file: File,
    capacity_sectors: u64,
}

impl FileBackend {
    fn open(path: &PathBuf) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("cannot open disk {}: {e}", path.display()))?;
        let len = file
            .metadata()
            .map_err(|e| format!("cannot inspect disk {}: {e}", path.display()))?
            .len();
        if len == 0 || len % 512 != 0 {
            return Err(format!(
                "disk {} must be a non-empty multiple of 512 bytes (got {len})",
                path.display()
            ));
        }
        Ok(Self {
            file,
            capacity_sectors: len / 512,
        })
    }

    fn offset(sector: u64, len: usize, capacity_sectors: u64) -> Result<u64, BlockError> {
        let offset = sector.checked_mul(512).ok_or(BlockError::OutOfRange)?;
        let end = offset
            .checked_add(len as u64)
            .ok_or(BlockError::OutOfRange)?;
        (end <= capacity_sectors * 512)
            .then_some(offset)
            .ok_or(BlockError::OutOfRange)
    }
}

impl BlockBackend for FileBackend {
    fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    fn read_at(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let offset = Self::offset(sector, buf.len(), self.capacity_sectors)?;
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(buf))
            .map_err(|_| BlockError::Io)
    }

    fn write_at(&mut self, sector: u64, buf: &[u8]) -> Result<(), BlockError> {
        let offset = Self::offset(sector, buf.len(), self.capacity_sectors)?;
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.write_all(buf))
            .map_err(|_| BlockError::Io)
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.file.sync_data().map_err(|_| BlockError::Io)
    }
}

/// Number of bytes written for the `fw_dynamic_info` handoff struct
/// (`{ magic, version, next_addr, next_mode, options }`, five u64 words).
const FW_DYNAMIC_INFO_LEN: u64 = 40;
/// The `fw_dynamic_info` struct is written one page below the DTB.
const FW_DYNAMIC_INFO_OFFSET: u64 = 0x1000;

/// A physical-memory region that will be populated before boot.
#[derive(Debug, Clone)]
struct Region {
    start: u64,
    len: u64,
    name: String,
}

impl Region {
    fn new(start: u64, len: u64, name: impl Into<String>) -> Self {
        Region {
            start,
            len,
            name: name.into(),
        }
    }

    fn end(&self) -> u64 {
        self.start.saturating_add(self.len)
    }
}

/// Validate that every load region fits in `[ram_start, ram_end)` and that no
/// two regions overlap. Regions are checked as half-open `[start, start+len)`
/// intervals; zero-length regions are ignored (nothing is written for them).
fn validate_regions(regions: &[Region], ram_start: u64, ram_end: u64) -> Result<(), String> {
    // Every region must fit within RAM.
    for r in regions {
        if r.len == 0 {
            continue;
        }
        let end = r.start.checked_add(r.len).ok_or_else(|| {
            format!(
                "{} at {:#x} (len {:#x}) overflows the address space",
                r.name, r.start, r.len
            )
        })?;
        if r.start < ram_start || end > ram_end {
            return Err(format!(
                "{} [{:#x}..{:#x}) does not fit in RAM [{ram_start:#x}..{ram_end:#x})",
                r.name, r.start, end
            ));
        }
    }

    // Sort by start address and check adjacent pairs for overlap.
    let mut sorted: Vec<&Region> = regions.iter().filter(|r| r.len != 0).collect();
    sorted.sort_by_key(|r| r.start);
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a.end() > b.start {
            return Err(format!(
                "{} [{:#x}..{:#x}) overlaps {} [{:#x}..{:#x})",
                a.name,
                a.start,
                a.end(),
                b.name,
                b.start,
                b.end()
            ));
        }
    }
    Ok(())
}

/// Build a machine with everything loaded and boot registers set.
pub fn build_machine(images: &BootImages) -> Result<Machine, String> {
    if images.bios.is_none() && images.kernel.is_none() {
        return Err(String::from("nothing to run (need --bios and/or --kernel)"));
    }
    let ram_bytes = images
        .ram_mb
        .checked_mul(1 << 20)
        .ok_or_else(|| format!("RAM size {} MB overflows", images.ram_mb))?;
    let ram_end = DRAM_BASE
        .checked_add(ram_bytes)
        .ok_or_else(|| format!("RAM size {} MB overflows the address space", images.ram_mb))?;

    // DTB near the end of RAM, 2MB aligned, capped at 0x8780_0000.
    let dtb_addr = ((ram_end.saturating_sub(TWO_MB)) & !(TWO_MB - 1)).min(DTB_ADDR_CAP);

    // Parse kernel/bios ELFs up front so we know every segment (and its memsz)
    // before deciding where anything lands.
    let kernel_elf = match &images.kernel {
        Some(data) if elfload::is_elf(data) => Some(elfload::load_elf(data)?),
        _ => None,
    };
    let bios_elf = match &images.bios {
        Some(data) if elfload::is_elf(data) => Some(elfload::load_elf(data)?),
        _ => None,
    };
    let kernel_entry = kernel_elf.as_ref().map(|e| e.entry).unwrap_or(KERNEL_ADDR);
    let bios_entry = bios_elf.as_ref().map(|e| e.entry).unwrap_or(BIOS_ADDR);
    let initrd_addr = INITRD_ADDR;

    let fw_dynamic = images.fw_dynamic;
    let fw_info_addr = dtb_addr.saturating_sub(FW_DYNAMIC_INFO_OFFSET);

    // Build the full list of regions that will be written, then validate that
    // they all fit in RAM and none overlap — before touching memory.
    let mut regions: Vec<Region> = Vec::new();
    if let Some(elf) = &bios_elf {
        for (paddr, memsz, _) in elf.loadable() {
            regions.push(Region::new(
                paddr,
                memsz,
                format!("bios segment {paddr:#x}"),
            ));
        }
    } else if let Some(data) = &images.bios {
        regions.push(Region::new(BIOS_ADDR, data.len() as u64, "bios"));
    }
    if let Some(elf) = &kernel_elf {
        for (paddr, memsz, _) in elf.loadable() {
            regions.push(Region::new(
                paddr,
                memsz,
                format!("kernel segment {paddr:#x}"),
            ));
        }
    } else if let Some(data) = &images.kernel {
        regions.push(Region::new(KERNEL_ADDR, data.len() as u64, "kernel"));
    }
    if let Some(data) = &images.initrd {
        regions.push(Region::new(initrd_addr, data.len() as u64, "initrd"));
    }
    if let Some(data) = &images.dtb {
        regions.push(Region::new(dtb_addr, data.len() as u64, "dtb"));
    }
    if fw_dynamic {
        regions.push(Region::new(
            fw_info_addr,
            FW_DYNAMIC_INFO_LEN,
            "fw_dynamic_info",
        ));
    }
    validate_regions(&regions, DRAM_BASE, ram_end)?;

    let ram_bytes = usize::try_from(ram_bytes)
        .map_err(|_| format!("RAM size {} MB is too large for this host", images.ram_mb))?;
    let mut machine = Machine::new_system(ram_bytes);
    machine.set_unix_time_ns(host_unix_time_ns()?);

    // Kernel: ELF (segments at p_paddr, BSS zeroed) or flat Image at 0x8020_0000.
    if let Some(elf) = &kernel_elf {
        for (paddr, memsz, bytes) in elf.loadable() {
            elfload::load_segment(&mut machine, paddr, memsz, bytes)
                .map_err(|_| format!("kernel segment at {paddr:#x} does not fit in RAM"))?;
            vprint(
                images,
                &format!(
                    "[emulate] kernel segment {paddr:#010x} ({} bytes)",
                    bytes.len()
                ),
            );
        }
    } else if let Some(data) = &images.kernel {
        machine
            .load_blob(KERNEL_ADDR, data)
            .map_err(|_| "kernel image does not fit in RAM".to_string())?;
        vprint(
            images,
            &format!(
                "[emulate] kernel (flat) at {KERNEL_ADDR:#010x} ({} bytes)",
                data.len()
            ),
        );
    }

    // Bios: ELF (BSS zeroed) or flat at 0x8000_0000.
    if let Some(elf) = &bios_elf {
        for (paddr, memsz, bytes) in elf.loadable() {
            elfload::load_segment(&mut machine, paddr, memsz, bytes)
                .map_err(|_| format!("bios segment at {paddr:#x} does not fit in RAM"))?;
            vprint(
                images,
                &format!(
                    "[emulate] bios segment {paddr:#010x} ({} bytes)",
                    bytes.len()
                ),
            );
        }
    } else if let Some(data) = &images.bios {
        machine
            .load_blob(BIOS_ADDR, data)
            .map_err(|_| "bios image does not fit in RAM".to_string())?;
        vprint(
            images,
            &format!(
                "[emulate] bios (flat) at {BIOS_ADDR:#010x} ({} bytes)",
                data.len()
            ),
        );
    }

    // DTB.
    if let Some(data) = &images.dtb {
        machine
            .load_blob(dtb_addr, data)
            .map_err(|_| format!("dtb does not fit at {dtb_addr:#x}"))?;
        vprint(
            images,
            &format!("[emulate] dtb at {dtb_addr:#010x} ({} bytes)", data.len()),
        );
    }

    // Initrd, right below the DTB (page aligned). The DTB built by `just guest`
    // already carries linux,initrd-start/end for this address.
    if let Some(data) = &images.initrd {
        machine
            .load_blob(initrd_addr, data)
            .map_err(|_| format!("initrd does not fit at {initrd_addr:#x}"))?;
        vprint(
            images,
            &format!(
                "[emulate] initrd at {initrd_addr:#010x} ({} bytes)",
                data.len()
            ),
        );
    }

    attach_disk(&mut machine, images)?;

    // Host networking: user-mode NAT over the relay, or link down.
    if let Some(url) = images.relay() {
        let substrate = net::substrate(url);
        let description = substrate.describe();
        machine.set_net_backend(Box::new(NatBackend::new(substrate)));
        vprint(images, &format!("[emulate] net: {description}"));
    }

    // Boot registers.
    let a1_dtb = if images.dtb.is_some() { dtb_addr } else { 0 };
    let (entry, a2) = if images.bios.is_some() {
        let a2 = if fw_dynamic {
            let info_addr = fw_info_addr;
            // struct fw_dynamic_info: { magic, version, next_addr, next_mode, options }
            let next_addr = if images.kernel.is_some() {
                kernel_entry
            } else {
                KERNEL_ADDR
            };
            let words: [u64; 5] = [0x4942_534F, 2, next_addr, 1, 0];
            for (i, w) in words.iter().enumerate() {
                machine
                    .write_phys(info_addr + (i as u64) * 8, *w, 8)
                    .map_err(|_| "cannot write fw_dynamic_info".to_string())?;
            }
            vprint(
                images,
                &format!(
                    "[emulate] fw_dynamic_info at {info_addr:#010x} (next_addr {next_addr:#x})"
                ),
            );
            info_addr
        } else {
            0
        };
        (bios_entry, a2)
    } else {
        // Bare M-mode kernel boot (xv6 style): a1 = dtb (0 when absent).
        (kernel_entry, 0)
    };

    machine.set_boot(entry, 0, a1_dtb, a2);
    vprint(
        images,
        &format!("[emulate] boot: pc={entry:#x} a0=0 a1={a1_dtb:#x} a2={a2:#x}"),
    );
    Ok(machine)
}

/// Attach whichever block image `images` names, volatile or persistent.
fn attach_disk(machine: &mut Machine, images: &BootImages) -> Result<(), String> {
    match &images.disk {
        Some(Disk::Volatile(data)) => {
            machine.set_disk(data);
            vprint(
                images,
                &format!("[emulate] virtio block image ({} bytes)", data.len()),
            );
        }
        Some(Disk::Persistent(path)) => {
            let backend = FileBackend::open(path)?;
            let bytes = backend.capacity_sectors() * 512;
            machine.set_disk_backend(Box::new(backend));
            vprint(
                images,
                &format!("[emulate] persistent virtio block image ({bytes} bytes)"),
            );
        }
        None => {}
    }
    Ok(())
}

/// Read a snapshot container, rejecting anything that is not one before the
/// caller gets far enough to care about its contents.
fn read_snapshot(path: &PathBuf) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read snapshot {}: {error}", path.display()))?;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Err(format!(
            "{} is gzipped; the native runner reads the raw container \
             (the .gz is the browser's copy) — gunzip it first",
            path.display()
        ));
    }
    snapshot::read_header(&bytes)
        .map_err(|why| format!("{}: {why}", path.display()))
        .map(|_| bytes)
}

/// Host wall clock as Unix epoch nanoseconds.
fn host_unix_time_ns() -> Result<u64, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("host clock is before the Unix epoch: {error}"))?
        .as_nanos();
    u64::try_from(nanos).map_err(|_| "host Unix time exceeds the guest RTC range".to_owned())
}

/// Build a machine by resuming `images.snapshot` instead of booting.
///
/// Nothing is loaded into RAM: the container carries it. The block backend is
/// attached first, because the snapshot writes its disk overlay through it;
/// the wall clock is re-seeded from the host afterwards, since the captured
/// RTC value is as old as the snapshot file; the network backend is attached
/// last, and is the one piece of host state a snapshot never carries.
///
/// The disk must be in the state the snapshot was captured against — a
/// freshly seeded root image. See `docs/disk.md`; the native runner cannot
/// check that for you, so it only checks the boot images.
pub fn restore_machine(images: &BootImages) -> Result<Machine, String> {
    let snapshot = images
        .snapshot
        .as_deref()
        .ok_or_else(|| String::from("no snapshot to restore"))?;
    let header = snapshot::read_header(snapshot)?;
    let ram_bytes = usize::try_from(header.ram_bytes)
        .map_err(|_| format!("snapshot RAM size {} is too large", header.ram_bytes))?;
    let mut machine = Machine::new_system(ram_bytes);
    attach_disk(&mut machine, images)?;
    machine.restore(
        snapshot,
        &RestoreCheck {
            image_hash: Some(images.image_hash),
            // The native runner has no versioned root-disk name to compare
            // against, and says so rather than inventing one.
            disk_id: None,
        },
    )?;
    machine.set_unix_time_ns(host_unix_time_ns()?);
    if let Some(url) = images.relay() {
        let substrate = net::substrate(url);
        let description = substrate.describe();
        machine.set_net_backend(Box::new(NatBackend::new(substrate)));
        if !machine.control_input(b"1 network.connect\n") {
            return Err("restored guest control port is full".into());
        }
        vprint(images, &format!("[emulate] net: {description}"));
    }
    vprint(
        images,
        &format!(
            "[emulate] restored {} MiB of RAM from {} pages, root disk {:?}, {} overlay blocks",
            header.ram_bytes >> 20,
            header.ram_pages,
            header.identity.disk_id,
            header.disk_blocks
        ),
    );
    Ok(machine)
}

/// Verbose print that is safe in raw terminal mode (explicit CRLF).
fn vprint(images: &BootImages, msg: &str) {
    if images.verbose {
        eprint!("{msg}\r\n");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn net_cmdline_patch_flips_placeholder_in_place() {
        let mut dtb = b"bootargs\0console=ttyS0 emulate.net=none\0rest".to_vec();
        let before = dtb.len();
        super::patch_net_cmdline(&mut dtb);
        assert_eq!(dtb.len(), before);
        assert!(dtb.windows(16).any(|w| w == b"emulate.net=dhcp"));
        assert!(!dtb.windows(16).any(|w| w == b"emulate.net=none"));
    }

    #[test]
    fn net_cmdline_patch_is_a_no_op_without_the_placeholder() {
        let mut dtb = b"bootargs\0console=ttyS0\0".to_vec();
        let expected = dtb.clone();
        super::patch_net_cmdline(&mut dtb);
        assert_eq!(dtb, expected);
    }

    use super::*;

    // 128MB Linux boot defaults, mirrored here for the region tests.
    const RAM_128MB_END: u64 = DRAM_BASE + (128 << 20); // 0x8800_0000
    const DEFAULT_DTB: u64 = 0x8780_0000;
    const DEFAULT_FW_INFO: u64 = DEFAULT_DTB - FW_DYNAMIC_INFO_OFFSET; // 0x877f_f000

    #[test]
    fn normal_linux_boot_layout_validates_cleanly() {
        // bios (OpenSBI) at DRAM_BASE, flat kernel, initrd, dtb, fw_dynamic_info.
        let regions = vec![
            Region::new(BIOS_ADDR, 0x4_0000, "bios segment 0x80000000"),
            Region::new(KERNEL_ADDR, 0x40_0000, "kernel"),
            Region::new(INITRD_ADDR, 0x20_0000, "initrd"),
            Region::new(DEFAULT_DTB, 0x1000, "dtb"),
            Region::new(DEFAULT_FW_INFO, FW_DYNAMIC_INFO_LEN, "fw_dynamic_info"),
        ];
        validate_regions(&regions, DRAM_BASE, RAM_128MB_END).expect("default layout is valid");
    }

    #[test]
    fn initrd_ending_in_the_fw_dynamic_info_page_is_rejected() {
        // An initrd ending in [dtb-0x1000, dtb) clears an initrd-vs-dtb check
        // but collides with the fw_dynamic_info struct just below the dtb.
        let initrd_start = 0x8460_0000;
        let initrd_len = DEFAULT_FW_INFO + 8 - initrd_start; // ends inside the fw page
        let regions = vec![
            Region::new(initrd_start, initrd_len, "initrd"),
            Region::new(DEFAULT_DTB, 0x1000, "dtb"),
            Region::new(DEFAULT_FW_INFO, FW_DYNAMIC_INFO_LEN, "fw_dynamic_info"),
        ];
        let err = validate_regions(&regions, DRAM_BASE, RAM_128MB_END)
            .expect_err("initrd tail overlaps fw_dynamic_info");
        assert!(err.contains("initrd"), "err: {err}");
        assert!(err.contains("fw_dynamic_info"), "err: {err}");
    }

    #[test]
    fn kernel_and_initrd_overlap_is_rejected() {
        // A large flat kernel Image running into the initrd load address.
        let regions = vec![
            Region::new(KERNEL_ADDR, INITRD_ADDR - KERNEL_ADDR + 0x1000, "kernel"),
            Region::new(INITRD_ADDR, 0x1000, "initrd"),
        ];
        let err = validate_regions(&regions, DRAM_BASE, RAM_128MB_END)
            .expect_err("kernel runs into initrd");
        assert!(
            err.contains("kernel") && err.contains("initrd"),
            "err: {err}"
        );
    }

    #[test]
    fn adjacent_regions_do_not_overlap() {
        let regions = vec![
            Region::new(DRAM_BASE, 0x1000, "a"),
            Region::new(DRAM_BASE + 0x1000, 0x1000, "b"),
        ];
        validate_regions(&regions, DRAM_BASE, RAM_128MB_END).expect("adjacent is fine");
    }

    #[test]
    fn region_outside_ram_is_rejected() {
        // A dtb landing past a small RAM (the RAM_MB < 128 case).
        let small_ram_end = DRAM_BASE + (64 << 20); // 0x8400_0000
        let regions = vec![Region::new(DEFAULT_DTB, 0x1000, "dtb")];
        let err = validate_regions(&regions, DRAM_BASE, small_ram_end)
            .expect_err("dtb is above 64MB RAM");
        assert!(err.contains("does not fit"), "err: {err}");
    }

    #[test]
    fn region_below_dram_base_is_rejected() {
        let regions = vec![Region::new(DRAM_BASE - 0x1000, 0x1000, "low")];
        let err =
            validate_regions(&regions, DRAM_BASE, RAM_128MB_END).expect_err("below DRAM base");
        assert!(err.contains("does not fit"), "err: {err}");
    }

    #[test]
    fn zero_length_regions_are_ignored() {
        // An empty image shares a start address but writes nothing.
        let regions = vec![
            Region::new(KERNEL_ADDR, 0, "empty"),
            Region::new(KERNEL_ADDR, 0x1000, "kernel"),
        ];
        validate_regions(&regions, DRAM_BASE, RAM_128MB_END).expect("zero-len ignored");
    }
}
