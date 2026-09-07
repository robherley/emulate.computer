//! End-to-end snapshots against the real guest artifacts: capture one from the
//! Alpine root filesystem, resume it, and check that what comes back is a
//! working machine rather than a plausible-looking one.
//!
//! The capture is the expensive part (a full boot), so every assertion that
//! can share one snapshot does. Everything runs in this process through
//! `emulate_cli`; nothing spawns the binary.

use crate::support as common;

use emulate_cli::capture::{capture, CaptureArgs};
use emulate_cli::headless::{Headless, Stopped};
use emulate_cli::run::{build_machine, restore_machine, BootImages, BootPaths};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The persistent root's prompt (`root@emulate.computer:~# `).
const PROMPT: &[u8] = b"root@emulate.computer:";
const RESTORE_BUDGET: Duration = Duration::from_secs(120);
const COMMAND_BUDGET: Duration = Duration::from_secs(120);
const BOOT_BUDGET: Duration = Duration::from_secs(600);
/// The disk id the browser keys its OPFS image on. Only its equality matters
/// here; the native runner does not check it.
const DISK_ID: &str = "integration-test-root-v1";

struct Fixture {
    /// The pristine seed image the snapshot was captured against.
    seed: PathBuf,
    snapshot: PathBuf,
    dir: PathBuf,
}

impl Fixture {
    fn boot_paths(&self, disk: &Path, snapshot: Option<&Path>) -> BootPaths {
        let root = common::repo_root();
        BootPaths {
            ram_mb: 128,
            bios: Some(root.join("guest/out/fw.bin")),
            kernel: Some(root.join("guest/out/Image")),
            initrd: Some(root.join("guest/out/initramfs.cpio.gz")),
            dtb: Some(root.join("guest/out/virt-rootfs.dtb")),
            disk: Some(disk.to_path_buf()),
            fw_dynamic: Some(true),
            snapshot: snapshot.map(Path::to_path_buf),
            ..BootPaths::default()
        }
    }

    /// A fresh copy of the pristine seed, which is what a restore requires.
    fn seeded_disk(&self, name: &str) -> PathBuf {
        let disk = self.dir.join(name);
        common::sparse_copy(&self.seed, &disk);
        disk
    }

    /// Restore onto `disk` and drive it to a prompt, returning the guest and
    /// how long the restore-to-prompt took.
    fn resume(&self, disk: &Path) -> (Headless, Duration) {
        let started = Instant::now();
        let images =
            BootImages::load(&self.boot_paths(disk, Some(&self.snapshot))).expect("load images");
        let mut machine = restore_machine(&images).expect("restore the snapshot");
        // The captured guest was idle at a prompt with an empty UART, so the
        // console stays blank until the shell is given a reason to redraw.
        machine.uart_input(b"\n");
        let mut guest = Headless::new(machine, false).expect("headless guest");
        guest
            .run_until(PROMPT, RESTORE_BUDGET)
            .expect("restored guest reaches its prompt");
        (guest, started.elapsed())
    }
}

/// The one capture every test in this file shares: booting a guest to take it
/// is the expensive part, and nothing here mutates it.
fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(build_fixture)
}

/// Capture against a private copy of the seed image.
fn build_fixture() -> Fixture {
    common::require_rootfs();
    let root = common::repo_root();
    let dir = common::scratch_dir("snapshot");
    let seed = dir.join("seed.ext4");
    common::sparse_copy(&root.join("guest/out/alpine-rootfs.ext4"), &seed);

    let snapshot = dir.join("snapshot.bin");
    let summary = capture(&CaptureArgs {
        desktop: false,
        ram_mb: 128,
        bios: root.join("guest/out/fw.bin"),
        kernel: root.join("guest/out/Image"),
        initrd: Some(root.join("guest/out/initramfs.cpio.gz")),
        dtb: root.join("guest/out/virt-rootfs.dtb"),
        disk: seed.clone(),
        disk_id: String::from(DISK_ID),
        out: snapshot.clone(),
        verbose: false,
    })
    .expect("capture a snapshot");
    eprintln!("{summary}");

    // The capture must not have touched the image it was pointed at; that is
    // what keeps the shipped browser seed byte-reproducible.
    let pristine = std::fs::read(root.join("guest/out/alpine-rootfs.ext4")).expect("read seed");
    let after = std::fs::read(&seed).expect("read the fixture seed");
    assert_eq!(
        pristine, after,
        "capture modified the image it was given; it must run on a scratch copy"
    );

    Fixture {
        seed,
        snapshot,
        dir,
    }
}

#[test]
#[ignore = "requires prepared guest artifacts; run just test-guest"]
fn a_restored_guest_answers_at_its_shell_far_faster_than_it_boots() {
    let fixture = fixture();

    let disk = fixture.seeded_disk("restore.ext4");
    let (mut guest, restore_elapsed) = fixture.resume(&disk);

    let answer = run_command(&mut guest, "echo RESTORED_OK");
    assert!(
        answer.contains("RESTORED_OK"),
        "the restored shell did not run a command: {answer:?}"
    );

    // The wall clock is the host's, not the one frozen into the container: a
    // snapshot that sat on a CDN for a week must not resume into last week.
    let host_now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("host clock")
        .as_secs();
    let printed = run_command(&mut guest, "date +%s");
    let guest_now =
        last_integer(&printed).unwrap_or_else(|| panic!("no Unix timestamp in {printed:?}"));
    let skew = guest_now.abs_diff(host_now as i64);
    assert!(
        skew < 300,
        "restored guest clock is {skew}s from the host's ({guest_now} vs {host_now})"
    );

    // Timing, for docs/disk.md. Not asserted tightly: this runs on
    // whatever CI machine is free, and the point is the order of magnitude.
    let boot_elapsed = time_a_normal_boot(fixture);
    eprintln!(
        "\n[timing] restore to prompt {:.2}s vs boot to prompt {:.2}s",
        restore_elapsed.as_secs_f64(),
        boot_elapsed.as_secs_f64()
    );
    assert!(
        restore_elapsed < boot_elapsed,
        "restoring ({restore_elapsed:?}) was not faster than booting ({boot_elapsed:?})"
    );

    common::pass(&format!(
        "snapshot: restored to a working shell in {:.2}s (boot takes {:.2}s)",
        restore_elapsed.as_secs_f64(),
        boot_elapsed.as_secs_f64()
    ));
}

#[test]
#[ignore = "requires prepared guest artifacts; run just test-guest"]
fn writes_made_after_a_restore_are_still_there_on_the_next_normal_boot() {
    let fixture = fixture();

    let disk = fixture.seeded_disk("persist.ext4");
    let (mut guest, _) = fixture.resume(&disk);
    let answer = run_command(
        &mut guest,
        "echo written-after-restore > /root/marker && sync && echo DONE",
    );
    assert!(
        answer.contains("DONE"),
        "the write did not land: {answer:?}"
    );
    guest
        .machine_mut()
        .flush_disk()
        .expect("flush the restored guest's disk");
    drop(guest);

    // A full boot from that disk — no snapshot — must see the file.
    let images = BootImages::load(&fixture.boot_paths(&disk, None)).expect("load images");
    let machine = build_machine(&images).expect("build machine");
    let mut booted = Headless::new(machine, false).expect("headless guest");
    booted
        .run_until(PROMPT, BOOT_BUDGET)
        .expect("normal boot reaches its prompt");
    let marker = run_command(&mut booted, "cat /root/marker");
    assert!(
        marker.contains("written-after-restore"),
        "the file written after the restore did not survive the reboot: {marker:?}"
    );

    common::pass("snapshot: writes made after a restore persist across a normal boot");
}

#[test]
#[ignore = "requires prepared guest artifacts; run just test-guest"]
fn a_guest_that_reboots_out_of_a_restored_state_boots_normally() {
    let fixture = fixture();

    let disk = fixture.seeded_disk("reboot.ext4");
    let (mut guest, _) = fixture.resume(&disk);
    guest.shell("reboot -f");
    match guest.run_until(b"never-printed", Duration::from_secs(120)) {
        Err(Stopped::Reset) => {}
        other => panic!("expected the guest to request a reset, got {other:?}"),
    }
    guest
        .machine_mut()
        .flush_disk()
        .expect("flush before the reboot");
    drop(guest);

    // The reset path rebuilds from the images, exactly as `console::cmd_run`
    // does: a reboot out of a restored machine is a real boot.
    let images = BootImages::load(&fixture.boot_paths(&disk, None)).expect("load images");
    let machine = build_machine(&images).expect("rebuild on reset");
    let mut rebooted = Headless::new(machine, false).expect("headless guest");
    rebooted
        .run_until(PROMPT, BOOT_BUDGET)
        .expect("the rebooted guest reaches its prompt");

    common::pass("snapshot: a reboot out of a restored machine boots from the images");
}

#[test]
#[ignore = "requires prepared guest artifacts; run just test-guest"]
fn a_snapshot_is_refused_when_the_boot_images_are_not_the_ones_it_was_taken_from() {
    let fixture = fixture();

    // Swap the DTB for the initramfs one: same shape, different bytes, so the
    // identity hash changes and nothing else about the boot does.
    let root = common::repo_root();
    let mut paths = fixture.boot_paths(&fixture.seed, Some(&fixture.snapshot));
    paths.dtb = Some(root.join("guest/out/virt.dtb"));
    let images = BootImages::load(&paths).expect("load images");

    let Err(error) = restore_machine(&images) else {
        panic!("a snapshot from other boot images must not restore");
    };
    assert!(
        error.contains("different boot images"),
        "unexpected error: {error}"
    );

    common::pass("snapshot: a container from other boot images is refused");
}

/// How long a plain boot of the same guest takes, for the comparison above.
fn time_a_normal_boot(fixture: &Fixture) -> Duration {
    let disk = fixture.seeded_disk("boot-timing.ext4");
    let started = Instant::now();
    let images = BootImages::load(&fixture.boot_paths(&disk, None)).expect("load images");
    let machine = build_machine(&images).expect("build machine");
    let mut guest = Headless::new(machine, false).expect("headless guest");
    guest
        .run_until(PROMPT, BOOT_BUDGET)
        .expect("normal boot reaches its prompt");
    started.elapsed()
}

/// Run one shell command and return what it printed.
///
/// The guest echoes the line as it is typed, so waiting for a word the command
/// *contains* matches the echo rather than the result. Waiting for the next
/// prompt is the only reliable "it finished" signal; the echoed line is then
/// dropped from the front of the capture.
fn run_command(guest: &mut Headless, command: &str) -> String {
    guest.shell(command);
    guest
        .run_until(PROMPT, COMMAND_BUDGET)
        .unwrap_or_else(|why| panic!("command {command:?} did not finish: {why}"));
    let captured = String::from_utf8_lossy(guest.output()).into_owned();
    match captured.split_once('\n') {
        Some((_echoed, rest)) => rest.to_owned(),
        None => captured,
    }
}

/// The last run of at least ten digits in `output` — `date +%s`'s answer.
fn last_integer(output: &str) -> Option<i64> {
    output
        .split(|c: char| !c.is_ascii_digit())
        .rfind(|token| token.len() >= 10)
        .and_then(|token| token.parse().ok())
}

#[test]
#[ignore = "requires prepared desktop rootfs; run just test-guest"]
fn a_graphical_snapshot_resumes_and_reports_ready_without_console_input() {
    common::require_guest();
    let root = common::repo_root().join("guest/out");
    let source = std::env::var_os("EMULATE_DESKTOP_TEST_DISK")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("alpine-rootfs.ext4"));
    let dir = common::scratch_dir("desktop-snapshot");
    let snapshot = dir.join("snapshot.bin");
    let dtb = source
        .parent()
        .expect("disk directory")
        .join("virt-desktop.dtb");
    capture(&CaptureArgs {
        desktop: true,
        ram_mb: 128,
        bios: root.join("fw.bin"),
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio.gz")),
        dtb: dtb.clone(),
        disk: source.clone(),
        disk_id: DISK_ID.into(),
        out: snapshot.clone(),
        verbose: false,
    })
    .expect("capture running desktop");
    let disk = dir.join("restore.ext4");
    common::sparse_copy(&source, &disk);
    let images = BootImages::load(&BootPaths {
        ram_mb: 128,
        bios: Some(root.join("fw.bin")),
        kernel: Some(root.join("Image")),
        initrd: Some(root.join("initramfs.cpio.gz")),
        dtb: Some(dtb),
        disk: Some(disk),
        snapshot: Some(snapshot),
        net: emulate_cli::net::NetMode::User,
        relay_url: "ws://127.0.0.1:9".into(),
        fw_dynamic: Some(true),
        ..Default::default()
    })
    .expect("load desktop restore images");
    let machine = restore_machine(&images).expect("restore graphical snapshot");
    assert!(
        machine.bus.framebuffer_blitter.committed(),
        "snapshot must retain the committed desktop image"
    );
    let mut guest = Headless::new(machine, false).expect("headless desktop");
    guest
        .run_until_control(b"0 desktop ready\n", RESTORE_BUDGET)
        .expect("desktop heartbeat without host keyboard input");
    guest
        .run_until_control(b"0 network 10.0.2.15/24\n", RESTORE_BUDGET)
        .expect("DHCP after snapshot restore without a console command");
    guest.shell("DISPLAY=:0 xprop -name Terminal WM_STATE; echo DESKTOP_'CLIENTS'");
    guest
        .run_until(b"DESKTOP_CLIENTS", COMMAND_BUDGET)
        .expect("query restored X clients");
    assert!(
        common::contains(guest.output(), b"window state: Normal"),
        "restored window manager must still manage the terminal"
    );
    drop(guest);
    std::fs::remove_dir_all(dir).expect("remove desktop snapshot scratch files");
}

#[test]
#[ignore = "requires prepared Alpine rootfs"]
fn control_actions_preserve_console_input() {
    let fixture = fixture();
    let disk = fixture.seeded_disk("control.ext4");
    let (mut guest, _) = fixture.resume(&disk);
    guest
        .run_until_control(b"0 desktop stopped\n", RESTORE_BUDGET)
        .unwrap();
    guest
        .run_until_control(b"0 ready\n", RESTORE_BUDGET)
        .unwrap();
    guest.send("printf 'CONTROL_%s\\n' ");
    guest.settle(Duration::from_millis(100)).unwrap();
    guest.clear();
    guest
        .machine_mut()
        .set_net_backend(Box::new(emulate_core::hostnet::NatBackend::new(
            emulate_cli::net::substrate("ws://127.0.0.1:9"),
        )));
    for (id, action) in [
        (1, "network.connect"),
        (2, "network.disconnect"),
        (3, "desktop.start"),
        (4, "desktop.stop"),
    ] {
        assert!(guest
            .machine_mut()
            .control_input(format!("{id} {action}\n").as_bytes()));
        guest
            .run_until_control(format!("{id} ok\n").as_bytes(), RESTORE_BUDGET)
            .unwrap();
        assert!(
            guest.output().is_empty(),
            "control action polluted UART: {}",
            String::from_utf8_lossy(guest.output())
        );
    }
    assert!(guest.machine_mut().control_input(b"5 shell echo BAD\n"));
    guest
        .run_until_control(b"5 error\n", RESTORE_BUDGET)
        .unwrap();
    guest.send("PRESERVED\n");
    guest
        .run_until(b"CONTROL_PRESERVED", COMMAND_BUDGET)
        .unwrap();
    drop(guest);
    std::fs::remove_file(disk).unwrap();
}
