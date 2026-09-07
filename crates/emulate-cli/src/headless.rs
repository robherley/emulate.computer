//! A guest driven without a terminal: run in slices, drain the UART, answer
//! the shell's cursor query, and skip idle time instead of sleeping through it.
//!
//! Shared by snapshot capture and integration tests; callers choose matching
//! and reboot policy.

use emulate_core::machine::{Machine, RunEvent};
use std::fs::File;
use std::io::Read;
use std::time::{Duration, Instant};

use crate::run::{ENTROPY_REFILL_BYTES, SLICE};

/// The terminal cursor-position query the guest's shell emits at startup and
/// then waits for an answer to.
pub const CURSOR_QUERY: &[u8] = b"\x1b[6n";
/// What we answer it with: row 1, column 1.
const CURSOR_REPLY: &[u8] = b"\x1b[1;1R";

/// Console tail included in a timeout error.
const TAIL_BYTES: usize = 4096;
const TICKS_PER_MS: u64 = 10_000;
const MAX_NET_SKEW_TICKS: u64 = 250 * TICKS_PER_MS;

/// Why [`Headless::run_until`] gave up. A guest that resets or powers off is
/// not necessarily a failure — `boot --snapshot`'s reboot path and the
/// integration tests both want to act on it — so it is a variant rather than
/// a message.
#[derive(Debug)]
pub enum Stopped {
    /// The pattern never arrived; carries the console tail.
    Timeout(String),
    Shutdown(u32),
    Reset,
    /// The host side failed (entropy source, and so on).
    Host(String),
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stopped::Timeout(detail) => write!(f, "{detail}"),
            Stopped::Shutdown(code) => {
                write!(f, "guest shut down (code {code}) before it was ready")
            }
            Stopped::Reset => write!(f, "guest reset itself before it was ready"),
            Stopped::Host(detail) => write!(f, "{detail}"),
        }
    }
}

/// A machine plus the console bookkeeping needed to talk to its shell.
pub struct Headless {
    machine: Machine,
    /// Where this machine's clock started. `advance_mtime_from_host` takes
    /// deltas against the caller's own zero, not a wall-clock epoch.
    machine_start: Instant,
    clock_origin: u64,
    cursor_query_bytes: usize,
    entropy_source: File,
    entropy: [u8; ENTROPY_REFILL_BYTES],
    output: Vec<u8>,
    control: Vec<u8>,
    /// Guest output echoed to stderr as it arrives, for a progress trail.
    echo: bool,
}

impl Headless {
    pub fn new(machine: Machine, echo: bool) -> Result<Self, String> {
        Ok(Headless {
            clock_origin: machine.mtime(),
            cursor_query_bytes: 0,
            machine,
            machine_start: Instant::now(),
            entropy_source: File::open("/dev/urandom")
                .map_err(|error| format!("cannot open host entropy source: {error}"))?,
            entropy: [0; ENTROPY_REFILL_BYTES],
            output: Vec::new(),
            control: Vec::new(),
            echo,
        })
    }

    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    pub fn machine_mut(&mut self) -> &mut Machine {
        &mut self.machine
    }

    pub fn into_machine(self) -> Machine {
        self.machine
    }

    /// Everything the guest has printed since the last [`Headless::clear`].
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    pub fn clear(&mut self) {
        self.output.clear();
    }

    /// Type into the guest console.
    pub fn send(&mut self, text: &str) {
        self.machine.uart_input(text.as_bytes());
    }

    /// Clear the capture buffer, then run one shell command line.
    pub fn shell(&mut self, command: &str) {
        self.clear();
        self.send(command);
        self.send("\n");
    }

    /// Replace a rebooted machine, retaining the captured console output.
    pub fn replace_machine(&mut self, machine: Machine) {
        self.clock_origin = machine.mtime();
        self.machine = machine;
        self.machine_start = Instant::now();
        self.cursor_query_bytes = 0;
        self.control.clear();
    }

    pub fn run_until_control(&mut self, needle: &[u8], budget: Duration) -> Result<(), Stopped> {
        let deadline = Instant::now() + budget;
        while !contains(&self.control, needle) {
            if Instant::now() >= deadline {
                return Err(Stopped::Timeout(format!(
                    "waiting for control {:?}: {}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&self.control)
                )));
            }
            self.step()?;
        }
        self.control.clear();
        Ok(())
    }

    pub fn run_until(&mut self, needle: &[u8], budget: Duration) -> Result<(), Stopped> {
        self.run_until_matching(
            |output| contains(output, needle),
            Instant::now() + budget,
            &format!("{:?}", String::from_utf8_lossy(needle)),
        )
    }

    /// Wait against an absolute deadline so callers can preserve their budget across reboots.
    pub fn run_until_matching(
        &mut self,
        matches: impl Fn(&[u8]) -> bool,
        deadline: Instant,
        description: &str,
    ) -> Result<(), Stopped> {
        loop {
            if matches(&self.output) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let tail = self.output.len().saturating_sub(TAIL_BYTES);
                return Err(Stopped::Timeout(format!(
                    "timed out waiting for {description}\n--- console tail ---\n{}\n--- end ---",
                    String::from_utf8_lossy(&self.output[tail..])
                )));
            }
            self.step()?;
        }
    }

    /// Run for a wall-clock interval, returning early if the guest stops or resets.
    pub fn settle(&mut self, budget: Duration) -> Result<(), Stopped> {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            self.step()?;
        }
        Ok(())
    }

    fn step(&mut self) -> Result<(), Stopped> {
        let needed = self.machine.entropy_needed().min(self.entropy.len());
        if needed > 0 {
            self.entropy_source
                .read_exact(&mut self.entropy[..needed])
                .map_err(|error| {
                    Stopped::Host(format!("cannot read host entropy source: {error}"))
                })?;
            self.machine.add_entropy(&self.entropy[..needed]);
        }
        let event = self.machine.run(SLICE);
        self.machine
            .advance_mtime_from_host(self.machine_start.elapsed().as_micros() as u64 * 10);
        self.control.extend(self.machine.control_output());
        if self.control.len() > TAIL_BYTES {
            self.control.drain(..self.control.len() - TAIL_BYTES);
        }
        self.drain_console();
        match event {
            RunEvent::BudgetExhausted => Ok(()),
            RunEvent::Wfi => {
                self.skip_idle();
                Ok(())
            }
            RunEvent::Shutdown(code) => Err(Stopped::Shutdown(code)),
            RunEvent::Reset => Err(Stopped::Reset),
        }
    }

    fn drain_console(&mut self) {
        let out = self.machine.uart_output();
        if out.is_empty() {
            return;
        }
        if self.echo {
            use std::io::Write;
            let mut stderr = std::io::stderr();
            let _ = stderr.write_all(&out);
            let _ = stderr.flush();
        }
        for &byte in &out {
            if byte == CURSOR_QUERY[self.cursor_query_bytes] {
                self.cursor_query_bytes += 1;
                if self.cursor_query_bytes == CURSOR_QUERY.len() {
                    self.machine.uart_input(CURSOR_REPLY);
                    self.cursor_query_bytes = 0;
                }
            } else {
                self.cursor_query_bytes = usize::from(byte == CURSOR_QUERY[0]);
            }
        }
        self.output.extend_from_slice(&out);
    }

    // Network timers must leave time for the host relay to answer.
    fn skip_idle(&mut self) {
        let now = self.machine.mtime();
        let mut target = self.machine.next_timer_deadline();
        if let Some(deadline_ms) = self.machine.net_deadline_ms() {
            let host = self
                .clock_origin
                .saturating_add(self.machine_start.elapsed().as_micros() as u64 * 10);
            target = target
                .min(deadline_ms.saturating_mul(TICKS_PER_MS))
                .min(host.saturating_add(MAX_NET_SKEW_TICKS));
        }
        if target != u64::MAX && target > now {
            self.machine.set_mtime(target);
        } else {
            std::thread::sleep(Duration::from_micros(500));
        }
    }
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emulate_core::devices::net::NetBackend;

    const UART: u64 = 0x1000_0000;

    fn driver() -> Headless {
        Headless::new(Machine::new(64 * 1024), false).unwrap()
    }

    fn output(guest: &mut Headless, bytes: &[u8]) {
        for &byte in bytes {
            guest
                .machine_mut()
                .write_phys(UART, u64::from(byte), 1)
                .unwrap();
        }
        guest.drain_console();
    }

    fn input(guest: &mut Headless) -> Vec<u8> {
        let mut bytes = Vec::new();
        while guest.machine_mut().read_phys(UART + 5, 1).unwrap() & 1 != 0 {
            bytes.push(guest.machine_mut().read_phys(UART, 1).unwrap() as u8);
        }
        bytes
    }

    #[test]
    fn split_cursor_queries_are_answered_once_even_when_capture_is_cleared() {
        let mut guest = driver();
        output(&mut guest, b"boot\x1b[6");
        assert!(input(&mut guest).is_empty());
        guest.clear();
        output(&mut guest, b"n\x1b[6n");
        assert_eq!(input(&mut guest), CURSOR_REPLY.repeat(2));
        assert_eq!(guest.output(), b"n\x1b[6n");
    }

    #[test]
    fn reboot_keeps_console_history_but_discards_partial_cursor_queries() {
        let mut guest = driver();
        output(&mut guest, b"rebooting\x1b[6");
        guest.replace_machine(Machine::new(64 * 1024));
        output(&mut guest, b"n");
        assert!(input(&mut guest).is_empty());
        assert_eq!(guest.output(), b"rebooting\x1b[6n");
        assert!(matches!(
            guest.run_until_matching(|_| false, Instant::now(), "new boot"),
            Err(Stopped::Timeout(message)) if message.contains("new boot") && message.contains("rebooting")
        ));
    }

    struct Deadline(u64);
    impl NetBackend for Deadline {
        fn transmit(&mut self, _: &[u8]) {}
        fn receive(&mut self) -> Option<Vec<u8>> {
            None
        }
        fn poll(&mut self, _: u64) {}
        fn next_deadline_ms(&self) -> Option<u64> {
            Some(self.0)
        }
    }

    #[test]
    fn idle_skipping_respects_network_time_after_snapshot_restore() {
        let mut machine = Machine::new(64 * 1024);
        let origin = 9_000 * TICKS_PER_MS;
        machine.set_mtime(origin);
        machine
            .write_phys(0x0200_4000, origin + 60_000 * TICKS_PER_MS, 8)
            .unwrap();
        machine.set_net_backend(Box::new(Deadline(9_500)));
        let mut guest = Headless::new(machine, false).unwrap();
        guest.machine_start = Instant::now() + Duration::from_secs(60);
        guest.skip_idle();
        assert_eq!(guest.machine().mtime(), origin + MAX_NET_SKEW_TICKS);
    }

    #[test]
    fn settling_propagates_guest_reset_and_shutdown() {
        for (value, reset) in [(0x7777, true), (0x5555, false)] {
            let mut guest = driver();
            guest.machine_mut().write_phys(0x10_0000, value, 4).unwrap();
            let stopped = guest.settle(Duration::from_secs(1));
            if reset {
                assert!(matches!(stopped, Err(Stopped::Reset)));
            } else {
                assert!(matches!(stopped, Err(Stopped::Shutdown(0))));
            }
        }
    }

    #[test]
    fn contains_finds_a_needle_at_either_end() {
        assert!(super::contains(
            b"root@emulate.computer:~# ",
            b"emulate.computer:~# "
        ));
        assert!(super::contains(b"abc", b"abc"));
        assert!(!super::contains(b"ab", b"abc"));
        assert!(super::contains(b"", b""));
    }
}
