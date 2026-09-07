//! `emulate-computer` — native CLI for the RISC-V RV64GC emulator: `boot` a
//! system, `test` a riscv-tests ELF, or `snapshot` one.

mod console;
mod term;

use emulate_cli::{capture, net, run, test};

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "emulate-computer", version, about = "RISC-V RV64GC emulator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a riscv-tests ELF (tohost/fromhost HTIF protocol).
    Test {
        /// Path to the test ELF (e.g. vendor/riscv-tests/isa/rv64ui-p-add).
        elf: PathBuf,
        /// Wall-clock timeout in minutes.
        #[arg(long, default_value_t = 1)]
        timeout_mins: u64,
        /// RAM size in MB.
        #[arg(long, default_value_t = 64)]
        ram: u64,
        /// Print progress and detailed decode-cache telemetry while running.
        #[arg(short, long)]
        verbose: bool,
        /// Expose storage-only PMP CSRs for the legacy riscv-tests suite.
        #[arg(long, hide = true)]
        legacy_pmp_stubs: bool,
    },
    /// Boot a system (OpenSBI + kernel, or a bare M-mode kernel) with an
    /// interactive console. Ctrl-A x exits.
    // `run` is a hidden alias, so it stays out of `--help`.
    #[command(alias = "run")]
    Boot {
        /// RAM size in MB.
        #[arg(long, default_value_t = 128)]
        ram: u64,
        /// Firmware/BIOS image (flat binary or ELF). Loaded at 0x8000_0000
        /// if flat; ELF segments go to their p_paddr.
        #[arg(long)]
        bios: Option<PathBuf>,
        /// Kernel image (flat Image or ELF). Flat images load at 0x8020_0000.
        #[arg(long)]
        kernel: Option<PathBuf>,
        /// Initrd image, loaded below the DTB (default 0x8460_0000).
        #[arg(long)]
        initrd: Option<PathBuf>,
        /// Device tree blob, loaded near the end of RAM (default
        /// 0x8780_0000 for 128MB RAM).
        #[arg(long)]
        dtb: Option<PathBuf>,
        /// Persistent VirtIO block image (for example, an ext4 rootfs).
        #[arg(long)]
        disk: Option<PathBuf>,
        /// Load --disk into memory and discard guest writes on exit.
        #[arg(long, requires = "disk")]
        disk_volatile: bool,
        /// Pass an OpenSBI fw_dynamic info struct in a2. Defaults to true
        /// when the --bios filename contains "dynamic".
        #[arg(long)]
        fw_dynamic: Option<bool>,
        /// Guest networking. "user" attaches a slirp-style user-mode NAT
        /// (DHCP hands the guest 10.0.2.15; the gateway 10.0.2.2 is the host
        /// itself, so http://10.0.2.2:PORT reaches a local server) and dials
        /// the relay named by --relay. Defaults to "none".
        #[arg(long, value_enum, default_value_t = net::NetMode::None)]
        net: net::NetMode,
        /// The relay `--net user` dials (`relay/server.ts`, or any
        /// wire-compatible server). Overrides $EMULATE_RELAY_URL.
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
        /// Resume from a post-boot state snapshot instead of booting, which
        /// reaches the shell in about a second. The --bios/--kernel/--initrd/
        /// --dtb images must be the ones the snapshot was taken from, and
        /// --disk must be a root image still in its seeded state; see
        /// docs/disk.md.
        #[arg(long, value_name = "FILE")]
        snapshot: Option<PathBuf>,
        /// Print the load map, boot registers, and decode-cache telemetry.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Capture a post-boot state snapshot: boot headlessly to the shell
    /// prompt, quiesce the guest, and write an EMUSNAP1 container that
    /// `boot --snapshot` and the browser resume from (docs/disk.md).
    Snapshot {
        /// RAM size in MB. Must match what the hosts boot with.
        #[arg(long, default_value_t = 128)]
        ram: u64,
        #[arg(long)]
        bios: PathBuf,
        #[arg(long)]
        kernel: PathBuf,
        #[arg(long)]
        initrd: Option<PathBuf>,
        #[arg(long)]
        dtb: PathBuf,
        /// The root filesystem image. It is *not* modified: the capture runs
        /// on a sparse scratch copy and the blocks it changed are carried in
        /// the snapshot as a disk overlay.
        #[arg(long)]
        disk: PathBuf,
        /// Root disk identity (`sha256:<hash>` of the seed ext4 image).
        /// Browser restores require the matching published seed hash.
        #[arg(long, value_name = "ID")]
        disk_id: String,
        /// Wait for the graphical desktop before capturing. Requires the desktop DTB.
        #[arg(long)]
        desktop: bool,
        /// Where to write the container.
        #[arg(short, long, value_name = "FILE")]
        out: PathBuf,
        /// Echo the guest console while it boots.
        #[arg(short, long)]
        verbose: bool,
    },
}

/// `--relay`, else $EMULATE_RELAY_URL, else the Bun relay's local listener.
fn relay_url(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var(net::RELAY_URL_ENV).ok())
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| net::DEFAULT_RELAY_URL.to_string())
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Test {
            elf,
            timeout_mins,
            ram,
            verbose,
            legacy_pmp_stubs,
        } => test::cmd_test(&elf, timeout_mins, ram, verbose, legacy_pmp_stubs),
        Cmd::Boot {
            ram,
            bios,
            kernel,
            initrd,
            dtb,
            disk,
            disk_volatile,
            fw_dynamic,
            net,
            relay,
            snapshot,
            verbose,
        } => console::cmd_run(&run::BootPaths {
            ram_mb: ram,
            bios,
            kernel,
            initrd,
            dtb,
            disk,
            disk_volatile,
            fw_dynamic,
            net,
            relay_url: relay_url(relay),
            snapshot,
            verbose,
        }),
        Cmd::Snapshot {
            ram,
            bios,
            kernel,
            initrd,
            dtb,
            disk,
            disk_id,
            desktop,
            out,
            verbose,
        } => capture::cmd_snapshot(&capture::CaptureArgs {
            ram_mb: ram,
            bios,
            kernel,
            initrd,
            dtb,
            disk,
            disk_id,
            desktop,
            out,
            verbose,
        }),
    };
    std::process::exit(code);
}
