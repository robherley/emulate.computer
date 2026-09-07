use std::hint::black_box;
use std::time::{Duration, Instant};

use emulate_core::bus::DRAM_BASE;
use emulate_core::machine::{Machine, RunEvent};
use emulate_core::snapshot::{DiskOverlay, RestoreCheck, SnapshotIdentity};

const DEFAULT_SAMPLES: usize = 7;
const DEFAULT_INSTRUCTIONS: u64 = 5_000_000;
const SNAPSHOT_RAM_BYTES: usize = 8 * 1024 * 1024;

fn setting<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn measure(samples: usize, mut operation: impl FnMut()) -> Duration {
    operation();
    let timings = (0..samples)
        .map(|_| {
            let started = Instant::now();
            operation();
            started.elapsed()
        })
        .collect();
    median(timings)
}

fn identity() -> SnapshotIdentity {
    SnapshotIdentity {
        image_hash: emulate_core::snapshot::hash_images(&[b"benchmark"]),
        disk_id: String::from("benchmark-disk-v1"),
    }
}

fn cpu_machine() -> Machine {
    // addi x1, x1, 1; jal x0, -4. The two-instruction loop stays in one
    // decoded block and has no device or timer interactions.
    const LOOP: [u32; 2] = [0x0010_8093, 0xffdf_f06f];
    let bytes: Vec<u8> = LOOP.iter().flat_map(|word| word.to_le_bytes()).collect();
    let mut machine = Machine::new_system(1024 * 1024);
    machine.load_blob(DRAM_BASE, &bytes).unwrap();
    machine.set_boot(DRAM_BASE, 0, 0, 0);
    machine
}

fn main() {
    let samples = setting("EMULATE_BENCH_SAMPLES", DEFAULT_SAMPLES).max(1);
    let instructions = setting("EMULATE_BENCH_INSTRUCTIONS", DEFAULT_INSTRUCTIONS).max(1);

    let cpu = median(
        (0..samples)
            .map(|_| {
                let mut machine = cpu_machine();
                assert_eq!(machine.run(100_000), RunEvent::BudgetExhausted);
                let started = Instant::now();
                assert_eq!(machine.run(instructions), RunEvent::BudgetExhausted);
                black_box(machine.cpu.regs[1]);
                started.elapsed()
            })
            .collect(),
    );
    let ips = (instructions as u128 * 1_000_000_000 / cpu.as_nanos()) as u64;
    println!(
        "{{\"schema\":\"emulate-bench-v1\",\"benchmark\":\"cpu_tight_loop\",\"samples\":{samples},\"iterations\":{instructions},\"median_ns\":{},\"instructions_per_second\":{ips}}}",
        cpu.as_nanos()
    );

    let mut source = Machine::new_system(SNAPSHOT_RAM_BYTES);
    for offset in (0..SNAPSHOT_RAM_BYTES).step_by(4096) {
        source
            .write_phys(DRAM_BASE + offset as u64, (offset / 4096) as u64, 8)
            .unwrap();
    }
    let identity = identity();
    let overlay = DiskOverlay::default();
    let snapshot = source.snapshot(&identity, &overlay);
    let capture = measure(samples, || {
        black_box(source.snapshot(&identity, &overlay));
    });
    println!(
        "{{\"schema\":\"emulate-bench-v1\",\"benchmark\":\"snapshot_capture\",\"samples\":{samples},\"bytes\":{},\"median_ns\":{}}}",
        snapshot.len(),
        capture.as_nanos()
    );

    let restore_check = RestoreCheck {
        image_hash: Some(identity.image_hash),
        disk_id: Some(&identity.disk_id),
    };
    let restore = measure(samples, || {
        let mut target = Machine::new_system(SNAPSHOT_RAM_BYTES);
        target.restore(&snapshot, &restore_check).unwrap();
        black_box(target);
    });
    println!(
        "{{\"schema\":\"emulate-bench-v1\",\"benchmark\":\"snapshot_restore\",\"samples\":{samples},\"bytes\":{},\"median_ns\":{}}}",
        snapshot.len(),
        restore.as_nanos()
    );
}
