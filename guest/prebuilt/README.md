# Prebuilt desktop

`desktop.tar.gz` contains the stripped RISC-V Linux/musl executables, custom
shared libraries, support files, and licenses for XLibre Xfbdev, Fluxbox,
FOX/Adie/PathFinder, Solitaire, Minesweeper, Xtris, and DoomGeneric. Tar preserves
executable permissions and shared-library symlinks. Runtime dependencies still
come from Alpine. Freedoom game data is downloaded separately by the guest build.

Normal builds use this bundle. Rebuild it explicitly after changing the source
versions, build flags, or patches:

```sh
just guest-prebuilt
bash scripts/prebuilt.sh verify
just guest-rootfs
```

Commit `desktop.tar.gz`, `manifest.json`, and `SHA256SUMS` together with the source
changes. `Dockerfile` holds the source URLs/checksums and complete build settings;
its context is `guest/`. `manifest.json` records the target, Alpine base, source
pins, recipe/patch hashes, and archive size/hash. `SHA256SUMS` lets Docker verify
the same inputs, archive, and manifest without installing a host scripting runtime.
Both verification paths reject changed or missing inputs; new patches also
invalidate the bundle. Changes to desktop configuration or other rootfs overlays
do not require recompilation.

The host script uses Bash, jq, and shasum. The rebuild uses Docker Buildx with `linux/riscv64` emulation and its usual layer
cache. Compiler dependencies are resolved from the pinned Alpine release's
repositories. The manifest records the recipe, not a guarantee of byte-identical
compiler output across future repository changes. Packaging normalizes ownership,
entry order, timestamps, and gzip metadata.

The main guest Dockerfile checks the Alpine base agrees with this recipe and
checks runtime linking, including the ban on Mesa/LLVM dependencies. After a
rebuild, boot the desktop and verify the affected applications before shipping.
