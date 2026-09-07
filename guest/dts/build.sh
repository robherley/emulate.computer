#!/bin/sh
# Build initramfs, disk-root console, and disk-root desktop DTBs.

set -eu

dts="$1"
initrd="$2"
out_dir="$3"

# Must match emulate-cli's `boot --ram-mb` default and the initrd address
# emulate-core loads the ramdisk at.
ram_mb=128
initrd_start=$((0x84600000))

# Display geometry. THE SOURCE OF TRUTH IS
# crates/emulate-core/src/devices/framebuffer.rs — this is a copy, because this
# script runs inside a Docker stage that has no Rust sources. Any drift fails
# crates/emulate-core/tests/dts_geometry.rs.
#
# The framebuffer is carved out of the top of DRAM rather than given an MMIO
# window of its own, so the memory node below is shrunk by the reservation and
# the emulator's bus needs no display device at all (docs/display.md).
fb_width=1024
fb_height=768
fb_stride=8192
fb_format=x8r8g8b8
fb_reserved=$((16 * 1024 * 1024))
fb_size=$((fb_stride * 2048))
fb_base=$((0x80000000 + ram_mb * 1024 * 1024 - fb_reserved))

# emulate.net=none is a fixed-width placeholder: the native host can flip it
# in place to emulate.net=dhcp (same length, no FDT surgery) for full boots.
# Browser guests request their lease automatically through the control port.
console="console=ttyS0 earlycon=uart8250,mmio,0x10000000"

ram_size=$((ram_mb * 1024 * 1024 - fb_reserved))
initrd_end=$((initrd_start + $(stat -c%s "$initrd")))

mkdir -p "$out_dir"

compile() {
    bootargs="$1"
    output="$2"
    sed -e "s|@BOOTARGS@|${bootargs}|" \
        -e "s|@RAM_SIZE@|$(printf '0x%x' "$ram_size")|" \
        -e "s|@FB_UNIT@|$(printf '%x' "$fb_base")|" \
        -e "s|@FB_BASE@|$(printf '0x%x' "$fb_base")|" \
        -e "s|@FB_SIZE@|$(printf '0x%x' "$fb_size")|" \
        -e "s|@FB_WIDTH@|${fb_width}|" \
        -e "s|@FB_HEIGHT@|${fb_height}|" \
        -e "s|@FB_STRIDE@|${fb_stride}|" \
        -e "s|@FB_FORMAT@|${fb_format}|" \
        -e "s|@INITRD_START@|$(printf '0x%x' "$initrd_start")|" \
        -e "s|@INITRD_END@|$(printf '0x%x' "$initrd_end")|" \
        "$dts" > /tmp/virt.dts
    dtc -I dts -O dtb -o "$output" /tmp/virt.dts
    printf 'wrote %s (ram=0x%x initrd=0x%x..0x%x fb=0x%x %sx%s)\n' \
        "$output" "$ram_size" "$initrd_start" "$initrd_end" \
        "$fb_base" "$fb_width" "$fb_height"
}

compile "$console root=/dev/ram0 emulate.net=none" "$out_dir/virt.dtb"
compile "$console emulate.root=/dev/vda emulate.net=none" "$out_dir/virt-rootfs.dtb"

compile "$console emulate.root=/dev/vda emulate.net=none emulate.desktop=auto" "$out_dir/virt-desktop.dtb"
