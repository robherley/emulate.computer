# Bare-metal RISC-V cross toolchain for building riscv-tests and xv6, used when
# no host `riscv64-unknown-elf-` prefix is installed. Build it with
# `just toolchain-image`.
#
# Built for the host architecture: Debian ships gcc-riscv64-unknown-elf for both
# amd64 and arm64, so this needs no emulation. Base pinned by index digest.
FROM debian:trixie-slim@sha256:d7e12182ce18b85b93007c1dedf31f2d29e01ccf3182cc4017c709b6259bc132

# Compiler and binutils are version-pinned so a mirror update is a loud failure
# rather than a silent change in the ELFs under test. The host `gcc`/`libc6-dev`
# are for xv6's Makefile, which compiles mkfs/mkfs.c natively for fs.img.
RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        gcc-riscv64-unknown-elf=14.2.0+19 \
        binutils-riscv64-unknown-elf=2.44-3+7+b1 \
        make \
        gcc \
        libc6-dev; \
    rm -rf /var/lib/apt/lists/*

# Smoke the cross toolchain at build time, linking freestanding exactly the way
# riscv-tests and xv6 do, so a broken or wrong-arch package fails here instead
# of halfway through a corpus build.
RUN set -eux; \
    printf '.section .text\n.globl _start\n_start:\n  li a0, 0\n  j _start\n' > /tmp/hello.S; \
    riscv64-unknown-elf-gcc -march=rv64g -mabi=lp64 -static -mcmodel=medany \
        -nostdlib -nostartfiles -ffreestanding \
        -Wl,-Ttext=0x80000000 -o /tmp/hello.elf /tmp/hello.S; \
    riscv64-unknown-elf-objdump -f /tmp/hello.elf | grep -q 'elf64-littleriscv'; \
    riscv64-unknown-elf-readelf -h /tmp/hello.elf | grep -q 'RISC-V'; \
    rm -f /tmp/hello.S /tmp/hello.elf

WORKDIR /work
