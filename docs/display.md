# Display and input

The desktop runs XLibre Xfbdev and Fluxbox inside the guest. The browser presents
pixels from an emulated framebuffer and delivers input through VirtIO. A custom
rectangle device accelerates supported guest drawing operations on the host; it
is not a VirtIO GPU or a guest OpenGL interface.

## Desktop session

`emuctl` starts XLibre and Fluxbox during boot, then opens Terminal and Dillo's
local project homepage. The browser's Console and Desktop tabs select which view
is visible; switching tabs does not stop the graphical session.

Fluxbox uses native titlebar controls, a single workspace, and an applications
menu in its toolbar. Configuration and the bitmap theme live in
[the rootfs overlay](../guest/overlays/rootfs/etc/fluxbox/). A small Fluxbox patch
uses a 24-pixel XPM copy of the site favicon for the launcher and anchors its menu
above the button instead of at the pointer. Desktop right-click still opens the
normal context menu. XLibre's patches provide direct framebuffer access,
copy/fill and present operations, resizing, and absolute-pointer compatibility.
See [guest patches](../guest/patches/) and [disk.md](disk.md) for installed apps.

## Framebuffer

The guest boots a `simple-framebuffer` node backed by reserved DRAM. Linux's
`simpledrm` exposes `/dev/fb0`; XLibre maps the underlying scanout directly.

| Property | Value |
|---|---|
| Base | `0x8700_0000` |
| Boot geometry | 1024 × 768 |
| Runtime limits | 320–2048 × 200–2048 |
| Backing stride | 8192 bytes, two 4 KiB RAM pages per row |
| Backing size / reservation | 16 MiB |
| Pixel format | `x8r8g8b8` (B, G, R, X in memory) |
| Guest-allocatable RAM | 112 MiB of the 128 MiB machine |

[framebuffer.rs](../crates/emulate-core/src/devices/framebuffer.rs) defines the geometry.
[guest/dts/build.sh](../guest/dts/build.sh) carries the build's copy;
[dts_geometry.rs](../crates/emulate-core/tests/dts_geometry.rs) checks agreement. The reservation supports the largest mode without
moving guest memory when the browser resizes.

RAM writes increment per-page generations. The display combines the two page
generations in each row, compares them with the last captured values, and copies
only changed active rows. Conversion to RGBA happens on the host. Padding outside
the active width is excluded from Wasm-to-JavaScript transfers.

## Resizing

A `ResizeObserver` measures the desktop area beneath the browser navigation bar.
After 100 ms without another change, it sends a bounded `display-resize` message.
The worker records the requested dimensions in the framebuffer device, preserving
that request across machine restart. The shared viewport is measured even when Console is selected, so the desktop
can finish resizing before its tab opens. Zero-sized viewports are ignored.
Sizes use CSS pixels rather than multiplying guest rendering work by display DPR.

XLibre polls the requested dimensions every 100 ms. It updates the screen pixmap,
root clipping and physical dimensions, then sends RandR/root configuration
notifications. Fluxbox rebuilds its layout while retaining application windows.
Absolute input uses the current X screen dimensions, so pointer coordinates cover
the resized desktop.

The new dimensions become host-visible with the next committed frame. The worker
reallocates its staging image and canvas, then captures every active row. Main-thread
fallback frames carry their dimensions alongside the pixels, avoiding dependence
on a separate asynchronous resize notification.

## Rendering

XLibre commits after processing an event batch, immediately before sleeping.
The blitter copies changed scanout rows into a host-owned front buffer. Subsequent
guest draws stay hidden until the next commit, preventing the browser from showing
partially moved windows.

The worker captures committed rows and coalesces them in `DisplayBuffer` until
presentation. Canvas 2D renders on an OffscreenCanvas where supported; otherwise
packed dirty rows go to the main thread, with one frame in flight. Acknowledgements
bound fallback traffic. Context restoration forces a full capture. Rendering pauses
while the desktop tab or document is hidden; the guest keeps running.

The Metrics panel includes display frame rate, frame interval, and transfer-rate
sparklines alongside CPU and memory metrics. Benchmark instrumentation can also
measure input timing; this is not a complete input-to-photon measurement.

## Copy/fill and geometry registers

The [blitter](../crates/emulate-core/src/devices/framebuffer_blitter.rs) at
`0x1000_6000` accepts aligned little-endian 32-bit accesses.

| Offset | Register |
|---|---|
| `0x00` | Read-only version ID: `0x45464203` |
| `0x04` | Command: `1` copy, `2` fill, `3` present, `4` release; read status `0` success / `1` rejected |
| `0x08` | Source byte offset from framebuffer base |
| `0x0c` | Destination byte offset |
| `0x10` | Rectangle width in bytes, a positive multiple of four |
| `0x14` | Rectangle height in rows |
| `0x18` | Copy plane mask or fill AND mask |
| `0x1c` | Fill XOR value |
| `0x20` | Read-only host-requested width |
| `0x24` | Read-only host-requested height |
| `0x28` | Write pending geometry as `(height << 16) | width`; read committed geometry |

Copy/fill use the fixed backing stride and complete synchronously. Copy preserves
bits outside its plane mask and handles overlap like `memmove`; fill computes
`(destination & AND) ^ XOR`. Validation rejects unaligned or out-of-bounds rectangles
before writing. RAM page generations and code-cache invalidation still apply.

The XLibre patches install optional `fbBlt`/`fbSolid` hooks for supported scanout
operations. Pixmaps and other raster operations retain the software path. Device
or mapping failure falls back to `/dev/fb0`, which supports the boot resolution only.
The direct path bypasses simpledrm's deferred-I/O shadow copy.

## Input and snapshots

VirtIO keyboard and absolute tablet devices carry browser input. The browser maps
physical key codes to evdev, releases held input on blur, and coalesces pointer
moves while flushing coordinates before buttons. XLibre scales absolute positions
against the current screen width and height.

Snapshot format **6** includes the blitter's version-3 section: front pixels,
present counter, staged registers, committed geometry and pending geometry. The
browser size is requested again after restore. Changes to the framebuffer
reservation or device protocol require matching guest images and snapshots.

The native guest regressions exercise fbdev writes, copy/fill activation, desktop
startup, runtime resize, Fluxbox's work area, pointer corners and graphical restore.

See [testing.md](testing.md) for guest display regressions and browser checks.
