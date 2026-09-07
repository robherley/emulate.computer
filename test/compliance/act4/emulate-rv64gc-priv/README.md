# emulate-rv64gc-priv ACT4 configuration

This project-owned ACT4 profile describes the privileged architecture that
emulate.computer currently implements. It is derived from the pinned upstream
`config/sail/sail-RVA20S64` UDB and Sail pair, with the advertised surface
reduced to RV64 IMAFDC, Zicsr, Zifencei, Zicntr, Sm/S/U, Sstc, Sv39, Svbare,
and Svadu. The UDB file also lists Zmmul, Zaamo, Zalrsc, Zca, and Zcd because the
current UDB schema requires those constituent extensions for M, A, and C; they
do not broaden the claimed instruction set.

The profile declares zero PMP entries, and its linker and Sail memory region
both limit RAM to 64 MiB at `0x8000_0000`. The DUT macros use the platform UART
at `0x1000_0000`, CLINT at `0x0200_0000`, ACT4 simple interrupt generator at
`0x1001_0000`, and the self-checking `tohost` protocol. The interrupt generator
supplies the same SSIP/SEIP/MEIP stimulus to Sail and the DUT, so the Sm/S/U
interrupt, delegation, priority, vectored-trap, and WFI suites are valid
compliance claims. `InterruptsSstc` uses CLINT `mtime` and the `stimecmp` CSR.

The interactive CLI and browser use a separate system-machine constructor with
storage-only PMP compatibility registers because xv6 assumes at least one PMP
entry during its earliest M-mode startup. ACT4 continues to use the truthful
zero-PMP constructor described by this profile.

Generate selected suites with `just act4-generate --profile priv --extensions LIST` and
run all generated ELFs with `just test-act4 --profile priv`. Generated ELFs live under
`target/act4/emulate-rv64gc-priv/elfs`.
