# Freestanding libc headers

Minimal headers used to build `riscv-tests/env/v` with a bare-metal compiler
that does not include a C library such as Newlib. These are project-owned build
support, not a vendored libc implementation.

- `ctype.h` implements the small ASCII character helpers the tests need.
- `string.h` declares routines supplied by `riscv-tests/env/v/string.c`.
- `stdio.h` supplies declarations; the test environment handles console I/O through HTIF.

[build-riscv-tests.sh](../../scripts/build-riscv-tests.sh) adds `include/` to the
compiler search path. Run `just prepare-isa` to build the corpus with these
headers, then `just test-isa` to execute it.
