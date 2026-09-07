//! Upstream xv6-riscv as a bare M-mode kernel on a volatile copy of its
//! VirtIO filesystem image. `just test-guest` checks shell compatibility;
//! `just test-stress` runs the full usertests workload.

use crate::support as common;

use common::{Guest, Pattern, XV6_PROMPT};
use std::time::Duration;

const MARKER: &[u8] = b"EMULATE_XV6_SMOKE_OK";

#[test]
#[ignore = "requires prepared guest artifacts; run just test-guest"]
fn xv6_reaches_shell() {
    common::require_xv6();

    let mut guest = Guest::boot_xv6();
    guest.run_until(&XV6_PROMPT, "the xv6 shell prompt");
    let shell = guest.elapsed();

    guest.shell("echo EMULATE_XV6_SMOKE_OK");
    guest.run_until(&Pattern::Line(MARKER), "the xv6 shell marker");

    common::pass(&format!(
        "xv6: shell in {:.1}s, marker in {:.1}s",
        shell.as_secs_f64(),
        guest.elapsed().as_secs_f64()
    ));
}

#[test]
#[ignore = "long-running kernel stress test; run just test-stress"]
fn xv6_usertests() {
    common::require_xv6();

    let mut guest = Guest::boot_xv6();
    guest.set_timeout(Duration::from_secs(3600));
    guest.run_until(&XV6_PROMPT, "the xv6 shell prompt");

    guest.shell("usertests -q");
    guest.run_until(
        &Pattern::Line(b"ALL TESTS PASSED"),
        "xv6 usertests completion",
    );

    common::pass(&format!(
        "xv6: usertests -q returned ALL TESTS PASSED in {:.1}s",
        guest.elapsed().as_secs_f64()
    ));
}
