//! Snapshot round-trips: a restored machine must be indistinguishable from
//! the one it was captured from, and a container that does not belong to this
//! host must be refused rather than resumed into.
//!
//! Everything here goes through the public surface — `write_phys`/`read_phys`
//! for MMIO, `uart_input`/`uart_output` for the console — so the tests exercise
//! the same paths a guest does. Full-state comparison is done by re-capturing
//! both machines: a snapshot *is* the serialization of the whole machine, so
//! two byte-identical containers mean two identical machines.

use emulate_core::bus::DRAM_BASE;
use emulate_core::machine::{Machine, RunEvent};
use emulate_core::snapshot::{DiskOverlay, RestoreCheck, SnapshotIdentity};

const RAM_BYTES: usize = 1 << 20;

/// CLINT (see `emulate_core::bus`, mirrored here because those constants are
/// crate-internal).
const CLINT_BASE: u64 = 0x200_0000;
const CLINT_MTIMECMP: u64 = CLINT_BASE + 0x4000;
const PLIC_BASE: u64 = 0xC00_0000;
const UART_BASE: u64 = 0x1000_0000;
const VIRTIO_BLK_BASE: u64 = 0x1000_1000;
/// ACT4 simple interrupt generator command register.
const SIMPLE_IRQ_COMMAND: u64 = 0x1001_0000 + 4;

fn identity() -> SnapshotIdentity {
    SnapshotIdentity {
        image_hash: emulate_core::snapshot::hash_images(&[b"fw", b"kernel"]),
        disk_id: String::from("test-disk-v1"),
    }
}

fn check(identity: &SnapshotIdentity) -> RestoreCheck<'_> {
    RestoreCheck {
        image_hash: Some(identity.image_hash),
        disk_id: Some(&identity.disk_id),
    }
}

fn capture(machine: &Machine) -> Vec<u8> {
    machine.snapshot(&identity(), &DiskOverlay::default())
}

/// A loop that walks a pointer through memory storing an incrementing counter,
/// so both registers and a growing set of RAM pages advance every iteration.
///
/// ```text
/// 0x0: addi x2, x0, 0x100      x2 = store pointer
/// 0x4: addi x1, x1, 1          counter++
/// 0x8: sd   x1, 0(x2)
/// 0xc: addi x2, x2, 8
/// 0x10: jal x0, -12            back to 0x4
/// ```
const PROGRAM: [u32; 5] = [
    0x1000_0113,
    0x0010_8093,
    0x0011_3023,
    0x0081_0113,
    0xff5f_f06f,
];

fn program_bytes() -> Vec<u8> {
    PROGRAM.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn booted_machine() -> Machine {
    let mut machine = Machine::new_system(RAM_BYTES);
    machine.load_blob(DRAM_BASE, &program_bytes()).unwrap();
    machine.set_boot(DRAM_BASE, 0, 0, 0);
    machine
}

#[test]
fn restored_machine_runs_identically_to_one_that_was_never_interrupted() {
    // Reference: one machine that runs straight through.
    let mut reference = booted_machine();
    assert_eq!(reference.run(5_000), RunEvent::BudgetExhausted);
    let midpoint = capture(&reference);
    assert_eq!(reference.run(3_000), RunEvent::BudgetExhausted);

    // Subject: restore the midpoint into a fresh machine and run the rest.
    let mut restored = Machine::new_system(RAM_BYTES);
    restored
        .restore(&midpoint, &check(&identity()))
        .expect("restore the midpoint");
    assert_eq!(restored.run(3_000), RunEvent::BudgetExhausted);

    assert_eq!(
        capture(&restored),
        capture(&reference),
        "a restored machine diverged from the uninterrupted run"
    );
}

#[test]
fn snapshot_and_restore_round_trip_without_running_anything() {
    let mut machine = booted_machine();
    machine.run(1_234);
    let bytes = capture(&machine);

    let mut restored = Machine::new_system(RAM_BYTES);
    restored
        .restore(&bytes, &check(&identity()))
        .expect("restore");

    assert_eq!(capture(&restored), bytes);
}

#[test]
fn uart_fifo_contents_and_registers_survive_a_round_trip() {
    let mut machine = Machine::new_system(RAM_BYTES);
    machine.uart_input(b"hello");
    // IER = RX-data-available, LCR = 8N1, SCR is a scratch byte the guest owns.
    machine.write_phys(UART_BASE + 1, 0x01, 1).unwrap();
    machine.write_phys(UART_BASE + 3, 0x03, 1).unwrap();
    machine.write_phys(UART_BASE + 7, 0x5a, 1).unwrap();
    let bytes = capture(&machine);

    let mut restored = Machine::new_system(RAM_BYTES);
    restored
        .restore(&bytes, &check(&identity()))
        .expect("restore");

    assert_eq!(restored.read_phys(UART_BASE + 1, 1), Ok(0x01));
    assert_eq!(restored.read_phys(UART_BASE + 3, 1), Ok(0x03));
    assert_eq!(restored.read_phys(UART_BASE + 7, 1), Ok(0x5a));
    // The queued input is still queued, in order.
    for expected in b"hello" {
        assert_eq!(restored.read_phys(UART_BASE, 1), Ok(u64::from(*expected)));
    }
}

#[test]
fn pending_uart_output_survives_a_round_trip() {
    let mut machine = Machine::new_system(RAM_BYTES);
    machine.write_phys(UART_BASE, u64::from(b'o'), 1).unwrap();
    machine.write_phys(UART_BASE, u64::from(b'k'), 1).unwrap();
    let bytes = capture(&machine);

    let mut restored = Machine::new_system(RAM_BYTES);
    restored
        .restore(&bytes, &check(&identity()))
        .expect("restore");

    assert_eq!(restored.uart_output(), b"ok");
}

#[test]
fn plic_configuration_and_pending_state_survive_a_round_trip() {
    const UART_IRQ: u64 = 10;
    let mut machine = Machine::new_system(RAM_BYTES);
    // Priority, S-mode context enable, threshold — the guest's own setup.
    machine.write_phys(PLIC_BASE + UART_IRQ * 4, 7, 4).unwrap();
    machine
        .write_phys(PLIC_BASE + 0x2000 + 0x80, 1 << UART_IRQ, 4)
        .unwrap();
    machine
        .write_phys(PLIC_BASE + 0x20_0000 + 0x1000, 1, 4)
        .unwrap();
    // Raise the line: a byte waiting with RX interrupts enabled.
    machine.write_phys(UART_BASE + 1, 0x01, 1).unwrap();
    machine.uart_input(b"x");
    machine.run(1);
    let bytes = capture(&machine);

    let mut restored = Machine::new_system(RAM_BYTES);
    restored
        .restore(&bytes, &check(&identity()))
        .expect("restore");

    assert_eq!(restored.read_phys(PLIC_BASE + UART_IRQ * 4, 4), Ok(7));
    assert_eq!(
        restored.read_phys(PLIC_BASE + 0x2000 + 0x80, 4),
        Ok(1 << UART_IRQ)
    );
    // Pending, and claimable by the S-mode context, exactly as before.
    assert_eq!(restored.read_phys(PLIC_BASE + 0x1000, 4), Ok(1 << UART_IRQ));
    assert_eq!(
        restored.read_phys(PLIC_BASE + 0x20_0000 + 0x1004, 4),
        Ok(UART_IRQ)
    );
}

#[test]
fn clint_mtime_and_mtimecmp_survive_a_round_trip() {
    let mut machine = Machine::new_system(RAM_BYTES);
    machine.set_mtime(123_456);
    machine.write_phys(CLINT_MTIMECMP, 999_000, 8).unwrap();
    machine.write_phys(CLINT_BASE, 1, 4).unwrap(); // msip
    let bytes = capture(&machine);

    let mut restored = Machine::new_system(RAM_BYTES);
    restored
        .restore(&bytes, &check(&identity()))
        .expect("restore");

    assert_eq!(restored.mtime(), 123_456);
    assert_eq!(restored.next_timer_deadline(), 999_000);
    assert_eq!(restored.read_phys(CLINT_BASE, 4), Ok(1));
}

#[test]
fn virtio_queue_addresses_and_status_survive_a_round_trip() {
    let mut machine = Machine::new_system(RAM_BYTES);
    let program = |machine: &mut Machine, offset: u64, value: u64| {
        machine
            .write_phys(VIRTIO_BLK_BASE + offset, value, 4)
            .unwrap();
    };
    program(&mut machine, 0x030, 0); // QueueSel = 0
    program(&mut machine, 0x038, 128); // QueueNum
    program(&mut machine, 0x080, 0x1_2000); // QueueDescLow
    program(&mut machine, 0x084, 0); // QueueDescHigh
    program(&mut machine, 0x090, 0x1_3000); // QueueDriverLow
    program(&mut machine, 0x0a0, 0x1_4000); // QueueDeviceLow
    program(&mut machine, 0x044, 1); // QueueReady
    program(&mut machine, 0x070, 0x0f); // Status: DRIVER_OK
    let bytes = capture(&machine);

    let mut restored = Machine::new_system(RAM_BYTES);
    restored
        .restore(&bytes, &check(&identity()))
        .expect("restore");

    assert_eq!(restored.read_phys(VIRTIO_BLK_BASE + 0x044, 4), Ok(1));
    assert_eq!(restored.read_phys(VIRTIO_BLK_BASE + 0x070, 4), Ok(0x0f));
    // QueueNum is write-only in the MMIO transport, so the queue's size and
    // ring addresses are checked by re-capturing: identical containers mean
    // identical queues.
    assert_eq!(capture(&restored), bytes);
}

#[test]
fn simple_interrupt_generator_levels_survive_a_round_trip() {
    let mut machine = Machine::new_system(RAM_BYTES);
    // Bit 31 = set; MEIP.
    machine
        .write_phys(SIMPLE_IRQ_COMMAND, (1u64 << 31) | (1 << 11), 4)
        .unwrap();
    let bytes = capture(&machine);

    let mut restored = Machine::new_system(RAM_BYTES);
    restored
        .restore(&bytes, &check(&identity()))
        .expect("restore");

    assert_eq!(restored.read_phys(SIMPLE_IRQ_COMMAND, 4), Ok(1 << 11));
}

#[test]
fn sparse_ram_is_carried_page_exactly() {
    let mut machine = Machine::new_system(RAM_BYTES);
    // One byte in the first page, one in the last: everything between stays
    // zero and must not be carried.
    machine.write_phys(DRAM_BASE, 0xab, 1).unwrap();
    machine
        .write_phys(DRAM_BASE + RAM_BYTES as u64 - 8, 0xcdcd, 2)
        .unwrap();
    let bytes = capture(&machine);
    let header = emulate_core::snapshot::read_header(&bytes).expect("header");
    assert_eq!(header.ram_pages, 2);
    assert_eq!(header.ram_bytes, RAM_BYTES as u64);
    // Two pages plus the fixed sections, nowhere near the 1 MiB of RAM.
    assert!(
        bytes.len() < 64 * 1024,
        "container is {} bytes",
        bytes.len()
    );

    let mut restored = Machine::new_system(RAM_BYTES);
    restored
        .restore(&bytes, &check(&identity()))
        .expect("restore");
    assert_eq!(restored.read_phys(DRAM_BASE, 1), Ok(0xab));
    assert_eq!(restored.read_phys(DRAM_BASE + 8, 1), Ok(0));
    assert_eq!(
        restored.read_phys(DRAM_BASE + RAM_BYTES as u64 - 8, 2),
        Ok(0xcdcd)
    );
}

#[test]
fn restore_clears_ram_the_target_machine_had_already_written() {
    let mut source = Machine::new_system(RAM_BYTES);
    source.write_phys(DRAM_BASE, 0x11, 1).unwrap();
    let bytes = capture(&source);

    let mut target = Machine::new_system(RAM_BYTES);
    target.write_phys(DRAM_BASE + 0x8000, 0x99, 1).unwrap();
    target
        .restore(&bytes, &check(&identity()))
        .expect("restore");

    assert_eq!(target.read_phys(DRAM_BASE, 1), Ok(0x11));
    assert_eq!(
        target.read_phys(DRAM_BASE + 0x8000, 1),
        Ok(0),
        "the target's own pages must not survive a restore"
    );
}

// ---------------------------------------------------------------------------
// Stale-snapshot rejection
// ---------------------------------------------------------------------------

#[test]
fn a_snapshot_from_different_boot_images_is_refused() {
    let bytes = capture(&booted_machine());
    let mut machine = Machine::new_system(RAM_BYTES);
    let other = emulate_core::snapshot::hash_images(&[b"a different kernel"]);
    let error = machine
        .restore(
            &bytes,
            &RestoreCheck {
                image_hash: Some(other),
                disk_id: None,
            },
        )
        .expect_err("stale images");
    assert!(error.contains("different boot images"), "{error}");
}

#[test]
fn a_snapshot_from_a_different_root_disk_is_refused() {
    let bytes = capture(&booted_machine());
    let mut machine = Machine::new_system(RAM_BYTES);
    let error = machine
        .restore(
            &bytes,
            &RestoreCheck {
                image_hash: None,
                disk_id: Some("some-other-disk-v9"),
            },
        )
        .expect_err("stale disk");
    assert!(error.contains("root disk"), "{error}");
}

#[test]
fn a_snapshot_taken_at_a_different_ram_size_is_refused() {
    let bytes = capture(&booted_machine());
    let mut machine = Machine::new_system(RAM_BYTES * 2);
    let error = machine
        .restore(&bytes, &check(&identity()))
        .expect_err("wrong RAM size");
    assert!(error.contains("bytes of RAM"), "{error}");
}

#[test]
fn a_container_with_the_wrong_magic_is_refused() {
    let mut bytes = capture(&booted_machine());
    bytes[0] = b'X';
    let mut machine = Machine::new_system(RAM_BYTES);
    let error = machine
        .restore(&bytes, &check(&identity()))
        .expect_err("bad magic");
    assert!(error.contains("not an EMUSNAP container"), "{error}");
}

#[test]
fn a_container_from_a_future_format_version_is_refused() {
    let mut bytes = capture(&booted_machine());
    bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
    let mut machine = Machine::new_system(RAM_BYTES);
    let error = machine
        .restore(&bytes, &check(&identity()))
        .expect_err("future format");
    assert!(error.contains("format version 99"), "{error}");
    assert!(
        emulate_core::snapshot::read_header(&bytes).is_err(),
        "read_header must reject it too"
    );
}

#[test]
fn a_truncated_container_is_refused_rather_than_partially_applied() {
    let bytes = capture(&booted_machine());
    let truncated = &bytes[..bytes.len() / 2];
    let mut machine = Machine::new_system(RAM_BYTES);
    let error = machine
        .restore(truncated, &check(&identity()))
        .expect_err("truncated container");
    assert!(error.contains("truncated"), "{error}");
}

#[test]
fn header_reports_the_identity_it_was_captured_with() {
    let bytes = capture(&booted_machine());
    let header = emulate_core::snapshot::read_header(&bytes).expect("header");
    assert_eq!(header.identity, identity());
    assert_eq!(header.timebase_hz, emulate_core::machine::TIMEBASE_FREQ);
    assert_eq!(header.disk_blocks, 0);
}
