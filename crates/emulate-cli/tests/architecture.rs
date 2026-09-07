//! Architectural conformance through the same HTIF runner used by the CLI.
//! These tests require prepared corpora; see docs/testing.md.

use emulate_cli::test::{run_htif_elf, HtifOptions, HtifOutcome};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn corpus_dir(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn collect_elfs(directory: &Path, select: &impl Fn(&Path) -> bool, elfs: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap_or_else(|error| {
        panic!(
            "read {}: {error}; see docs/testing.md for setup",
            directory.display()
        )
    }) {
        let path = entry.expect("read corpus entry").path();
        if path.is_dir() {
            collect_elfs(&path, select, elfs);
        } else if path.is_file() && select(&path) {
            elfs.push(path);
        }
    }
}

fn run_corpus(
    directory: &Path,
    select: impl Fn(&Path) -> bool,
    expected: Option<usize>,
    pmp: bool,
) {
    let mut elfs = Vec::new();
    collect_elfs(directory, &select, &mut elfs);
    elfs.sort();
    assert!(
        !elfs.is_empty(),
        "no ELFs in {}; see docs/testing.md for setup",
        directory.display()
    );
    if let Some(expected) = expected {
        assert_eq!(
            elfs.len(),
            expected,
            "incomplete or stale pinned corpus in {}; rebuild it",
            directory.display()
        );
    }
    let options = HtifOptions {
        timeout: Duration::from_secs(60),
        ram_mb: 64,
        legacy_pmp_stubs: pmp,
        verbose: false,
        stream_console: false,
    };
    let mut failures = Vec::new();
    for elf in &elfs {
        let name = elf
            .strip_prefix(directory)
            .expect("corpus-relative path")
            .display();
        match run_htif_elf(elf, &options) {
            Ok(run) if run.outcome == HtifOutcome::Pass => {}
            Ok(run) => failures.push(format!(
                "{name}: {}\n{}",
                run.outcome.summary(),
                run.console_text()
            )),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    eprintln!(
        "{}: {} passed / {} selected",
        directory.display(),
        elfs.len() - failures.len(),
        elfs.len()
    );
    assert!(
        failures.is_empty(),
        "{} ELF failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn riscv_suite(suites: &[&str], expected: usize) {
    run_corpus(
        &corpus_dir("vendor/riscv-tests/isa"),
        |path| {
            let name = path.file_name().unwrap().to_string_lossy();
            !name.ends_with(".dump")
                && suites
                    .iter()
                    .any(|suite| name.starts_with(&format!("{suite}-p-")))
        },
        Some(expected),
        true,
    );
}

#[test]
#[ignore = "requires just prepare-isa; run just test-isa"]
fn riscv_base_and_privilege() {
    riscv_suite(
        &[
            "rv64ui", "rv64um", "rv64ua", "rv64uf", "rv64ud", "rv64uc", "rv64mi", "rv64si",
        ],
        134,
    );
}

#[test]
#[ignore = "requires just prepare-isa; run just test-isa"]
fn riscv_bitmanip() {
    riscv_suite(&["rv64uzbb", "rv64uzbc", "rv64uzbkb"], 31);
}

fn act4(profile: &str) {
    // Generation may select extensions. Report the selected corpus size rather
    // than claiming full profile coverage from a partial build.
    run_corpus(
        &corpus_dir(&format!("target/act4/{profile}/elfs")),
        |path| path.extension().is_some_and(|ext| ext == "elf"),
        None,
        false,
    );
}

#[test]
#[ignore = "requires just act4-generate; run just test-conformance"]
fn act4_unprivileged() {
    act4("emulate-rv64gc");
}

#[test]
#[ignore = "requires just act4-generate --profile priv; run just test-conformance"]
fn act4_privileged() {
    act4("emulate-rv64gc-priv");
}
