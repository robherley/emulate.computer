//! Linux on the persistent Alpine ext4 root: `/dev/vda` is the root
//! filesystem, the console auto-logs into root with bash and QuickJS, and a
//! write survives a real guest reboot.

use crate::support as common;

use common::{Guest, Pattern, ROOTFS_PROMPT};
use std::time::Duration;

const MARKER: &[u8] = b"EMULATE_ROOTFS_PERSIST_OK";

#[test]
#[ignore = "requires prepared guest artifacts; run just test-guest"]
fn linux_mounts_the_block_root_and_keeps_writes_across_a_reboot() {
    common::require_rootfs();

    // Never boot the real image: guest writes are permanent, and this test
    // makes them on purpose.
    let scratch = common::scratch_dir("linux-rootfs");
    let disk = scratch.join("rootfs.ext4");
    common::sparse_copy(
        &common::repo_root().join("guest/out/alpine-rootfs.ext4"),
        &disk,
    );

    let mut guest = Guest::boot_linux(Some(&disk), None);
    guest.set_timeout(Duration::from_secs(900));
    guest.run_until(&ROOTFS_PROMPT, "the Alpine block-root login shell");
    let shell = guest.elapsed();

    // Auto-login landed on root, and its toolchain works.
    guest.shell(
        "id -un; \
         bash --version | head -1; \
         qjs -e 'console.log(\"EMULATE_QUICKJS_OK\", 1 + 1)'",
    );
    guest.run_until(&Pattern::Line(b"root"), "the auto-logged-in root account");
    guest.run_until(&Pattern::Text(b"GNU bash, version 5.3"), "the bash build");
    guest.run_until(
        &Pattern::Line(b"EMULATE_QUICKJS_OK 2"),
        "the QuickJS interpreter",
    );

    guest.shell(
        "python3 -c 'import platform; print(\"EMULATE_PYTHON_OK\", platform.machine(), 6 * 7)'; \
         test \"$(date -u +%s)\" -gt 1700000000 && echo EMULATE_TIME_OK; \
         test \"$(dd if=/dev/hwrng bs=32 count=1 2>/dev/null | wc -c)\" -eq 32 \
         && echo EMULATE_VIRTIO_RNG_OK",
    );
    guest.run_until(
        &Pattern::Line(b"EMULATE_PYTHON_OK riscv64 42"),
        "the riscv64 Python runtime",
    );
    guest.run_until(
        &Pattern::Line(b"EMULATE_TIME_OK"),
        "host-backed wall-clock time after switching root",
    );
    guest.run_until(
        &Pattern::Line(b"EMULATE_VIRTIO_RNG_OK"),
        "32 bytes from the VirtIO RNG after switching root",
    );

    guest.shell(
        "grep ' /dev/vda / ext4 ' /proc/mounts; \
         echo EMULATE_ROOTFS_PERSIST_OK > \"$HOME/emulate-persist\"; sync; reboot -f",
    );
    guest.run_until(&Pattern::Text(b"OpenSBI"), "the rebooted OpenSBI banner");
    guest.clear();
    guest.run_until(&ROOTFS_PROMPT, "the rebooted Alpine login shell");

    guest.shell("cat \"$HOME/emulate-persist\"");
    guest.run_until(&Pattern::Line(MARKER), "the persistent rootfs marker");

    let elapsed = guest.elapsed();
    drop(guest);
    let _ = std::fs::remove_dir_all(&scratch);

    common::pass(&format!(
        "linux-rootfs: shell in {:.1}s, /dev/vda write survived a reboot in {:.1}s",
        shell.as_secs_f64(),
        elapsed.as_secs_f64()
    ));
}
