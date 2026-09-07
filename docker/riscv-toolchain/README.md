# Bare-metal RISC-V toolchain

Docker fallback for compiling `riscv-tests` and xv6 when no host RISC-V GCC is
installed. The image runs on the host architecture and cross-compiles guest
ELFs. Native GCC also builds xv6's filesystem creation utility.

```sh
just toolchain-image
FORCE_TOOLCHAIN_CONTAINER=1 just prepare-isa
FORCE_TOOLCHAIN_CONTAINER=1 just prepare-xv6
```

[Toolchain resolution](../../scripts/riscv-toolchain.sh) checks `RISCV_TOOLPREFIX`,
then host `riscv64-elf-` / `riscv64-unknown-elf-` compilers, then this image.
`TOOLCHAIN_IMAGE` overrides the default `emulate-riscv-toolchain:trixie` tag.

The Dockerfile pins the Debian base, cross GCC, and binutils, and checks that
the compiler can link a freestanding RISC-V ELF. Alpine guest images use the
separate [guest build](../../guest/README.md).
