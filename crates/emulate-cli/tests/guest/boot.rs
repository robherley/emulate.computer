//! OpenSBI -> Linux -> initramfs, to a BusyBox shell: it comes up, it is
//! riscv64, the clock is host-backed, and the VirtIO RNG hands out entropy.

use crate::support as common;

use common::{Guest, Pattern, CURSOR_QUERY, MOTD, SHELL_PROMPT};

const MARKER: &[u8] = b"EMULATE_LINUX_SMOKE_OK";

#[test]
#[ignore = "requires prepared guest artifacts; run just test-guest"]
fn linux_reaches_a_busybox_shell_with_host_time_and_entropy() {
    common::require_guest();

    let mut guest = Guest::boot_linux(None, None);
    guest.run_until(&MOTD, "the motd banner");
    guest.run_until(&SHELL_PROMPT, "the BusyBox ash prompt");
    guest.run_until(&Pattern::Text(CURSOR_QUERY), "the terminal cursor query");
    let shell = guest.elapsed();

    guest.shell("echo EMULATE_LINUX_SMOKE_OK; uname -m");
    guest.run_until(&Pattern::Line(MARKER), "the Linux shell marker");
    guest.run_until(&Pattern::Line(b"riscv64"), "the Linux architecture");

    guest.shell(
        "test \"$(date -u +%s)\" -gt 1700000000 && echo EMULATE_TIME_OK; \
         test \"$(dd if=/dev/hwrng bs=32 count=1 2>/dev/null | wc -c)\" -eq 32 \
         && echo EMULATE_VIRTIO_RNG_OK",
    );
    guest.run_until(
        &Pattern::Line(b"EMULATE_TIME_OK"),
        "the host-backed guest wall clock",
    );
    guest.run_until(
        &Pattern::Line(b"EMULATE_VIRTIO_RNG_OK"),
        "the VirtIO entropy device",
    );

    common::pass(&format!(
        "linux: shell in {:.1}s, marker, host time and VirtIO RNG in {:.1}s",
        shell.as_secs_f64(),
        guest.elapsed().as_secs_f64()
    ));
}
