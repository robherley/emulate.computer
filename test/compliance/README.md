# Architectural compliance profiles

Project-owned ACT4 device-under-test (DUT) definitions: supported instructions,
platform configuration, and any linker scripts or model macros needed to
generate tests for this emulator.

- [emulate-rv64gc](act4/emulate-rv64gc/): unprivileged profile using pinned upstream platform definitions.
- [emulate-rv64gc-priv](act4/emulate-rv64gc-priv/): privileged profile with project-owned platform definitions.

```sh
just act4-generate
just act4-generate --profile priv
just test-conformance
```

Upstream sources live in `vendor/riscv-arch-test`; generated ELFs go under
`target/act4/`. The executable harness lives in
[the CLI architecture tests](../../crates/emulate-cli/tests/architecture.rs).
`test-conformance` also requires the ISA corpus built by `just prepare-isa`.

See [testing.md](../../docs/testing.md) for prerequisites, focused runs, and how
generated coverage limits compliance claims.
