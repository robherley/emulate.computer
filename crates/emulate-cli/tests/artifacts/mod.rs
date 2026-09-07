//! Tests consume artifacts; build scripts prepare them explicitly.

use std::path::{Path, PathBuf};

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn require(paths: &[&str], prepare: &str) {
    let root = repo_root();
    let missing: Vec<_> = paths
        .iter()
        .filter(|path| !root.join(path).is_file())
        .collect();
    assert!(
        missing.is_empty(),
        "missing test artifacts: {missing:?}; run `{prepare}`"
    );
}

pub fn require_guest() {
    require(
        &[
            "guest/out/fw.bin",
            "guest/out/Image",
            "guest/out/initramfs.cpio.gz",
            "guest/out/virt.dtb",
            "guest/out/virt-rootfs.dtb",
        ],
        "just guest",
    );
}

pub fn require_rootfs() {
    require_guest();
    require(&["guest/out/alpine-rootfs.ext4"], "just guest-rootfs");
}

pub fn require_xv6() {
    require(
        &["vendor/xv6-riscv/kernel/kernel", "vendor/xv6-riscv/fs.img"],
        "just prepare-xv6",
    );
}
