//! `emulate-computer boot` — the interactive console around a machine built by
//! [`emulate_cli::run`]. Ctrl-A x exits.
//!
//! Raw mode, stdin forwarding, the Ctrl-A escape and signal handling live here;
//! the machine itself comes from the library.

use crate::term::{self, RawTerm};
use emulate_cli::run::{
    build_machine, restore_machine, BootImages, BootPaths, ENTROPY_REFILL_BYTES, SLICE,
};
use emulate_core::machine::{Machine, RunEvent};
use std::fs::File;
use std::io::{Read, Write};
use std::time::Instant;

enum Outcome {
    Shutdown(u32),
    Reset,
    Quit,
    Signal(libc::c_int),
    InputClosed,
    InputError(libc::c_int),
    HostError(String),
}

fn signal_exit_code(signal: libc::c_int) -> i32 {
    128 + signal
}

pub fn cmd_run(paths: &BootPaths) -> i32 {
    let images = match BootImages::load(paths) {
        Ok(i) => i,
        Err(msg) => {
            eprintln!("emulate-computer boot: {msg}");
            return 4;
        }
    };

    // Checked before the machine, so a missing relay is an error rather than a
    // guest whose network quietly never comes up.
    if let Err(msg) = images.check_relay() {
        eprintln!("emulate-computer boot: {msg}");
        return 4;
    }

    let mut term = RawTerm::enable();
    // Only the first machine is restored: a guest that resets wants a real
    // reboot from the images, not the same frozen instant again.
    let mut restore_next = images.snapshot.is_some();
    loop {
        let restored = restore_next;
        restore_next = false;
        let built = if restored {
            restore_machine(&images)
        } else {
            build_machine(&images)
        };
        let mut machine = match built {
            Ok(m) => m,
            Err(msg) => {
                term.restore();
                eprintln!("emulate-computer boot: {msg}");
                return 4;
            }
        };
        if restored {
            // The captured guest was sitting at an idle prompt with nothing
            // left in the UART, so the console is blank until the shell is
            // given a reason to redraw. A bare newline is that reason.
            machine.uart_input(b"\n");
        }
        let mut outcome = console_loop(&mut machine);
        vprint_decode_cache_stats(paths, &machine);
        if paths.disk.is_some() && machine.flush_disk().is_err() {
            term.restore();
            eprintln!("\nemulate-computer boot: cannot flush disk image");
            return 4;
        }
        if let Some(signal) = term::pending_signal() {
            outcome = Outcome::Signal(signal);
        }
        match outcome {
            Outcome::Shutdown(code) => {
                term.restore();
                eprintln!("\n[emulate] guest shutdown, code {code}");
                return code.min(255) as i32;
            }
            Outcome::Reset => {
                vprint(paths, "[emulate] guest requested reset, rebooting");
                continue;
            }
            Outcome::Quit => {
                term.restore();
                eprintln!("\n[emulate] quit");
                return 0;
            }
            Outcome::Signal(signal) => {
                term.restore();
                eprintln!("\n[emulate] interrupted by signal {signal}");
                return signal_exit_code(signal);
            }
            Outcome::InputClosed => {
                term.restore();
                eprintln!("\n[emulate] stdin closed");
                return 0;
            }
            Outcome::InputError(error) => {
                term.restore();
                eprintln!("\n[emulate] cannot read stdin: OS error {error}");
                return 4;
            }
            Outcome::HostError(error) => {
                term.restore();
                eprintln!("\n[emulate] {error}");
                return 4;
            }
        }
    }
}

/// Main interactive loop. Drives mtime from the wall clock
/// (TIMEBASE_FREQ = 10 MHz -> 10 ticks per microsecond).
fn console_loop(machine: &mut Machine) -> Outcome {
    let start = Instant::now();
    let mut entropy_source = match File::open("/dev/urandom") {
        Ok(source) => source,
        Err(error) => {
            return Outcome::HostError(format!("cannot open host entropy source: {error}"));
        }
    };
    let mut entropy = [0u8; ENTROPY_REFILL_BYTES];
    let mut ctrl_a = false;
    let mut inbuf = [0u8; 256];

    loop {
        if let Some(signal) = term::pending_signal() {
            return Outcome::Signal(signal);
        }

        let needed = machine.entropy_needed().min(entropy.len());
        if needed > 0 {
            if let Err(error) = entropy_source.read_exact(&mut entropy[..needed]) {
                return Outcome::HostError(format!("cannot read host entropy source: {error}"));
            }
            machine.add_entropy(&entropy[..needed]);
        }

        let ev = machine.run(SLICE);

        // Wall-clock time.
        machine.advance_mtime_from_host(start.elapsed().as_micros() as u64 * 10);

        // Drain guest console output.
        machine.control_output();
        let out = machine.uart_output();
        if !out.is_empty() {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&out);
            let _ = stdout.flush();
        }

        // Feed console input (non-blocking).
        loop {
            let n = match term::read_stdin(&mut inbuf) {
                term::StdinRead::Data(n) => n,
                term::StdinRead::WouldBlock => break,
                term::StdinRead::Eof => return Outcome::InputClosed,
                term::StdinRead::Error(error) => return Outcome::InputError(error),
            };
            let mut fwd = Vec::with_capacity(n + 1);
            for &b in &inbuf[..n] {
                if ctrl_a {
                    ctrl_a = false;
                    match b {
                        b'x' | b'X' => return Outcome::Quit,
                        0x01 => fwd.push(0x01), // Ctrl-A Ctrl-A -> literal Ctrl-A
                        _ => {
                            fwd.push(0x01);
                            fwd.push(b);
                        }
                    }
                } else if b == 0x01 {
                    ctrl_a = true;
                } else {
                    fwd.push(b);
                }
            }
            machine.uart_input(&fwd);
        }

        match ev {
            RunEvent::BudgetExhausted => {}
            RunEvent::Wfi => {
                // Sleep until the next timer deadline (capped at 10ms) while
                // watching stdin; input wakes the guest via the UART irq.
                machine.advance_mtime_from_host(start.elapsed().as_micros() as u64 * 10);
                let now = machine.mtime();
                let deadline = machine.next_timer_deadline();
                let mut wait_us = if deadline != u64::MAX && deadline > now {
                    ((deadline - now) / 10).min(10_000)
                } else {
                    10_000
                };
                // Shorten the sleep to the network backend's next deadline so
                // retransmits, DHCP timers and arriving host bytes are not
                // stuck behind an idle guest.
                if let Some(net_deadline_ms) = machine.net_deadline_ms() {
                    let net_wait_us = net_deadline_ms
                        .saturating_sub(machine.net_now_ms())
                        .saturating_mul(1_000);
                    wait_us = wait_us.min(net_wait_us);
                }
                term::poll_stdin((wait_us / 1000).max(1) as i32);
            }
            RunEvent::Shutdown(code) => return Outcome::Shutdown(code),
            RunEvent::Reset => return Outcome::Reset,
        }
    }
}

/// Verbose print that is safe in raw terminal mode (explicit CRLF).
fn vprint(paths: &BootPaths, msg: &str) {
    if paths.verbose {
        eprint!("{msg}\r\n");
    }
}

fn vprint_decode_cache_stats(paths: &BootPaths, machine: &Machine) {
    if !paths.verbose {
        return;
    }
    let stats = machine.cpu.decode_cache_stats();
    let total = stats.hits + stats.misses;
    let hit_rate = if total == 0 {
        0.0
    } else {
        stats.hits as f64 * 100.0 / total as f64
    };
    eprint!(
        "decode cache: {} hits, {} misses ({hit_rate:.2}% hit rate)\r\n",
        stats.hits, stats.misses
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_exit_codes_follow_shell_convention() {
        assert_eq!(signal_exit_code(libc::SIGINT), 130);
        assert_eq!(signal_exit_code(libc::SIGTERM), 143);
    }
}
