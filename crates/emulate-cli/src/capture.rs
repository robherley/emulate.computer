//! `emulate-computer snapshot` — boot a guest headlessly to its shell prompt,
//! quiesce it, and write an `EMUSNAP1` container the hosts can resume from.
//!
//! The capture conditions matter as much as the container format; they are
//! enforced here rather than left to the caller (see `docs/disk.md`):
//!
//! * **No network backend.** A snapshot must never bake in a DHCP lease or a
//!   relay flow, so the machine is built with `--net none` and the DTB keeps
//!   its `emulate.net=none` placeholder. A restored guest that wants
//!   networking can request a lease with `emuctl net connect`.
//! * **The seed disk is not modified.** The boot runs against a sparse scratch
//!   copy, so `guest/out/alpine-rootfs.ext4` — and the browser seed gzipped
//!   from it — stay byte-reproducible. The blocks the capture *did* change are
//!   diffed out of the scratch copy and carried in the container as a disk
//!   overlay, which a restore writes back before the guest resumes. That is
//!   what keeps the restored guest's filesystem caches consistent with the
//!   disk underneath them.
//! * **Caches dropped.** `sync` plus `drop_caches` cuts the non-zero page set
//!   roughly in half and, more importantly, means the restored guest re-reads
//!   file data from the disk instead of trusting a cache captured against a
//!   different one.

use emulate_core::snapshot::{DiskOverlay, SnapshotIdentity};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::headless::Headless;
use crate::net::NetMode;
use crate::run::{build_machine, BootImages, BootPaths};

/// Root's bash prompt (`root@emulate.computer:~# `).
const PROMPT: &[u8] = b"root@emulate.computer:";

/// How long the guest gets to reach its prompt. Generous: an unoptimized
/// build of the core is orders of magnitude slower than a release one.
const BOOT_BUDGET: Duration = Duration::from_secs(900);
/// How long each post-boot shell command gets.
const COMMAND_BUDGET: Duration = Duration::from_secs(300);
/// Wall clock given to the guest after the last command, so `sync` finishes
/// and the shell settles back to an idle WFI before the state is frozen.
const SETTLE: Duration = Duration::from_millis(1_500);

/// Everything `snapshot` needs. Mirrors the clap subcommand one-for-one.
pub struct CaptureArgs {
    pub ram_mb: u64,
    pub bios: PathBuf,
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    pub dtb: PathBuf,
    pub disk: PathBuf,
    /// How the *hosts* name this root disk. The browser compares it against
    /// its versioned browser seed identity before resuming, so a snapshot can never be
    /// restored on top of a differently built root filesystem.
    pub disk_id: String,
    pub out: PathBuf,
    pub verbose: bool,
    pub desktop: bool,
}

pub fn cmd_snapshot(args: &CaptureArgs) -> i32 {
    match capture(args) {
        Ok(summary) => {
            eprintln!("{summary}");
            0
        }
        Err(message) => {
            eprintln!("emulate-computer snapshot: {message}");
            4
        }
    }
}

/// Capture a snapshot into `args.out`, returning the human-readable summary
/// the subcommand prints. Public so the integration tests can produce a real
/// snapshot without shelling out.
pub fn capture(args: &CaptureArgs) -> Result<String, String> {
    let started = Instant::now();
    let original = std::fs::read(&args.disk)
        .map_err(|error| format!("cannot read {}: {error}", args.disk.display()))?;

    // The scratch copy lives beside the output so it lands in the same build
    // tree; it is removed whether the capture succeeds or fails.
    let scratch = args.out.with_extension("capture-disk");
    sparse_copy(&args.disk, &scratch)?;
    let result = capture_onto(args, &scratch, &original, started);
    let _ = std::fs::remove_file(&scratch);
    result
}

fn capture_onto(
    args: &CaptureArgs,
    scratch: &Path,
    original: &[u8],
    started: Instant,
) -> Result<String, String> {
    let paths = BootPaths {
        ram_mb: args.ram_mb,
        bios: Some(args.bios.clone()),
        kernel: Some(args.kernel.clone()),
        initrd: args.initrd.clone(),
        dtb: Some(args.dtb.clone()),
        disk: Some(scratch.to_path_buf()),
        fw_dynamic: Some(true),
        // Never a backend: see the module docs.
        net: NetMode::None,
        verbose: args.verbose,
        ..BootPaths::default()
    };
    let images = BootImages::load(&paths)?;
    let machine = build_machine(&images)?;
    let mut guest = Headless::new(machine, args.verbose)?;

    guest
        .run_until(PROMPT, BOOT_BUDGET)
        .map_err(|why| why.to_string())?;
    guest
        .run_until_control(b"0 ready\n", BOOT_BUDGET)
        .map_err(|why| why.to_string())?;
    if args.desktop {
        guest
            .run_until_control(b"0 desktop ready\n", BOOT_BUDGET)
            .map_err(|why| why.to_string())?;
    }
    let boot_seconds = started.elapsed().as_secs_f64();

    // Flush the page cache back to the disk, then drop everything clean. The
    // second `sync` covers the writeback `drop_caches` itself triggers.
    guest.shell("sync; echo 3 > /proc/sys/vm/drop_caches; sync");
    guest
        .run_until(PROMPT, COMMAND_BUDGET)
        .map_err(|why| why.to_string())?;
    guest.settle(SETTLE).map_err(|why| why.to_string())?;

    // Push the emulator's own write-behind out to the scratch file before it
    // is diffed.
    guest
        .machine_mut()
        .flush_disk()
        .map_err(|why| format!("cannot flush the capture disk: {why:?}"))?;

    let modified =
        std::fs::read(scratch).map_err(|error| format!("cannot read the capture disk: {error}"))?;
    let overlay = DiskOverlay::diff(original, &modified)?;

    let identity = SnapshotIdentity {
        image_hash: images.image_hash,
        disk_id: args.disk_id.clone(),
    };
    let bytes = guest.machine().snapshot(&identity, &overlay);
    let ram_pages = bytes_to_pages(guest.machine().ram_bytes());

    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(&args.out, &bytes)
        .map_err(|error| format!("cannot write {}: {error}", args.out.display()))?;

    let header = emulate_core::snapshot::read_header(&bytes)?;
    Ok(format!(
        "wrote {} ({:.1} MiB raw)\n  \
         boot to capture readiness: {boot_seconds:.1}s\n  \
         RAM: {} of {ram_pages} pages carried ({:.1} MiB)\n  \
         disk overlay: {} blocks ({:.1} MiB)\n  \
         images: {}\n  \
         root disk id: {:?}",
        args.out.display(),
        bytes.len() as f64 / (1 << 20) as f64,
        header.ram_pages,
        (header.ram_pages * 4096) as f64 / (1 << 20) as f64,
        header.disk_blocks,
        (header.disk_blocks * 4096) as f64 / (1 << 20) as f64,
        emulate_core::snapshot::hex(&header.identity.image_hash),
        header.identity.disk_id,
    ))
}

fn bytes_to_pages(bytes: u64) -> u64 {
    bytes.div_ceil(4096)
}

/// Copy `source` to `destination`, seeking over all-zero chunks so a 512 MiB
/// sparse image does not occupy 512 MiB in the build tree.
fn sparse_copy(source: &Path, destination: &Path) -> Result<(), String> {
    let context = |what: &str, error: std::io::Error| format!("{what}: {error}");
    let mut input =
        std::fs::File::open(source).map_err(|e| context("cannot open the seed image", e))?;
    let mut output = std::fs::File::create(destination)
        .map_err(|e| context("cannot create the capture disk", e))?;
    let logical = input
        .metadata()
        .map_err(|e| context("cannot stat the seed image", e))?
        .len();
    let mut chunk = vec![0u8; 1 << 20];
    loop {
        let read = input
            .read(&mut chunk)
            .map_err(|e| context("cannot read the seed image", e))?;
        if read == 0 {
            break;
        }
        if chunk[..read].iter().any(|&b| b != 0) {
            output
                .write_all(&chunk[..read])
                .map_err(|e| context("cannot write the capture disk", e))?;
        } else {
            output
                .seek(SeekFrom::Current(read as i64))
                .map_err(|e| context("cannot seek the capture disk", e))?;
        }
    }
    output
        .set_len(logical)
        .map_err(|e| context("cannot size the capture disk", e))
}
