//! The display end to end, with no X and no compositor: the guest binds
//! `simpledrm` to the devicetree's `simple-framebuffer`, a write to `/dev/fb0`
//! lands in the emulator's DRAM at `FB_BASE`, and the dirty-row scan names the
//! row that changed.
//!
//! Injected keyboard events must also reach the guest evdev device.

use crate::support as common;

use common::{Guest, Pattern, SHELL_PROMPT};
use emulate_core::devices::framebuffer::{FB_BASE, FB_HEIGHT, FB_STRIDE};

/// Blue in `x8r8g8b8`: memory order is B, G, R, X.
const BLUE_PIXEL: u32 = 0x00_00_00_ff;
/// KEY_A, the code a browser's `KeyA` maps to.
const KEY_A: u16 = 30;

/// Run the guest until it prints `marker`.
fn until(guest: &mut Guest, marker: &str, what: &str) {
    guest.run_until(&Pattern::Line(marker.as_bytes()), what);
}

/// Row the test paints. Deliberately not row 0: with no console on tty0 the
/// fbcon cursor sits blinking on the top-left character cell and would repaint
/// over the pixel between the guest's write and our read.
const TEST_ROW: u32 = 400;

/// The four bytes of pixel (0, [`TEST_ROW`]), read out of guest DRAM.
fn test_pixel(guest: &mut Guest) -> u64 {
    let at = FB_BASE + u64::from(TEST_ROW) * u64::from(FB_STRIDE);
    guest
        .machine_mut()
        .read_phys(at, 4)
        .expect("framebuffer is inside DRAM")
}

fn dirty_rows(guest: &mut Guest) -> Vec<u32> {
    let mut rows = Vec::new();
    guest.machine_mut().framebuffer_dirty_rows(&mut rows);
    rows
}

#[test]
#[ignore = "requires prepared guest artifacts; run just test-guest"]
fn the_guest_paints_the_framebuffer_and_takes_injected_input() {
    common::require_guest();

    let mut guest = Guest::boot_linux(None, None);
    guest.run_until(&SHELL_PROMPT, "the BusyBox ash prompt");

    // ---- the display ----
    //
    // simpledrm and fbcon are built into Alpine's kernel, so the DT node alone
    // is enough: nothing was loaded to get here.
    guest.shell("cat /sys/class/graphics/fb0/virtual_size");
    guest.run_until(
        &Pattern::Line(b"1024,768"),
        "the fbdev geometry from the devicetree",
    );
    guest.shell("cat /sys/class/graphics/fb0/name");
    guest.run_until(
        &Pattern::Text(b"simpledrm"),
        "simpledrm bound to the simple-framebuffer node",
    );

    // Everything painted during boot is already on the host's canvas as far as
    // this test is concerned; take the baseline now so what follows is only
    // the pixel we are about to write.
    let painted = dirty_rows(&mut guest);
    assert!(
        !painted.is_empty(),
        "the guest should have written the framebuffer during boot"
    );

    guest.shell(
        "printf '\\377\\000\\000\\000' | \
           dd of=/dev/fb0 bs=4096 seek=400 conv=notrunc 2>/dev/null; \
         echo FB_WRITTEN",
    );
    until(&mut guest, "FB_WRITTEN", "the framebuffer write");
    // fbdev emulation writes a shadow buffer and a worker copies the damaged
    // rectangle into the real framebuffer; give it a scheduling slot.
    guest.shell("sleep 1; echo FB_SETTLED");
    until(&mut guest, "FB_SETTLED", "the damage flush");

    assert_eq!(
        test_pixel(&mut guest) as u32,
        BLUE_PIXEL,
        "a write to /dev/fb0 must land in DRAM at FB_BASE + row * stride, \
         in x8r8g8b8"
    );
    let rows = dirty_rows(&mut guest);
    assert!(
        rows.contains(&TEST_ROW),
        "row {TEST_ROW} changed but was not reported: {rows:?}"
    );
    assert!(
        rows.len() < FB_HEIGHT as usize / 2,
        "a four-byte write should not dirty half the screen: {} rows",
        rows.len()
    );

    // ---- input ----

    guest.shell(
        "for m in virtio_input evdev; do \
           insmod /lib/modules/$(uname -r)/$m.ko 2>/dev/null; done; \
         if [ -e /dev/input/event0 ]; then echo EVDEV_'PRESENT'; \
         else echo EVDEV_'ABSENT'; fi",
    );
    // The markers are quoted in the command above so the terminal's echo of
    // what was typed cannot itself satisfy the pattern.
    let probe = guest.run_until(
        &Pattern::Any(&[
            Pattern::Line(b"EVDEV_PRESENT"),
            Pattern::Line(b"EVDEV_ABSENT"),
        ]),
        "the evdev probe",
    );
    assert!(
        common::contains(&probe, b"EVDEV_PRESENT"),
        "guest is missing virtio_input/evdev modules; rebuild with just guest"
    );

    // Both devices, named as emulate-core advertises them.
    guest.shell("cat /sys/class/input/event*/device/name");
    guest.run_until(
        &Pattern::Line(b"emulate keyboard"),
        "the virtio keyboard's name",
    );
    guest.run_until(
        &Pattern::Line(b"emulate tablet"),
        "the virtio tablet's name",
    );

    // event0 is the keyboard: it is the first virtio-input device on the bus
    // (0x1000_4000), and evdev numbers them in probe order.
    //
    // The reader is backgrounded and given a moment to open the device before
    // the events go in: the driver posts its buffers at probe, so anything
    // injected before a client exists is delivered to the input core and
    // dropped there. Exactly four events are expected (press, SYN, release,
    // SYN), so `head` returns on its own.
    guest.shell(
        "timeout 30 head -c 96 /dev/input/event0 > /tmp/ev & \
         sleep 2; echo READER_READY",
    );
    guest.run_until(&Pattern::Line(b"READER_READY"), "the evdev reader to open");
    guest.machine_mut().input_key(KEY_A, true);
    guest.machine_mut().input_key(KEY_A, false);
    guest.shell("wait; od -An -tx1 -v < /tmp/ev | tr -d ' \\n'; echo; echo KEY_READ_DONE");
    guest.machine_mut().input_key(KEY_A, true);
    guest.machine_mut().input_key(KEY_A, false);
    let captured = guest.run_until(&Pattern::Line(b"KEY_READ_DONE"), "the evdev bytes");

    // `struct input_event` on riscv64 is { __kernel_ulong_t sec, usec; u16
    // type; u16 code; s32 value } = 24 bytes, little-endian. Look for the
    // 8-byte tail of the EV_KEY press: type 0x0001, code 0x001e, value 1.
    let hex = String::from_utf8_lossy(&captured);
    assert!(
        hex.contains("01001e0001000000"),
        "no EV_KEY/KEY_A press in the evdev stream:\n{hex}"
    );
    assert!(
        hex.contains("01001e0000000000"),
        "no EV_KEY/KEY_A release in the evdev stream:\n{hex}"
    );

    common::pass(
        "display: simple-framebuffer painted, /dev/fb0 write seen in DRAM, \
         dirty row reported, injected key read back from /dev/input/event0",
    );
}

#[test]
#[ignore = "requires prepared desktop rootfs; run just test-guest"]
fn desktop_tracks_absolute_pointer_buttons_and_keyboard() {
    let until = |guest: &mut Guest, marker: &str, what: &str| {
        guest.run_until(&Pattern::Text(marker.as_bytes()), what);
    };
    common::require_guest();
    let source = std::env::var_os("EMULATE_DESKTOP_TEST_DISK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| common::repo_root().join("guest/out/alpine-rootfs.ext4"));
    let scratch = common::scratch_dir("desktop-input");
    let disk = scratch.join("rootfs.ext4");
    common::sparse_copy(&source, &disk);
    let root = common::repo_root().join("guest/out");
    let mut guest = Guest::boot(
        "desktop",
        emulate_cli::run::BootPaths {
            ram_mb: 128,
            bios: Some(root.join("fw.bin")),
            kernel: Some(root.join("Image")),
            initrd: Some(root.join("initramfs.cpio.gz")),
            dtb: Some(
                source
                    .parent()
                    .expect("disk directory")
                    .join("virt-desktop.dtb"),
            ),
            disk: Some(disk.clone()),
            fw_dynamic: Some(true),
            ..Default::default()
        },
    );
    guest.set_timeout(std::time::Duration::from_secs(900));
    guest.run_until(&common::ROOTFS_PROMPT, "the Alpine login shell");
    guest.run_until_control(b"0 desktop ready\n");
    assert!(
        guest.machine_mut().bus.framebuffer_blitter.committed(),
        "XLibre must commit frames after startup"
    );
    guest.shell("for app in xfbdev wm; do p=$(cat /tmp/.desktop-0/$app.pid); [ \"$(awk '{print $19}' /proc/$p/stat)\" = -5 ] || exit 1; done; echo DESKTOP_PRIORITY_'OK'");
    until(
        &mut guest,
        "DESKTOP_PRIORITY_OK",
        "interactive desktop scheduling priority",
    );
    guest.shell("grep -q 'framebuffer copy/fill acceleration enabled' /tmp/.desktop-0/xfbdev.log && echo BLITTER_'READY'");
    until(
        &mut guest,
        "BLITTER_READY",
        "XLibre framebuffer acceleration",
    );
    guest.shell(&format!(
        "cat > /tmp/x11-input.py <<'PY'\n{}\nPY\necho PROBE_'READY'",
        include_str!("fixtures/x11-input.py")
    ));
    until(&mut guest, "PROBE_READY", "the X11 input probe");

    guest.machine_mut().input_pointer_abs(32767, 32767);
    guest.machine_mut().input_button(0x110, true);
    guest.machine_mut().input_key(KEY_A, true);
    guest.shell("python3 /tmp/x11-input.py 1023 767 1 1");
    until(
        &mut guest,
        "X11_INPUT_OK 1023 767 1 1",
        "X11 input press state",
    );

    guest.machine_mut().input_button(0x110, false);
    guest.machine_mut().input_key(KEY_A, false);
    guest.machine_mut().input_pointer_abs(0, 0);
    guest.shell("python3 /tmp/x11-input.py 0 0 0 0");
    until(
        &mut guest,
        "X11_INPUT_OK 0 0 0 0",
        "X11 input release state",
    );

    for (width, height) in [(1440, 900), (800, 600)] {
        guest
            .machine_mut()
            .bus
            .framebuffer_blitter
            .request_size(width, height)
            .unwrap();
        guest.shell(&format!(
            "for i in $(seq 1 60); do DISPLAY=:0 xprop -root _NET_WORKAREA | grep -q '0, 0, {width}, {}$' && break; sleep 0.1; done; DISPLAY=:0 xprop -root _NET_WORKAREA; echo RESIZE_'CHECK'",
            height - 33,
        ));
        let output = guest.run_until(
            &Pattern::Line(b"RESIZE_CHECK"),
            "the window manager's resized work area",
        );
        let expected = format!("_NET_WORKAREA(CARDINAL) = 0, 0, {width}, {}", height - 33);
        assert!(output
            .windows(expected.len())
            .any(|part| part == expected.as_bytes()));
        assert_eq!(
            guest.machine_mut().bus.framebuffer_blitter.size(),
            (width, height)
        );
        guest.machine_mut().input_pointer_abs(32767, 32767);
        guest.shell(&format!(
            "python3 /tmp/x11-input.py {} {} 0 0",
            width - 1,
            height - 1
        ));
        until(
            &mut guest,
            &format!("X11_INPUT_OK {} {} 0 0", width - 1, height - 1),
            "pointer coordinates after resizing",
        );
    }

    guest.shell("emuctl desktop stop; echo DESKTOP_'STOPPED'");
    until(&mut guest, "DESKTOP_STOPPED", "desktop shutdown");
    drop(guest);
    std::fs::remove_dir_all(scratch).expect("remove desktop scratch disk");
}
