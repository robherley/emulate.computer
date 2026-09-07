//! In-process guest driver and isolated disk/relay fixtures.

pub use emulate_cli::headless::{contains, CURSOR_QUERY};
use emulate_cli::headless::{Headless, Stopped};
use emulate_cli::net::NetMode;
use emulate_cli::run::{build_machine, BootImages, BootPaths};
use emulate_core::machine::Machine;
use std::fs::File;
use std::io::BufRead;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Wall-clock budget for a whole test. Generous so a hung guest reports its
/// console tail rather than sitting until the CI job timeout.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(600);
pub use super::artifacts::{repo_root, require_guest, require_rootfs, require_xv6};

/// A private scratch directory under `target/`, emptied first, so a copy of a
/// 512MB disk image stays inside the build tree.
pub fn scratch_dir(name: &str) -> PathBuf {
    let dir = repo_root().join("target/integration-scratch").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

/// Copy `source` to `destination`, punching holes for all-zero chunks so a
/// sparse 512MB image does not become 512MB on disk.
pub fn sparse_copy(source: &Path, destination: &Path) {
    let mut input = File::open(source).expect("open source image");
    let mut output = File::create(destination).expect("create scratch image");
    let logical = input.metadata().expect("stat source image").len();
    let mut chunk = vec![0u8; 1 << 20];
    loop {
        let read = input.read(&mut chunk).expect("read source image");
        if read == 0 {
            break;
        }
        if chunk[..read].iter().any(|&b| b != 0) {
            output
                .write_all(&chunk[..read])
                .expect("write scratch image");
        } else {
            output
                .seek(SeekFrom::Current(read as i64))
                .expect("seek scratch image");
        }
    }
    output.set_len(logical).expect("size scratch image");
}

/// `bun run server.ts` in `relay/`, on a free port.
///
/// The guest reaches host services through the gateway alias `10.0.2.2`, which
/// this test relay explicitly permits as a loopback destination.
pub struct BunRelay {
    child: Child,
    url: String,
}

impl BunRelay {
    /// Start a local relay; Bun must be installed.
    pub fn start() -> Self {
        let mut child = Command::new("bun")
            .args(["run", "server.ts"])
            .current_dir(repo_root().join("relay"))
            // A free port, reported on the startup line below.
            .env("PORT", "0")
            .env("RELAY_ALLOW_PRIVATE", "true")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the Bun relay; install Bun before running just test-guest");

        // `[relay] listening on ws://127.0.0.1:<port>/`
        let stderr = child.stderr.take().expect("relay stderr");
        let mut reader = std::io::BufReader::new(stderr);
        let mut url = None;
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            eprint!("[relay] {line}");
            if let Some((_, tail)) = line.split_once("listening on ") {
                url = Some(tail.trim().to_string());
                break;
            }
            line.clear();
        }
        let url = url.unwrap_or_else(|| {
            let _ = child.kill();
            panic!("the Bun relay never reported a listening address");
        });
        // Keep draining, or the relay blocks once the pipe fills.
        std::thread::spawn(move || {
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                eprint!("[relay] {line}");
                line.clear();
            }
        });
        eprintln!("[harness] relay at {url}");
        Self { child, url }
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for BunRelay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What [`Guest::run_until`] waits for. Few enough shapes that a dependency on
/// `regex` would not earn its place.
pub enum Pattern<'a> {
    /// Anywhere in the output.
    Text(&'a [u8]),
    /// A whole line equal to this, CR/LF terminated.
    Line(&'a [u8]),
    /// A line starting with `prefix` and containing `then` later on.
    LineWith { prefix: &'a [u8], then: &'a [u8] },
    /// Any of these.
    Any(&'a [Pattern<'a>]),
}

/// A slice of the ASCII-art `emulate.computer` banner both guests print as
/// their motd (guest/overlays/initramfs/etc/motd, guest/overlays/rootfs/etc/motd).
pub const MOTD: Pattern<'static> = Pattern::Text(b"(_)__\\___/");

/// The initramfs shell prompt: root, BusyBox ash.
pub const SHELL_PROMPT: Pattern<'static> = Pattern::LineWith {
    prefix: b"root@emulate.computer:",
    then: b"# ",
};
/// The persistent root's bash prompt (`root@emulate.computer:~# `).
pub const ROOTFS_PROMPT: Pattern<'static> = Pattern::LineWith {
    prefix: b"root@emulate.computer:",
    then: b"# ",
};
/// The xv6 shell prompt.
pub const XV6_PROMPT: Pattern<'static> = Pattern::LineWith {
    prefix: b"$ ",
    then: b"",
};
impl Pattern<'_> {
    pub fn matches(&self, haystack: &[u8]) -> bool {
        match self {
            Pattern::Text(needle) => contains(haystack, needle),
            Pattern::Line(needle) => lines(haystack)
                .iter()
                .any(|(line, terminated)| *terminated && line == needle),
            Pattern::LineWith { prefix, then } => lines(haystack)
                .iter()
                .any(|(line, _)| line.starts_with(prefix) && contains(&line[prefix.len()..], then)),
            Pattern::Any(patterns) => patterns.iter().any(|p| p.matches(haystack)),
        }
    }

    fn describe(&self) -> String {
        match self {
            Pattern::Text(needle) => format!("{:?}", String::from_utf8_lossy(needle)),
            Pattern::Line(needle) => format!("the line {:?}", String::from_utf8_lossy(needle)),
            Pattern::LineWith { prefix, then } => format!(
                "a line starting {:?} containing {:?}",
                String::from_utf8_lossy(prefix),
                String::from_utf8_lossy(then)
            ),
            Pattern::Any(patterns) => patterns
                .iter()
                .map(Pattern::describe)
                .collect::<Vec<_>>()
                .join(" or "),
        }
    }
}

/// Split into `(line, terminated)` pairs on CR and LF. The trailing partial
/// line is included, unterminated: a prompt is never followed by a newline.
fn lines(data: &[u8]) -> Vec<(&[u8], bool)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' || b == b'\r' {
            out.push((&data[start..i], true));
            start = i + 1;
        }
    }
    out.push((&data[start..], false));
    out
}

/// Every dotted-quad in `data`, in order.
pub fn ipv4_addresses(data: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(data);
    let mut found = Vec::new();
    let mut token = String::new();
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() || ch == '.' {
            token.push(ch);
        } else {
            let parts: Vec<&str> = token.split('.').collect();
            if parts.len() == 4
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.parse::<u8>().is_ok())
            {
                found.push(token.clone());
            }
            token.clear();
        }
    }
    found
}

/// A guest running in this process.
///
/// The machine is rebuilt from the same images when the guest asks for a reset,
/// which is what makes the rootfs test's `reboot` a real reboot.
pub struct Guest {
    label: String,
    images: BootImages,
    driver: Headless,
    started: Instant,
    deadline: Instant,
}

impl Guest {
    pub fn run_until_control(&mut self, needle: &[u8]) {
        self.driver
            .run_until_control(
                needle,
                self.deadline.saturating_duration_since(Instant::now()),
            )
            .expect("guest control response");
    }

    /// Boot OpenSBI + Linux. `disk` switches to the block-root DTB and attaches
    /// the image; `relay` (a `ws://` URL) attaches the user-mode NAT.
    pub fn boot_linux(disk: Option<&Path>, relay: Option<&str>) -> Self {
        let root = repo_root();
        let paths = BootPaths {
            ram_mb: 128,
            bios: Some(root.join("guest/out/fw.bin")),
            kernel: Some(root.join("guest/out/Image")),
            initrd: Some(root.join("guest/out/initramfs.cpio.gz")),
            dtb: Some(root.join(if disk.is_some() {
                "guest/out/virt-rootfs.dtb"
            } else {
                "guest/out/virt.dtb"
            })),
            disk: disk.map(Path::to_path_buf),
            fw_dynamic: Some(true),
            net: if relay.is_some() {
                NetMode::User
            } else {
                NetMode::None
            },
            relay_url: relay.unwrap_or_default().to_string(),
            ..BootPaths::default()
        };
        Self::boot(
            if disk.is_some() {
                "linux-rootfs"
            } else {
                "linux"
            },
            paths,
        )
    }

    /// Boot upstream xv6 as a bare M-mode kernel on a volatile copy of its
    /// filesystem image.
    pub fn boot_xv6() -> Self {
        let root = repo_root();
        let paths = BootPaths {
            kernel: Some(root.join("vendor/xv6-riscv/kernel/kernel")),
            disk: Some(root.join("vendor/xv6-riscv/fs.img")),
            disk_volatile: true,
            ..BootPaths::default()
        };
        Self::boot("xv6", paths)
    }

    pub fn boot(label: &str, paths: BootPaths) -> Self {
        let images = BootImages::load(&paths).expect("load boot images");
        let machine = build_machine(&images).expect("build machine");
        let now = Instant::now();
        Self {
            label: label.to_owned(),
            images,
            driver: Headless::new(machine, true).expect("headless guest"),
            started: now,
            deadline: now + DEFAULT_DEADLINE,
        }
    }

    pub fn machine_mut(&mut self) -> &mut Machine {
        self.driver.machine_mut()
    }

    /// Replace the wall-clock budget, measured from now.
    pub fn set_timeout(&mut self, budget: Duration) {
        self.deadline = Instant::now() + budget;
    }

    /// Wall-clock time since this guest was created.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn clear(&mut self) {
        self.driver.clear();
    }

    /// Clear the captured output and run one command line in the guest shell.
    pub fn shell(&mut self, command: &str) {
        self.driver.shell(command);
    }

    /// Step the machine until `pattern` shows up in the captured output, and
    /// return that output. Panics — with the console tail — on timeout, or if
    /// the guest shuts down first.
    pub fn run_until(&mut self, pattern: &Pattern, what: &str) -> Vec<u8> {
        let description = format!("{what} ({})", pattern.describe());
        loop {
            match self.driver.run_until_matching(
                |output| pattern.matches(output),
                self.deadline,
                &description,
            ) {
                Ok(()) => return self.driver.output().to_vec(),
                Err(Stopped::Reset) => self.driver.replace_machine(
                    build_machine(&self.images).expect("rebuild machine on reset"),
                ),
                Err(error) => panic!("{}: {error}", self.label),
            }
        }
    }
}

/// The one line every integration test prints when it passes.
pub fn pass(summary: &str) {
    println!("\n{summary}");
}
