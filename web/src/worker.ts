/// <reference lib="webworker" />
import { createRestorable } from "./session/restore.ts";
import { DisplayBuffer } from "./display-buffer";
import { DisplayMetrics } from "./display-metrics";
import { NetworkPort, GuestTransport } from "./network/transport";
import { Session } from "./session/session";
import { BrowserDisk, type DiskAttachmentStatus } from "./session/disk";
import { countBytes, gunzipIfNeeded } from "./session/streams";

import init, { WasmMachine } from "./wasm/emulate_wasm.js";
import {
  DEFAULT_NET,
  DISPLAY_FRAME_MS,
  FB_HEIGHT,
  FB_WIDTH,
  STDOUT_WINDOW_BYTES,
  TICKS_PER_MS,
  type DisplayInput,
  type BootMessage,
  type DownloadProgressMessage,
  type EmulatorStats,
  type NetConfig,
  type ErrorMessage,
  type MainToWorker,
  type WorkerToMain,
} from "./protocol";

const ctx = self as unknown as Worker;
let network: NetworkPort | null = null;
let pendingPointer: Extract<DisplayInput, { kind: "pointer" }> | null = null;
let pendingPointerSentAt: number | undefined;

function post(msg: WorkerToMain, transfer?: Transferable[]): void {
  if (transfer) {
    ctx.postMessage(msg, transfer);
  } else {
    ctx.postMessage(msg);
  }
}

// run() status codes (see emulate-wasm/src/lib.rs)
const STATUS_BUDGET_EXHAUSTED = 0;
const STATUS_WFI = 1;
const STATUS_SHUTDOWN = 2;
const STATUS_RESET = 3;

const SLICE_BUDGET = 50_000;
const MAX_WFI_SLEEP_MS = 50;
const STATS_SAMPLE_INTERVAL_MS = 100;
const STDOUT_CHUNK_BYTES = 32 * 1024;
const STDOUT_COALESCE_BYTES = 4 * 1024;
const STDOUT_COALESCE_MS = 16;

let wasmMemory: WebAssembly.Memory | null = null;
let t0 = 0;
// xterm credits bound queued output even when the guest prints continuously.
const stdoutPending = new Uint8Array(STDOUT_WINDOW_BYTES);
let stdoutPendingLength = 0;
let stdoutCredit = 0;
let stdoutDroppedBytes = 0;
let stdoutLastPost = 0;
let stdoutFlushTimer: ReturnType<typeof setTimeout> | null = null;
const textEncoder = new TextEncoder();

let sampleInstret = 0;
let sampleStart = 0;

let sleepTimer: ReturnType<typeof setTimeout> | null = null;

// MessageChannel yields without the setTimeout clamp.
const channel = new MessageChannel();
const pump = channel.port2;
let iterationQueued = false;
channel.port1.onmessage = () => {
  iterationQueued = false;
  iterateSafely();
};

function scheduleImmediate(): void {
  if (iterationQueued) return;
  iterationQueued = true;
  pump.postMessage(null);
}

function cancelSleep(): void {
  if (sleepTimer !== null) {
    clearTimeout(sleepTimer);
    sleepTimer = null;
  }
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message || error.name;
  if (typeof error === "string") return error;
  return "unknown error";
}

function issueKind(
  error: unknown,
  fallback: ErrorMessage["kind"],
): ErrorMessage["kind"] {
  return /disk|opfs|quota|storage/i.test(errorMessage(error))
    ? "disk"
    : fallback;
}

function reportIssue(
  kind: ErrorMessage["kind"],
  message: string,
  fatal: boolean,
): void {
  post({ type: "error", kind, message, fatal });
}

function reportFatal(
  kind: ErrorMessage["kind"],
  context: string,
  error: unknown,
): void {
  void session.fail(new Error(`${kind}: ${context}: ${errorMessage(error)}`));
}

const session = new Session(
  new BrowserDisk((loaded, done) =>
    postDownloadProgress("disk-seed", loaded, done),
  ),
  {
    create: createMachine,
    destroy: (machine: WasmMachine) =>
      disposeMachine(machine, "session disk close failed"),
    started: (machine: WasmMachine) => {
      machine.fb_mark_all_dirty();
      checkDisplayGeometry();
      if (displayVisible) captureDisplay();
      session.disk.started(machine);
      t0 = performance.now();
      sampleInstret = 0;
      displayFrames = 0;
      displayMetrics.reset();
      sampleStart = t0;
      lastIterateAt = t0;
      stallNotices = 0;
      scheduleImmediate();
    },
    pause: () => {
      pendingPointer = null;
      pendingPointerSentAt = undefined;
      cancelSleep();
      if (session.machine) session.disk.watchDiskWrites(session.machine);
      flushStdout(true);
    },
    changed: (state) => post({ type: "session-state", state }),
    error: (error) =>
      reportIssue(issueKind(error, "runtime"), errorMessage(error), true),
  },
);

const MAX_WEB_CRYPTO_BYTES = 65_536;

function refillEntropy(target: WasmMachine): void {
  const needed = target.entropy_needed();
  if (!Number.isSafeInteger(needed) || needed <= 0) return;
  const bytes = new Uint8Array(Math.min(needed, MAX_WEB_CRYPTO_BYTES));
  globalThis.crypto.getRandomValues(bytes);
  target.add_entropy(bytes);
}

function pollDiskError(target: WasmMachine): boolean {
  const error = target.take_disk_error();
  if (error !== undefined && error !== null && String(error).length > 0) {
    reportIssue("disk", String(error), false);
    return true;
  }
  return false;
}

function disposeMachine(target: WasmMachine, context: string): void {
  try {
    target.close_disk();
  } catch (error) {
    reportIssue("disk", `${context}: ${errorMessage(error)}`, false);
  } finally {
    try {
      pollDiskError(target);
    } catch (error) {
      reportIssue(
        "disk",
        `reading session disk status failed: ${errorMessage(error)}`,
        false,
      );
    } finally {
      target.free();
    }
  }
}

// Bound progress traffic; a done message always flushes the final count.
const DOWNLOAD_PROGRESS_INTERVAL_MS = 100;
const downloadPostedAt: Partial<
  Record<DownloadProgressMessage["asset"], number>
> = {};

function postDownloadProgress(
  asset: DownloadProgressMessage["asset"],
  loaded: number,
  done: boolean,
): void {
  const now = performance.now();
  if (
    !done &&
    now - (downloadPostedAt[asset] ?? -Infinity) < DOWNLOAD_PROGRESS_INTERVAL_MS
  ) {
    return;
  }
  downloadPostedAt[asset] = now;
  post({ type: "download-progress", asset, loaded, done });
}

async function fetchSnapshot(url: string): Promise<Uint8Array | null> {
  const response = await fetch(url);
  // Vite's dev server answers missing paths with an HTML fallback.
  const type = response.headers.get("content-type") ?? "";
  if (!response.ok || response.body === null || type.includes("text/html")) {
    return null;
  }
  let loaded = 0;
  postDownloadProgress("snapshot", 0, false);
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    const counted = countBytes(response.body, (bytes) => {
      loaded = bytes;
      postDownloadProgress("snapshot", bytes, false);
    });
    const reader = (await gunzipIfNeeded(counted)).getReader();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      total += value.length;
    }
  } finally {
    postDownloadProgress("snapshot", loaded, true);
  }
  const container = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    container.set(chunk, offset);
    offset += chunk.length;
  }
  return container;
}

async function tryRestore(
  target: WasmMachine,
  msg: BootMessage,
  allowSnapshot: boolean,
): Promise<boolean> {
  if (!msg.snapshotUrl || !msg.imageHash || !msg.diskId) return false;
  if (!allowSnapshot) return false;
  if (!session.disk.clean) {
    post({
      type: "disk-notice",
      message:
        "the disk has been written since it was seeded, so the machine is booting " +
        "normally instead of resuming the post-boot snapshot",
    });
    return false;
  }
  let container: Uint8Array | null = null;
  try {
    container = await fetchSnapshot(msg.snapshotUrl);
  } catch (error) {
    reportIssue(
      "boot",
      `post-boot snapshot could not be fetched (${errorMessage(error)}); booting normally`,
      false,
    );
    return false;
  }
  if (container === null) return false;
  target.restore(container, new Uint8Array(msg.imageHash), msg.diskId);
  return true;
}

async function createMachine(
  msg: BootMessage,
  disk: BrowserDisk,
  allowSnapshot: boolean,
): Promise<WasmMachine> {
  stdoutPendingLength = 0;
  stdoutDroppedBytes = 0;
  if (stdoutFlushTimer !== null) {
    clearTimeout(stdoutFlushTimer);
    stdoutFlushTimer = null;
  }

  const { candidate: prepared, restored } = await createRestorable({
    create: async () => {
      const target = new WasmMachine(msg.ramMB * 1024 * 1024);
      let diskStatus: DiskAttachmentStatus | null = null;
      try {
        for (const blob of msg.blobs) {
          target.load_blob(blob.paddr, new Uint8Array(blob.data));
        }
        if (msg.diskSeedUrl && msg.diskLogicalBytes) {
          diskStatus = await disk.attachDisk(target, msg.diskSeedUrl, msg.diskLogicalBytes);
        }
        return { target, diskStatus };
      } catch (error) {
        disposeMachine(target, "disk flush after boot failure failed");
        throw error;
      }
    },
    restore: ({ target }) => tryRestore(target, msg, allowSnapshot),
    destroy: ({ target }) => disposeMachine(target, "disk flush after restore failure failed"),
    resetDisk: () => disk.reset(),
    refused: error => reportIssue(
      "boot",
      `post-boot snapshot was refused (${errorMessage(error)}); booting normally`,
      false,
    ),
  });
  const { target: candidate, diskStatus } = prepared;
  try {
    candidate.set_unix_time_ms(Date.now());
    refillEntropy(candidate);
    applyNetConfig(candidate, msg.net ?? DEFAULT_NET);
    if (restored) {
      // Capture drains the idle shell's prompt; ask it for a fresh one.
      candidate.uart_input(new Uint8Array([13]));
    } else {
      candidate.set_boot(msg.pc, msg.a0, msg.a1, msg.a2);
    }
    if (diskStatus?.mode === "memory") {
      reportIssue(
        "disk",
        `OPFS unavailable; using an in-memory session disk: ${diskStatus.reason ?? "unknown error"}`,
        false,
      );
    }
    candidate.fb_request_size(requestedDisplaySize.width, requestedDisplaySize.height);
    return candidate;
  } catch (error) {
    disposeMachine(candidate, "disk flush after boot failure failed");
    throw error;
  }
}

let requestedDisplaySize = { width: FB_WIDTH, height: FB_HEIGHT };
let displayCanvas: OffscreenCanvas | null = null;
let displayContextLost = false;
let displayEvents: AbortController | null = null;
let displayContext: OffscreenCanvasRenderingContext2D | null = null;
let displayFallback = false;
let displayFrameInFlight = false;
let displayVisible = false;
let displayDebug = false;
let displayTimer: number | null = null;
let displayUsesAnimationFrame = false;
let displayBuffer = new DisplayBuffer(FB_WIDTH, FB_HEIGHT);
let displayImage = new ImageData(displayBuffer.pixels, FB_WIDTH, FB_HEIGHT);
let displayFrames = 0;
const displayMetrics = new DisplayMetrics();

const displayStats = {
  ticks: 0,
  frames: 0,
  rows: 0,
  copyMs: 0,
  since: 0,
};

function attachDisplay(canvas: OffscreenCanvas | null, debug = false): void {
  displayCanvas = canvas;
  displayEvents?.abort();
  displayEvents = new AbortController();
  displayContextLost = false;
  canvas?.addEventListener("contextlost", () => { displayContextLost = true; }, { signal: displayEvents.signal });
  canvas?.addEventListener("contextrestored", () => {
    displayContextLost = false;
    session.machine?.fb_mark_all_dirty();
    captureDisplay();
    flushDisplay();
  }, { signal: displayEvents.signal });
  const machine = session.machine;
  if (canvas) { canvas.width = displayBuffer.width; canvas.height = displayBuffer.height; }
  displayFallback = canvas === null;
  displayFrameInFlight = false;
  displayContext = canvas?.getContext("2d", { alpha: false }) ?? null;
  displayDebug = debug;
  machine?.fb_mark_all_dirty();
  checkDisplayGeometry();
}

function checkDisplayGeometry(): void {
  const machine = session.machine;
  if (!machine) return;
  const width = machine.fb_width();
  const height = machine.fb_height();
  if (width === displayBuffer.width && height === displayBuffer.height) return;
  displayBuffer = new DisplayBuffer(width, height);
  displayImage = new ImageData(displayBuffer.pixels, width, height);
  if (displayCanvas) { displayCanvas.width = width; displayCanvas.height = height; }
  machine.fb_mark_all_dirty();
}

function setDisplayVisible(visible: boolean): void {
  const machine = session.machine;
  displayVisible = visible;
  if (visible) {
    // A new canvas needs the current framebuffer even when the guest has not drawn.
    machine?.fb_mark_all_dirty();
    if (displayTimer === null) {
      scheduleDisplay();
    }
    captureDisplay();
  } else if (displayTimer !== null) {
    if (displayUsesAnimationFrame) cancelAnimationFrame(displayTimer);
    else clearTimeout(displayTimer);
    displayTimer = null;
  }
}

function scheduleDisplay(): void {
  if (typeof requestAnimationFrame === "function") {
    try {
      displayTimer = requestAnimationFrame(presentDisplay);
      displayUsesAnimationFrame = true;
      return;
    } catch { /* Some browsers expose worker rAF without supporting it. */ }
  }
  displayUsesAnimationFrame = false;
  displayTimer = setTimeout(presentDisplay, DISPLAY_FRAME_MS);
}

function presentDisplay(): void {
  displayTimer = null;
  if (!displayVisible) return;
  captureDisplay();
  flushDisplay();
  scheduleDisplay();
}

function flushDisplay(): void {
  if (displayContextLost || displayContext?.isContextLost?.() || !displayVisible || (displayFallback && displayFrameInFlight)) return;
  const rows = displayBuffer.takeRows();
  if (rows.length) {
    if (displayFallback) {
      displayFrameInFlight = true;
      const rgba = displayBuffer.copyRows(rows);
      displayMetrics.uploaded(rgba.byteLength);
      post({ type: "display-frame", width: displayBuffer.width, height: displayBuffer.height, rows, rgba }, [rows.buffer, rgba.buffer]);
    } else {
      const first = rows[0], last = rows[rows.length - 1];
      displayContext?.putImageData(displayImage, 0, 0, 0, first, displayBuffer.width, last - first + 1);
      displayMetrics.uploaded(displayBuffer.width * (last - first + 1) * 4);
      displayFrames++;
      displayMetrics.painted();
    }
  }
}

// Save idle frames before another input starts a draw; presentation stays on rAF.
function captureDisplay(): void {
  const machine = session.machine;
  if (!machine || !displayVisible) return;
  if (!displayContext && !displayFallback) return;
  checkDisplayGeometry();
  let rows: Uint32Array;
  try {
    rows = machine.fb_dirty_rows();
  } catch (error) {
    reportIssue(
      "runtime",
      `reading the display failed: ${errorMessage(error)}`,
      false,
    );
    return;
  }
  const now = performance.now();
  displayStats.ticks += 1;
  if (rows.length === 0) {
    if (displayDebug) reportDisplayStats(now);
    return;
  }

  // Capture changed rows before the guest starts its next draw.
  const copyStart = performance.now();
  const bytes = machine.fb_copy_rows(rows);
  displayMetrics.copied(performance.now() - copyStart);
  if (displayDebug) {
    displayStats.frames += 1;
    displayStats.rows += rows.length;
    displayStats.copyMs += performance.now() - copyStart;
    reportDisplayStats(now);
  }
  displayBuffer.update(rows, bytes);
}

function reportDisplayStats(now: number): void {
  if (displayStats.since === 0) {
    displayStats.since = now;
    return;
  }
  const seconds = (now - displayStats.since) / 1000;
  if (seconds < 1) return;
  console.log(
    `[display] ${(displayStats.ticks / seconds).toFixed(1)} scans/s, ` +
      `${(displayStats.frames / seconds).toFixed(1)} captures/s, ` +
      `${displayStats.frames > 0 ? (displayStats.rows / displayStats.frames).toFixed(1) : "0"} rows/capture, ` +
      `${displayStats.frames > 0 ? (displayStats.copyMs / displayStats.frames).toFixed(2) : "0"} ms copy/capture`,
  );
  displayStats.ticks = 0;
  displayStats.frames = 0;
  displayStats.rows = 0;
  displayStats.copyMs = 0;
  displayStats.since = now;
}

function applyDisplayInput(event: DisplayInput, sentAt?: number): void {
  const machine = session.machine;
  if (!machine) return;
  if (sentAt !== undefined) {
    displayMetrics.inputReceived(performance.timeOrigin + performance.now() - sentAt);
  }
  if (event.kind === "pointer") {
    pendingPointer = event;
    pendingPointerSentAt = sentAt;
    wake();
    return;
  }
  flushPointer(machine);
  switch (event.kind) {
    case "key":
      machine.input_key(event.code, event.pressed);
      break;
    case "button":
      machine.input_button(event.code, event.pressed);
      break;
    case "wheel":
      machine.input_wheel(event.delta);
      break;
  }
  wake();
}

function flushPointer(machine: WasmMachine): void {
  if (!pendingPointer) return;
  machine.input_pointer_abs(pendingPointer.x, pendingPointer.y);
  if (pendingPointerSentAt !== undefined) {
    displayMetrics.inputInjected(performance.timeOrigin + performance.now() - pendingPointerSentAt);
  }
  pendingPointerSentAt = undefined;
  pendingPointer = null;
}

function applyNetConfig(target: WasmMachine, config: NetConfig): void {
  if (config.mode === "relay" && config.relayUrl) {
    if (!network) throw new Error("network transport is not attached");
    target.set_net_transport(config.relayUrl, new GuestTransport(network));
  }
}

// Polling deadlines also cover network timeouts while the guest waits for interrupts.
function netSleepMs(target: WasmMachine): number | null {
  const deadline = target.net_deadline_ms();
  if (!Number.isFinite(deadline) || deadline < 0) return null;
  return Math.max(deadline - target.net_now_ms(), 0);
}

function enqueueStdout(out: Uint8Array): void {
  const available = stdoutPending.byteLength - stdoutPendingLength;
  const accepted = Math.min(available, out.byteLength);
  if (accepted > 0) {
    stdoutPending.set(out.subarray(0, accepted), stdoutPendingLength);
    stdoutPendingLength += accepted;
  }
  stdoutDroppedBytes += out.byteLength - accepted;
}

function scheduleStdoutFlush(): void {
  if (stdoutFlushTimer !== null || stdoutPendingLength === 0) return;
  stdoutFlushTimer = setTimeout(() => {
    stdoutFlushTimer = null;
    flushStdout(true);
  }, STDOUT_COALESCE_MS);
}

function flushStdout(force = false): void {
  if (
    !force &&
    stdoutPendingLength > 0 &&
    stdoutPendingLength < STDOUT_COALESCE_BYTES &&
    performance.now() - stdoutLastPost < STDOUT_COALESCE_MS
  ) {
    scheduleStdoutFlush();
    return;
  }
  if (force && stdoutFlushTimer !== null) {
    clearTimeout(stdoutFlushTimer);
    stdoutFlushTimer = null;
  }
  while (stdoutCredit > 0 && stdoutPendingLength > 0) {
    const bytes = Math.min(
      stdoutCredit,
      stdoutPendingLength,
      STDOUT_CHUNK_BYTES,
    );
    const data = stdoutPending.slice(0, bytes);
    stdoutPending.copyWithin(0, bytes, stdoutPendingLength);
    stdoutPendingLength -= bytes;
    stdoutCredit -= bytes;
    post({ type: "stdout", data }, [data.buffer]);
    stdoutLastPost = performance.now();
  }
  if (stdoutPendingLength === 0 && stdoutDroppedBytes > 0) {
    const dropped = stdoutDroppedBytes;
    stdoutDroppedBytes = 0;
    enqueueStdout(
      textEncoder.encode(
        `\r\n[terminal output truncated: ${dropped} bytes]\r\n`,
      ),
    );
    if (stdoutCredit > 0) flushStdout(force);
  }
  if (stdoutPendingLength > 0 && stdoutCredit > 0) scheduleStdoutFlush();
}

function grantStdoutCredit(bytes: number): void {
  if (!Number.isSafeInteger(bytes) || bytes <= 0) return;
  stdoutCredit = Math.min(STDOUT_WINDOW_BYTES, stdoutCredit + bytes);
  flushStdout();
}

function drainStdout(): void {
  const machine = session.machine;
  if (!machine) return;
  flushStdout();
  const control = machine.control_output();
  if (control.length) post({ type: "control-output", data: control }, [control.buffer]);
  const out = machine.uart_output();
  if (out.length > 0) {
    enqueueStdout(out);
    flushStdout();
  }
}

function counter(value: number): number {
  return Number.isFinite(value) ? value : 0;
}

function sampleStats(
  target: WasmMachine | null,
  mips: number,
): EmulatorStats | undefined {
  if (!target) return undefined;
  return {
    mips,
    displayFrames,
    ...displayMetrics.sample(),
    inputPendingEvents: target.input_pending_events(),
    displayCommitted: target.fb_committed(),
    displayBackend: displayFallback ? "canvas-main" : "canvas-worker",
    memoryBytes: wasmMemory?.buffer.byteLength ?? 0,
    ramBytes: counter(target.ram_bytes()),
    ramTouchedBytes: counter(target.ram_touched_bytes()),
    diskBytesRead: counter(target.disk_bytes_read()),
    diskBytesWritten: counter(target.disk_bytes_written()),
    networkConnections: network?.connections.snapshot() ?? [],
    netBytesRx: counter(target.net_bytes_rx()),
    netBytesTx: counter(target.net_bytes_tx()),
    decodeCacheHits: counter(target.decode_cache_hits()),
    decodeCacheMisses: counter(target.decode_cache_misses()),
  };
}

function maybeSampleStats(now: number): void {
  const machine = session.machine;
  const elapsed = now - sampleStart;
  if (elapsed < STATS_SAMPLE_INTERVAL_MS) return;
  const stats = sampleStats(machine, sampleInstret / 1000 / elapsed);
  sampleStart = now;
  sampleInstret = 0;
  if (stats) post({ type: "stats-sample", stats });
}

function advanceHostTime(target: WasmMachine, now: number): void {
  const hostTicks = (now - t0) * TICKS_PER_MS;
  target.advance_mtime_from_host(hostTicks);
}

// Allow for timer clamping before treating a missing iteration as a lost wake-up.
const WATCHDOG_STALL_MS = 2_000;
const WATCHDOG_INTERVAL_MS = 1_000;
const MAX_STALL_NOTICES = 3;

let lastIterateAt = 0;
let stallNotices = 0;
let unknownStatusReported = false;

function scheduleSleep(ms: number): void {
  if (iterationQueued) return;
  const delay = Number.isFinite(ms)
    ? Math.min(Math.max(ms, 0), MAX_WFI_SLEEP_MS)
    : MAX_WFI_SLEEP_MS;
  if (delay < 1) {
    scheduleImmediate();
    return;
  }
  sleepTimer = setTimeout(() => {
    sleepTimer = null;
    iterateSafely();
  }, delay);
}

function wake(): void {
  const machine = session.machine;
  if (!machine) return;
  cancelSleep();
  scheduleImmediate();
}

function iterate(): void {
  const machine = session.machine;
  if (!machine) return;
  lastIterateAt = performance.now();
  cancelSleep();

  advanceHostTime(machine, performance.now());
  refillEntropy(machine);

  // The main thread already limits motion frequency; deliver its latest position now.
  flushPointer(machine);
  const executionStart = performance.now();
  const status = machine.run(SLICE_BUDGET);
  displayMetrics.executed(performance.now() - executionStart);
  if (pollDiskError(machine)) {
    void session.reset();
    return;
  }
  drainStdout();

  const now = performance.now();

  switch (status) {
    case STATUS_BUDGET_EXHAUSTED: {
      sampleInstret += SLICE_BUDGET;
      maybeSampleStats(now);
      scheduleImmediate();
      break;
    }
    case STATUS_WFI: {
      captureDisplay();
      // Always schedule the next wake-up, even if polling a WFI guest throws.
      let ms = MAX_WFI_SLEEP_MS;
      try {
        flushStdout(true);
        maybeSampleStats(now);
        advanceHostTime(machine, performance.now());
        const deadline = machine.next_timer_deadline();
        if (Number.isFinite(deadline) && deadline >= 0) {
          const dueIn = (deadline - machine.mtime()) / TICKS_PER_MS;
          ms = Math.min(Math.max(dueIn, 0), MAX_WFI_SLEEP_MS);
        }
        const netDue = netSleepMs(machine);
        if (netDue !== null) ms = Math.min(ms, netDue);
      } finally {
        if (pendingPointer) scheduleImmediate();
        else scheduleSleep(ms);
      }
      break;
    }
    case STATUS_SHUTDOWN: {
      const code = machine.last_exit_code();
      void session.stop().then(() => post({ type: "shutdown", code }));
      break;
    }
    case STATUS_RESET: {
      void session.restart();
      break;
    }
    default: {
      if (!unknownStatusReported) {
        unknownStatusReported = true;
        reportIssue(
          "protocol",
          `emulator returned unknown run status ${String(status)}`,
          false,
        );
      }
      scheduleImmediate();
      break;
    }
  }

  if (session.state === "running") {
    session.disk.watchDiskWrites(machine);
  }
}

setInterval(() => {
  if (session.state !== "running") return;
  if (performance.now() - lastIterateAt < WATCHDOG_STALL_MS) return;
  if (stallNotices < MAX_STALL_NOTICES) {
    stallNotices += 1;
    reportIssue("runtime", "emulator run loop stalled; resuming", false);
  }
  lastIterateAt = performance.now();
  wake();
}, WATCHDOG_INTERVAL_MS);

function iterateSafely(): void {
  try {
    iterate();
  } catch (error) {
    reportFatal("runtime", "emulator failed", error);
  }
}

ctx.onmessage = (ev: MessageEvent<MainToWorker>) => {
  try {
    const msg = ev.data;
    switch (msg.type) {
      case "network-attach":
        network?.dispose();
        network = new NetworkPort(msg.port, msg.connected, connected => post({ type: "network-status", connected }), wake);
        break;
      case "boot":
        void session.boot(msg);
        break;
      case "display-resize":
        requestedDisplaySize = { width: msg.width, height: msg.height };
        session.machine?.fb_request_size(msg.width, msg.height);
        break;
      case "display-frame-ack":
        if (displayFrameInFlight) { displayFrames++; displayMetrics.painted(); }
        displayFrameInFlight = false;
        captureDisplay();
        flushDisplay();
        break;
      case "display-attach":
        attachDisplay(msg.canvas, msg.debug === true);
        break;
      case "display-visible":
        setDisplayVisible(msg.visible);
        break;
      case "display-input":
        applyDisplayInput(msg.event, msg.sentAt);
        break;
      case "control-input":
        if (session.machine && !session.machine.control_input(msg.data)) {
          post({ type: "control-output", data: textEncoder.encode(`${new TextDecoder().decode(msg.data).split(" ")[0]} error\n`) });
        }
        wake();
        break;
      case "stdin":
        if (session.machine) {
          session.machine.uart_input(msg.data);
          wake();
        }
        break;
      case "reset-disk":
        void session.reset();
        break;
      case "restart":
        void session.restart();
        break;
      case "power-off":
        void session.stop().then(() => post({ type: "shutdown", code: 0 }));
        break;
      case "stdout-credit":
        grantStdoutCredit(msg.bytes);
        break;
      case "dispose":
        void session.dispose().finally(() => network?.dispose());
        break;
    }
  } catch (error) {
    reportFatal("protocol", "worker message failed", error);
  }
};

init({
  module_or_path: new URL("./wasm/emulate_wasm_bg.wasm", import.meta.url),
})
  .then((exports) => {
    wasmMemory = exports.memory;

    post({ type: "ready" });
  })
  .catch((error: unknown) =>
    reportFatal("boot", "emulator initialization failed", error),
  );
