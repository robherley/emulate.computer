//! riscv-tests runner: the HTIF tohost/fromhost protocol.
//!
//! [`run_htif_elf`] returns an [`HtifOutcome`] plus the console the test
//! produced; [`cmd_test`] wraps it in printed output and an exit code.

use crate::elfload;
use emulate_core::machine::{Machine, RunEvent};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

/// Instructions per run slice. Must stay below ACT4's bounded polling loops so
/// the host-driven `time` counter can advance while a test is executing.
const SLICE: u64 = 1_000;

#[derive(Debug, PartialEq, Eq)]
enum HtifRequest {
    Pass,
    Fail(u64),
    ConsolePutchar(u8),
    Unexpected,
}

fn decode_htif_request(value: u64) -> HtifRequest {
    let device = value >> 56;
    let command = (value >> 48) & 0xff;
    match (device, command) {
        (1, 1) => HtifRequest::ConsolePutchar(value as u8),
        (0, 0) if value == 1 => HtifRequest::Pass,
        (0, 0) if value & 1 == 1 => HtifRequest::Fail(value >> 1),
        _ => HtifRequest::Unexpected,
    }
}

/// How a test ELF ended.
#[derive(Debug, PartialEq, Eq)]
pub enum HtifOutcome {
    /// `tohost` reported success, or the test finisher shut down with 0.
    Pass,
    /// `tohost` reported failure; the payload is the failing test number.
    Fail(u64),
    /// The test finisher shut down with a nonzero code.
    FailCode(u32),
    /// The wall-clock budget ran out.
    Timeout,
    /// A `tohost` value this harness does not understand.
    Unexpected(u64),
    /// The guest asked for a reset, which no test ELF should do.
    Reset,
}

impl HtifOutcome {
    /// The process exit code the CLI reports for this outcome.
    pub fn exit_code(&self) -> i32 {
        match self {
            HtifOutcome::Pass => 0,
            HtifOutcome::Fail(_) | HtifOutcome::FailCode(_) => 1,
            HtifOutcome::Unexpected(_) | HtifOutcome::Reset => 2,
            HtifOutcome::Timeout => 3,
        }
    }

    /// The one-line summary the CLI prints.
    pub fn summary(&self) -> String {
        match self {
            HtifOutcome::Pass => String::from("PASS"),
            HtifOutcome::Fail(test) => format!("FAIL (test {test})"),
            HtifOutcome::FailCode(code) => format!("FAIL (test finisher, code {code})"),
            HtifOutcome::Timeout => String::from("TIMEOUT"),
            HtifOutcome::Unexpected(value) => format!("unexpected tohost value {value:#018x}"),
            HtifOutcome::Reset => String::from("unexpected reset request"),
        }
    }
}

/// Knobs for [`run_htif_elf`].
pub struct HtifOptions {
    /// Wall-clock budget for the whole run.
    pub timeout: Duration,
    pub ram_mb: u64,
    /// Print load and progress telemetry on stderr.
    pub verbose: bool,
    /// Expose storage-only PMP CSRs for the legacy riscv-tests suite.
    pub legacy_pmp_stubs: bool,
    /// Also write the guest console to stdout as it arrives.
    pub stream_console: bool,
}

impl Default for HtifOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            ram_mb: 64,
            verbose: false,
            legacy_pmp_stubs: false,
            stream_console: false,
        }
    }
}

/// The result of one test ELF: how it ended, and everything it printed.
pub struct HtifRun {
    pub outcome: HtifOutcome,
    /// UART plus HTIF `console_putchar` output, in arrival order.
    pub console: Vec<u8>,
}

impl HtifRun {
    /// The console as text, with trailing whitespace trimmed.
    pub fn console_text(&self) -> String {
        String::from_utf8_lossy(&self.console).trim_end().to_owned()
    }
}

pub fn cmd_test(
    elf_path: &Path,
    timeout_mins: u64,
    ram_mb: u64,
    verbose: bool,
    legacy_pmp_stubs: bool,
) -> i32 {
    let options = HtifOptions {
        timeout: Duration::from_secs(timeout_mins * 60),
        ram_mb,
        verbose,
        legacy_pmp_stubs,
        stream_console: true,
    };
    match run_htif_elf(elf_path, &options) {
        Ok(run) => {
            println!("{}", run.outcome.summary());
            run.outcome.exit_code()
        }
        Err(msg) => {
            eprintln!("emulate-computer test: {msg}");
            4
        }
    }
}

/// Load `elf_path` and run it to its HTIF verdict.
///
/// `Err` means the ELF could not be loaded or the host could not be talked to;
/// a test that simply fails is `Ok` with a failing [`HtifOutcome`].
pub fn run_htif_elf(elf_path: &Path, options: &HtifOptions) -> Result<HtifRun, String> {
    let data =
        std::fs::read(elf_path).map_err(|e| format!("cannot read {}: {e}", elf_path.display()))?;
    let elf = elfload::load_elf(&data)?;
    let tohost = elfload::symbol_addr(&data, "tohost")
        .ok_or_else(|| format!("{}: no `tohost` symbol in ELF", elf_path.display()))?;
    let fromhost = elfload::symbol_addr(&data, "fromhost");

    let mut machine = Machine::new((options.ram_mb as usize) << 20);
    machine.enable_instruction_time();
    if options.legacy_pmp_stubs {
        machine.cpu.csr.enable_pmp_stubs();
    }
    for (paddr, memory_size, bytes) in elf.loadable() {
        elfload::load_segment(&mut machine, paddr, memory_size, bytes).map_err(|_| {
            format!(
                "segment at {paddr:#x} ({} bytes) does not fit in RAM",
                memory_size
            )
        })?;
    }
    machine.set_boot(elf.entry, 0, 0, 0);

    if options.verbose {
        eprintln!(
            "loaded {} segment(s), entry {:#x}, tohost {:#x}, ram {} MB",
            elf.segments.len(),
            elf.entry,
            tohost,
            options.ram_mb
        );
    }

    let deadline = Instant::now() + options.timeout;
    let mut total: u64 = 0;
    let mut next_progress: u64 = 10_000_000;
    let mut console: Vec<u8> = Vec::new();
    let emit = |console: &mut Vec<u8>, bytes: &[u8]| {
        console.extend_from_slice(bytes);
        if options.stream_console {
            let mut out = std::io::stdout();
            let _ = out.write_all(bytes);
            let _ = out.flush();
        }
    };

    loop {
        if Instant::now() >= deadline {
            return Ok(HtifRun {
                outcome: HtifOutcome::Timeout,
                console,
            });
        }

        let ev = machine.run(SLICE);
        total += SLICE; // approximate; slices may end early

        // Architectural tests print their self-check result on the UART
        // before terminating via tohost.
        let uart = machine.uart_output();
        if !uart.is_empty() {
            emit(&mut console, &uart);
        }

        if options.verbose && total >= next_progress {
            eprintln!(
                "[{:>6}M instrs] pc={:#x}",
                total / 1_000_000,
                machine.cpu.pc
            );
            next_progress += 10_000_000;
        }

        // HTIF: check tohost after each slice.
        let val = machine
            .read_phys(tohost, 8)
            .map_err(|_| format!("cannot read tohost at {tohost:#x}"))?;
        if val != 0 {
            match decode_htif_request(val) {
                HtifRequest::Pass => {
                    print_decode_cache_stats(&machine, options.verbose);
                    return Ok(HtifRun {
                        outcome: HtifOutcome::Pass,
                        console,
                    });
                }
                HtifRequest::Fail(test) => {
                    return Ok(HtifRun {
                        outcome: HtifOutcome::Fail(test),
                        console,
                    });
                }
                HtifRequest::ConsolePutchar(ch) => {
                    emit(&mut console, &[ch]);
                    machine
                        .write_phys(tohost, 0, 8)
                        .map_err(|_| "tohost write failed".to_string())?;
                    if let Some(fh) = fromhost {
                        machine
                            .write_phys(fh, 1, 8)
                            .map_err(|_| "fromhost write failed".to_string())?;
                    }
                }
                HtifRequest::Unexpected => {
                    return Ok(HtifRun {
                        outcome: HtifOutcome::Unexpected(val),
                        console,
                    });
                }
            }
        }

        match ev {
            RunEvent::BudgetExhausted => {}
            RunEvent::Wfi => {
                // Jump time to the next timer event; if there is none, this
                // is likely a hang — keep polling until the wall clock
                // timeout fires, but don't spin hot.
                let d = machine.next_timer_deadline();
                if d != u64::MAX {
                    if d > machine.mtime() {
                        machine.set_mtime(d);
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            RunEvent::Shutdown(code) => {
                let outcome = if code == 0 {
                    print_decode_cache_stats(&machine, options.verbose);
                    HtifOutcome::Pass
                } else {
                    HtifOutcome::FailCode(code)
                };
                return Ok(HtifRun { outcome, console });
            }
            RunEvent::Reset => {
                return Ok(HtifRun {
                    outcome: HtifOutcome::Reset,
                    console,
                });
            }
        }
    }
}

fn print_decode_cache_stats(machine: &Machine, verbose: bool) {
    if !verbose {
        return;
    }
    let stats = machine.cpu.decode_cache_stats();
    let total = stats.hits + stats.misses;
    let hit_rate = if total == 0 {
        0.0
    } else {
        stats.hits as f64 * 100.0 / total as f64
    };
    eprintln!(
        "decode cache: {} hits, {} misses ({hit_rate:.2}% hit rate)",
        stats.hits, stats.misses
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn htif_console_putchar_with_odd_payload_is_not_a_test_failure() {
        let request = decode_htif_request(0x0101_0000_0000_0061);

        assert_eq!(request, HtifRequest::ConsolePutchar(b'a'));
    }

    #[test]
    fn htif_exit_failure_is_limited_to_device_zero_command_zero() {
        let request = decode_htif_request(0x0200_0000_0000_0003);

        assert_eq!(request, HtifRequest::Unexpected);
    }
}
