# Disk and snapshots

Linux accesses an ext4 root filesystem through VirtIO block. The emulator only
reads and writes disk bytes; it does not interpret ext4. The native CLI can keep
changes in a file, while each browser tab gets its own temporary session disk.

## Building the root filesystem

[guest/Dockerfile](../guest/Dockerfile) builds the RISC-V guest using pinned
Alpine packages and a checked-in, checksum-verified desktop binary bundle. The build exports OpenSBI,
the Linux kernel, a BusyBox initramfs with kernel modules, devicetrees, and
`rootfs.tar`. Files in [guest/overlays](../guest/overlays/) are installed at their
corresponding guest paths.

The Alpine root includes Bash and BusyBox, Python 3, QuickJS (`qjs`), curl with HTTPS certificate verification, networking
utilities, and `emuctl`. The desktop includes XLibre Xfbdev, Fluxbox, xterm, Dillo,
Adie (Notepad), PathFinder (Files), Xcalc, Xclock, Xeyes, Solitaire, Minesweeper,
Xtris, and DoomGeneric with Freedoom: Phase 1. Dillo opens
`/usr/share/emulate/index.html`, a local project homepage.
Notepad and Files appear under Utilities; games appear under Games.
Files has a built-in preview for JPEG, PNG, GIF, and BMP images. `/root/cats.jpg` is a resized, compressed JPEG easter egg;
the original HEIC and its metadata are not included. `/root/readme.txt` introduces
the guest, and `script.py` and `script.js` provide runnable Python and QuickJS examples.

```sh
just guest-rootfs    # build guest artifacts, format ext4, publish browser assets and snapshot
just guest-snapshot # recapture from existing guest artifacts
```

The prebuilt bundle contains XLibre, Fluxbox, FOX/Adie/PathFinder, and the
compiled games. `just guest-prebuilt` rebuilds it from source; changes to its
recipe, patches, or Alpine base invalidate the manifest. Freedoom data remains
a checksum-verified download. See [prebuilt binaries](../guest/prebuilt/README.md).

The build requires jq and Docker Buildx with `linux/riscv64` emulation; snapshot capture
also requires Rust. [build-rootfs.sh](../scripts/build-rootfs.sh)
formats a sparse 512 MiB `guest/out/alpine-rootfs.ext4` from the tar using a pinned
native-architecture container. Fixed ownership, timestamps, filesystem UUIDs,
and formatting inputs make the disk reproducible for unchanged input content.
`ROOTFS_VERIFY_REPRODUCIBLE=1 just guest-rootfs` formats twice and compares hashes.

The guest build writes `web/public/guest/rootfs.ext4.gz` and its uncompressed
SHA-256 digest in `rootfs.sha256`. The disk identity is `sha256:<hash>` of the
uncompressed ext4 image. Snapshot capture verifies that identity and embeds it.

The Bash asset preparation validates the disk's size and digest, and checks that the snapshot
matches the disk and boot images. It copies public assets to the generated Vite
public directory and names every guest artifact with the SHA-256 of its own
served bytes, for example `rootfs.<hash>.ext4.gz` and `snapshot.<hash>.bin.gz`.
The compressed-file hash is distinct from the uncompressed disk identity. A new
snapshot therefore gets a new URL even when its disk is unchanged.

Local builds capture snapshots, validate asset pairing, and prepare hashed assets.
`vc build --standalone` packages the frontend and relay; `vc deploy --prebuilt`
uploads `.vercel/output` without rebuilding the guest or frontend on Vercel.
See [local deployment](deployment.md).
Public files are copied from the checkout during the site build. Guest homepage edits still require rebuilding the guest.

A generated manifest in `web/src/generated/guest.json` is imported into the app
bundle. Boot uses those pinned URLs without fetching a mutable `rootfs.sha256`
pointer. For deployment, `just site-publish` uploads the compressed rootfs and snapshot to
public Vercel Blob URLs with one-year caching. The browser fetches them directly;
they are excluded from the static deployment. Local preparation keeps local URLs.
Vercel serves the smaller `/guest/` files and Vite's `/assets/` with immutable caching;
HTML revalidates. The raw build inputs are not copied into `web/dist`.
The browser compares the disk identity when restoring; it does not rehash the
streamed disk in browser memory.

See [guest/README.md](../guest/README.md) for build stages and boot variants.

## Block device

[VirtioBlock](../crates/emulate-core/src/devices/virtio_blk.rs) exposes one modern
MMIO request queue. Capacity is expressed in 512-byte sectors. Requests contain
a header, data descriptors, and a writable completion byte. Direct
scatter/gather chains support reads and writes; FLUSH reaches the host backend.
Out-of-range requests, unsupported request types, and backend errors complete
with an I/O error. Descriptor addresses and directions are checked before use.

Queue notifications execute requests synchronously. `BlockBackend` provides
capacity, sector reads/writes, and flush operations. Memory-backed disks serve
tests and volatile boots; native file and browser OPFS backends supply host I/O.
Reads into guest RAM update page generations, including when the destination
contains cached instructions. Queue completion updates the used ring and raises
an interrupt.

## Native disk access

The CLI's `boot --disk path.img` uses a writable file backend, so changes survive
process exit. `--disk-volatile` loads an in-memory disk and discards guest writes
on exit. Guest flush requests reach `sync_data()` on the file; the runner also
flushes during normal exit, shutdown, and reset handling.

`just run-linux` uses `guest/out/alpine-rootfs.ext4` directly. Preserve data you
need before rebuilding that image. Tests and snapshot capture use private copies
to avoid modifying the seed or sharing filesystem state between runs.

## Browser disk access

The emulator worker streams and decompresses the seed into a uniquely named
OPFS file using `FileSystemSyncAccessHandle`. The file has the full logical
capacity, but zero 4 KiB blocks are skipped while seeding. Disk contents stay
outside Wasm linear memory. The synchronous handle allows a VirtIO request to
finish within the worker's execution of its queue notification.

Tabs do not share disks. Restart retains the current tab's OPFS disk; Reset
removes it and seeds a fresh one. A normal page close requests cleanup. Since
close handlers are not guaranteed to run, later visits remove abandoned managed
files while respecting live-tab ownership and locks.

If OPFS is unavailable or cannot be initialized, the worker uses a sparse
in-memory disk. That fallback is reseeded when the machine host is replaced.
These disks are session scratch space, not durable storage across visits.

See the [worker](../web/src/worker.ts), [session lifecycle](../web/src/session/),
and [Wasm disk backend](../crates/emulate-wasm/src/disk.rs).

## Snapshot capture

[capture-snapshot.sh](../scripts/capture-snapshot.sh) produces a console snapshot at
`guest/out/snapshot.bin` and a graphical snapshot at
`guest/out/snapshot-desktop.bin`. The graphical snapshot is compressed and
published as `web/public/guest/snapshot.bin.gz`.

Capture boots an isolated copy of the seed with networking inactive. It waits
for the shell and control service; graphical capture also waits for Fluxbox to
manage Terminal and Dillo with the local homepage open. The terminal runs
`uname -a` at startup; Dillo sits below and to its right, leaving that output visible. It synchronizes the
filesystem, drops clean guest caches, and lets the guest settle before capture.
Changed 4 KiB disk blocks are saved as an overlay relative to the original seed.
The seed itself is never modified by capture.

The [snapshot container](../crates/emulate-core/src/snapshot.rs) stores CPU and
CSR state, device state, nonzero RAM pages, and the disk overlay. Device state
includes the display's committed front buffer. Versioned, length-delimited
sections allow the reader to reject unsupported or malformed state. The header
identifies the firmware/kernel/initramfs/devicetree hash, root disk identity,
RAM size, and timer frequency.

TLBs and decoded-instruction caches are rebuilt after restore. Host disk handles
and network sockets are not serialized. Snapshots contain runtime clocks and
entropy, so they are not byte-reproducible even when the seed disk is.

## Restore and restart

The host attaches a pristine seed disk, restores machine state and the disk
overlay, refreshes wall-clock time, and then attaches networking. The control
service reports its state again so automatic networking can proceed without
changing the serial console.

A snapshot's guest filesystem caches must match its disk. The browser therefore
only restores onto a disk that has not received guest writes since seeding.
Restart always boots from the firmware and kernel, preserving the current disk
and bypassing snapshots even when the disk is untouched.
Reset reseeds the disk and makes snapshot restore eligible again. A guest-requested
reboot always boots from the images.

Missing or incompatible snapshots fall back to normal boot. If restore fails
after starting, the browser frees the candidate machine, removes its partial
disk overlay, and recreates both from the boot images and pristine seed. If
that cleanup fails, startup stops rather than booting partially restored state. Native
`just run-linux-snapshot` uses a throwaway copy of the seed for the same reason.
Snapshot and storage regression tests are described in [testing.md](testing.md).
