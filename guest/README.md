# Guest images

This directory builds the RISC-V software executed by the emulator: OpenSBI,
Linux, a BusyBox initramfs, and an Alpine root filesystem with an optional desktop.

## Layout

| Path | Purpose |
| --- | --- |
| `Dockerfile` | Pinned dependencies and build stages; `guest/` is the Docker context |
| `prebuilt/` | Desktop binaries, source build recipe, and input/checksum manifest |
| `dts/` | Machine description and compilation of three boot variants |
| `overlays/initramfs/` | Initial userspace files, arranged by their installed paths |
| `overlays/rootfs/` | Alpine configuration, commands, and desktop assets, arranged by their installed paths |
| `patches/xserver/` | XLibre framebuffer, blitter, resize, and absolute-pointer support |
| `patches/fluxbox/` | Favicon taskbar launcher with its menu anchored above the button |
| `patches/doom/` | DoomGeneric X11 window close, sizing, and keyboard handling |
| `patches/games/` | Ace's 64-bit window hints and Minesweeper default board |
| `out/` | Ignored build artifacts and caches; excluded from the Docker context |

For example, `overlays/rootfs/usr/local/bin/emuctl` becomes `/usr/local/bin/emuctl`, and
`overlays/rootfs/etc/fluxbox/` becomes `/etc/fluxbox/`. The Dockerfile
sets installation permissions explicitly. Keep executable scripts and plain
configuration files distinct when adding a `COPY` or `install` command.

The two overlays have separate console configurations. The initramfs starts
an `ash` shell or switches to `/dev/vda`; the Alpine root automatically logs
in as root with Bash. Their banners and profiles belong to different boot paths.

## Guest commands

`emuctl` is installed on Alpine's PATH:

```sh
emuctl desktop          # start the graphical session
emuctl desktop status
emuctl desktop stop
emuctl desktop restart
emuctl net connect      # request a DHCP lease on eth0
emuctl net disconnect   # clear addresses/routes and bring eth0 down
emuctl --help
```

The graphical session starts at boot. The browser automatically
requests networking through the guest control port; a configured host relay is required.

## Build

Run from the repository root:

```sh
just guest          # firmware, kernel, initramfs, DTBs, rootfs.tar; install browser boot images
just guest-rootfs   # also format ext4, install its browser seed, and capture a snapshot
just guest-snapshot # capture a snapshot from existing artifacts
just guest-prebuilt # rebuild the checked-in desktop binaries after source/patch changes
```

The image build requires jq and Docker Buildx with `linux/riscv64` emulation.
Snapshot capture also requires the Rust toolchain. To build one stage:

```sh
docker buildx build --platform linux/riscv64 --target initramfs guest
docker buildx build --platform linux/riscv64 --file guest/prebuilt/Dockerfile --target xserver guest
docker buildx build --platform linux/riscv64 --target rootfs guest
```

Normal image builds unpack the checked-in desktop bundle instead of compiling
XLibre, Fluxbox, FOX, and the games. Both the host script and Docker build verify
checksums; recipe, patch, or Alpine-base changes require `just guest-prebuilt`.
Runtime library checks still reject missing dependencies and Mesa/LLVM.
See [prebuilt/README.md](prebuilt/README.md) for regeneration and provenance.

Runtime package versions live in `Dockerfile`; compiled component versions and
build settings live in `prebuilt/Dockerfile`. Alpine bases are pinned by digest,
runtime packages by version, and source tarballs by SHA-256. The prebuilt recipe
keeps the XLibre and Fluxbox patches and builds without Mesa or LLVM.
Dillo opens `file:///usr/share/emulate/index.html`.

Doom uses a pinned DoomGeneric X11 build and Freedoom 0.13.0 Phase 1 data.
Start it from Games → Doom or run `doom` in a terminal. Arrow keys move and turn,
F/Ctrl fires, Space/E opens doors, Shift runs, and Esc opens the menu. Sound is
disabled. Settings and saves live under `~/.local/share/doom` on the session disk.
The bundle includes the engine license; the `freedoom` stage downloads and
verifies the game data and installs its license and credits.

## Artifacts

| Output in `out/` | Producer |
| --- | --- |
| `fw.bin` | `firmware` stage: OpenSBI dynamic firmware |
| `Image` | `kernel` stage: decompressed Alpine Linux kernel |
| `initramfs.cpio.gz` | `initramfs` stage: BusyBox, kernel modules, and initramfs overlay |
| `virt.dtb`, `virt-rootfs.dtb`, `virt-desktop.dtb` | `dtb` stage: `dts/build.sh` and `dts/virt.dts` |
| `rootfs.tar` | `rootfs-tar` stage: Alpine packages and rootfs overlay |
| `alpine-rootfs.ext4` | `scripts/build-alpine-rootfs.sh`: reproducible filesystem from the tar |
| `snapshot.bin`, `snapshot-desktop.bin` | `scripts/build-snapshot.sh`: post-boot state captured on a scratch disk copy |

`virt.dtb` boots into the initramfs. `virt-rootfs.dtb` adds
`emulate.root=/dev/vda` to switch onto the disk. `virt-desktop.dtb` also starts the graphical desktop.
All boot with networking disabled; the browser requests a lease automatically.
Native sessions can request one with `emuctl net connect`.

The scripts install browser assets under `web/public/guest/`. The rootfs build
hashes the uncompressed ext4 image and publishes `rootfs.sha256`. The browser
and snapshot metadata use `sha256:<hash>` as the disk identity, so content changes
need no manual version bump. Snapshot capture rejects a disk that differs from
the published seed; run `just guest-rootfs` to rebuild the seed and snapshots.

`ROOTFS_VERIFY_REPRODUCIBLE=1 just guest-rootfs` formats the ext4 image twice
and compares hashes. `ROOTFS_REUSE_IMAGE=1` reuses an existing image;
`ROOTFS_SKIP_SNAPSHOT=1` skips snapshot capture. Fixed timestamps, ownership,
and filesystem identifiers make ext4 reproducible for an unchanged input tar;
snapshots contain runtime clock state and are not byte-reproducible.

Build artifacts can be regenerated, but native `just run-linux` writes directly
to `out/alpine-rootfs.ext4`. Preserve any guest data you need before deleting or
rebuilding it. Rebuilding everything requires `just guest-rootfs`.

See [display](../docs/display.md), [disk and snapshots](../docs/disk.md),
and [testing](../docs/testing.md) for details.
