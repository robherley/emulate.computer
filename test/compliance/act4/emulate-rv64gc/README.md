# emulate-rv64gc ACT4 configuration

This ACT4 DUT profile for emulate.computer selects the
unprivileged tests supported by the official `spike-RVI20U64` configuration
from the pinned `vendor/riscv-arch-test` checkout, but gives the generated DUT
ELFs their own `emulate-rv64gc` identity.

The upstream profile currently matches emulate's platform contract:

- executable RAM begins at `0x8000_0000`;
- the NS16550-compatible UART begins at `0x1000_0000`;
- the CLINT begins at `0x0200_0000`; and
- self-checking tests terminate through the `tohost` symbol (`1` for pass,
  another odd value for failure).

Keeping the UDB, Sail configuration, linker script, and model macros anchored
to that exact upstream profile avoids maintaining divergent copies. When the
emulator's implementation or ACT4 profile changes, add project-owned overrides
here and update `test_config.yaml` to reference them.

Use `just act4-generate` to build self-checking ELFs in
`target/act4/emulate-rv64gc/elfs`, then `just test-act4` to run them. Both
recipes accept optional arguments; run either script with `--help` for details.
