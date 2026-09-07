//! The devicetree and the emulator must agree about the display.
//!
//! `guest/dts/build.sh` runs inside a Docker stage that has no Rust sources,
//! so it carries its own copy of the framebuffer geometry. This test is what
//! keeps that copy honest: it reads the shell variables straight out of the
//! script and compares them with `devices::framebuffer`. A number changed on
//! one side and not the other fails here rather than as a black canvas.

use emulate_core::devices::framebuffer::{
    FB_BASE, FB_BYTES, FB_FORMAT, FB_HEIGHT, FB_RESERVED_BYTES, FB_STRIDE, FB_WIDTH,
};

fn build_script() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../guest/dts/build.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|why| panic!("read {}: {why}", path.display()))
}

/// The right-hand side of a top-level `name=value` assignment.
fn assignment<'a>(script: &'a str, name: &str) -> &'a str {
    let prefix = format!("{name}=");
    script
        .lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("guest/dts/build.sh has no {name} assignment"))
        .trim()
}

fn number(script: &str, name: &str) -> u64 {
    let value = assignment(script, name);
    // Either a literal or a `$((...))` arithmetic expression of the two forms
    // this script uses.
    if let Some(expression) = value.strip_prefix("$((").and_then(|v| v.strip_suffix("))")) {
        return evaluate(expression, script);
    }
    value
        .parse()
        .unwrap_or_else(|_| panic!("{name}={value} is not a number"))
}

/// Evaluate the small subset of shell arithmetic build.sh uses: `+`, `-` and
/// `*` over decimal and `0x` literals and references to variables assigned
/// earlier in the script. No parentheses, no precedence surprises — `*` binds
/// tighter, which is all these expressions need.
fn evaluate(expression: &str, script: &str) -> u64 {
    let spaced = expression.replace('+', " + ").replace('-', " - ");
    let mut total: i128 = 0;
    let mut sign: i128 = 1;
    let mut product: i128 = 1;
    let mut expecting_factor = true;
    for token in spaced.split_whitespace() {
        match token {
            "+" | "-" => {
                total += sign * product;
                product = 1;
                sign = if token == "+" { 1 } else { -1 };
                expecting_factor = true;
            }
            "*" => expecting_factor = true,
            factor => {
                assert!(expecting_factor, "two values in a row: {expression:?}");
                expecting_factor = false;
                product *= i128::from(value_of(factor, script));
            }
        }
    }
    total += sign * product;
    u64::try_from(total).unwrap_or_else(|_| panic!("{expression:?} is not a u64"))
}

fn value_of(factor: &str, script: &str) -> u64 {
    if let Some(hex) = factor.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).expect("hex literal")
    } else if let Ok(value) = factor.parse::<u64>() {
        value
    } else {
        number(script, factor)
    }
}

#[test]
fn the_dts_build_script_carries_this_crates_display_geometry() {
    let script = build_script();

    assert_eq!(number(&script, "fb_width"), u64::from(FB_WIDTH));
    assert_eq!(number(&script, "fb_height"), u64::from(FB_HEIGHT));
    assert_eq!(number(&script, "fb_stride"), u64::from(FB_STRIDE));
    assert_eq!(number(&script, "fb_size"), FB_BYTES);
    assert_eq!(number(&script, "fb_reserved"), FB_RESERVED_BYTES);
    assert_eq!(number(&script, "fb_base"), FB_BASE);
    assert_eq!(assignment(&script, "fb_format"), FB_FORMAT);

    // The memory node must stop where the framebuffer starts, or the guest
    // will allocate over its own display.
    assert_eq!(
        0x8000_0000 + number(&script, "ram_size"),
        FB_BASE,
        "the shrunk memory node must end at the framebuffer"
    );
}

#[test]
fn the_dts_declares_the_framebuffer_and_both_input_slots() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../guest/dts/virt.dts");
    let dts = std::fs::read_to_string(path).expect("read guest/dts/virt.dts");

    assert!(dts.contains("compatible = \"simple-framebuffer\""));
    assert!(dts.contains("reg = <0x0 @FB_BASE@ 0x0 @FB_SIZE@>"));
    assert!(dts.contains("format = \"@FB_FORMAT@\""));
    // The two virtio-input slots, with the PLIC lines emulate-core drives.
    assert!(dts.contains("virtio_mmio@10004000"));
    assert!(dts.contains("virtio_mmio@10005000"));
}
