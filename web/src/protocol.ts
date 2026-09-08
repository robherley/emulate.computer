import type { Connection } from "./network/connections";
import type { SessionState } from "./session/session";

export const DRAM_BASE = 0x8000_0000;

export const DEFAULT_RAM_MB = 128;
const MIB = 1024 * 1024;
const TWO_MB = 2 * MIB;
const INITRD_OFFSET = 70 * MIB;
const DTB_ADDR_CAP = DRAM_BASE + 120 * MIB;
const FW_DYNAMIC_INFO_OFFSET = 0x1000;

export const FW_ADDR = DRAM_BASE;

export const KERNEL_ADDR = DRAM_BASE + TWO_MB;

export const INITRD_ADDR = DRAM_BASE + INITRD_OFFSET;

export interface GuestMemoryLayout {
  fwAddr: number;
  kernelAddr: number;
  initrdAddr: number;
  dtbAddr: number;
  fwDynamicInfoAddr: number;
  bootPc: number;
  bootA0: number;
  bootA1: number;
  bootA2: number;
}

// Match the native runner: DTB 2 MiB below RAM end, aligned and capped.
export function guestMemoryLayout(ramMB: number): GuestMemoryLayout {
  if (!Number.isSafeInteger(ramMB) || ramMB <= 0) {
    throw new Error(
      `RAM size must be a positive integer MiB count (got ${ramMB})`,
    );
  }
  const ramBytes = ramMB * MIB;
  const ramEnd = DRAM_BASE + ramBytes;
  if (!Number.isSafeInteger(ramEnd)) {
    throw new Error(
      `RAM size ${ramMB} MiB exceeds the JavaScript address range`,
    );
  }
  const candidate = Math.floor(Math.max(ramEnd - TWO_MB, 0) / TWO_MB) * TWO_MB;
  const dtbAddr = Math.min(candidate, DTB_ADDR_CAP);
  const fwDynamicInfoAddr = dtbAddr - FW_DYNAMIC_INFO_OFFSET;
  return {
    fwAddr: FW_ADDR,
    kernelAddr: KERNEL_ADDR,
    initrdAddr: INITRD_ADDR,
    dtbAddr,
    fwDynamicInfoAddr,
    bootPc: FW_ADDR,
    bootA0: 0,
    bootA1: dtbAddr,
    bootA2: fwDynamicInfoAddr,
  };
}

export const FW_DYNAMIC_INFO_MAGIC = 0x4942_534f;
export const FW_DYNAMIC_INFO_VERSION = 2;
/** Privilege mode for the next stage: 1 = S-mode. */
export const FW_DYNAMIC_INFO_NEXT_MODE = 1;
export const FW_DYNAMIC_INFO_OPTIONS = 0;

// OpenSBI fw_dynamic_info: magic, version, next_addr, next_mode, options as LE u64s.
export function buildFwDynamicInfo(nextAddr = KERNEL_ADDR): Uint8Array {
  const buf = new ArrayBuffer(5 * 8);
  const view = new DataView(buf);
  const fields = [
    FW_DYNAMIC_INFO_MAGIC,
    FW_DYNAMIC_INFO_VERSION,
    nextAddr,
    FW_DYNAMIC_INFO_NEXT_MODE,
    FW_DYNAMIC_INFO_OPTIONS,
  ];
  fields.forEach((v, i) => view.setBigUint64(i * 8, BigInt(v), true));
  return new Uint8Array(buf);
}

/** mtime frequency advertised in the DTB (matches emulate-core TIMEBASE_FREQ). */
export const TIMEBASE_FREQ = 10_000_000;
export const TICKS_PER_MS = TIMEBASE_FREQ / 1000;

/** Maximum worker stdout bytes awaiting xterm acknowledgement. */
export const STDOUT_WINDOW_BYTES = 256 * 1024;

export type NetMode = "relay" | "none";

export interface NetConfig {
  mode: NetMode;
  relayUrl?: string;
}

export const DEFAULT_RELAY_URL = "ws://127.0.0.1:7654";
export const DEFAULT_NET: NetConfig = {
  mode: "relay",
  relayUrl: DEFAULT_RELAY_URL,
};

export function resolveNetConfig(envRelayUrl?: string): NetConfig {
  const relayUrl = envRelayUrl?.trim() || DEFAULT_RELAY_URL;
  if (relayUrl && /^wss?:\/\//i.test(relayUrl))
    return { mode: "relay", relayUrl };
  return { mode: "none" };
}

// Must match size_mb in scripts/build-rootfs.sh.
export const DISK_LOGICAL_BYTES = 512 * 1024 * 1024;


// Match snapshot::hash_images: firmware, kernel, initramfs, DTB, each prefixed by its LE u64 length.
export async function imageIdentityHash(
  parts: (ArrayBuffer | null)[],
): Promise<ArrayBuffer> {
  const total = parts.reduce((n, p) => n + 8 + (p?.byteLength ?? 0), 0);
  const framed = new Uint8Array(total);
  const view = new DataView(framed.buffer);
  let offset = 0;
  for (const part of parts) {
    view.setBigUint64(offset, BigInt(part?.byteLength ?? 0), true);
    offset += 8;
    if (part) {
      framed.set(new Uint8Array(part), offset);
      offset += part.byteLength;
    }
  }
  return await crypto.subtle.digest("SHA-256", framed);
}

export interface BlobSpec {
  paddr: number;
  data: ArrayBuffer;
}

export interface BootMessage {
  type: "boot";
  ramMB: number;
  blobs: BlobSpec[];
  diskSeedUrl?: string;
  diskId?: string;
  diskLogicalBytes?: number;
  snapshotUrl?: string;
  imageHash?: ArrayBuffer;
  net?: NetConfig;
  pc: number;
  a0: number;
  a1: number;
  a2: number;
}

// Boot geometry; the guest acknowledges runtime sizes through the framebuffer device.
export const FB_WIDTH = 1024;
export const FB_HEIGHT = 768;

export const DISPLAY_FRAME_MS = 16;

// Bound frame latency while waiting for the guest to finish drawing.

export const POINTER_FLUSH_MS = 16;

// A null canvas selects main-thread rendering through display-frame messages.
export interface DisplayAttachMessage {
  type: "display-attach";
  canvas: OffscreenCanvas | null;
  debug?: boolean;
}

export interface DisplayVisibleMessage {
  type: "display-visible";
  visible: boolean;
}

// evdev keys/buttons, absolute pointer coordinates 0..32767, wheel in whole detents.
export type DisplayInput =
  | { kind: "key"; code: number; pressed: boolean }
  | { kind: "button"; code: number; pressed: boolean }
  | { kind: "pointer"; x: number; y: number }
  | { kind: "wheel"; delta: number };

export interface DisplayInputMessage {
  type: "display-input";
  event: DisplayInput;
  sentAt?: number;
}

export interface StdinMessage {
  type: "stdin";
  data: Uint8Array;
}

// Reset erases the disk; restart preserves it.
export interface ResetDiskMessage {
  type: "reset-disk";
}

export interface RestartMessage {
  type: "restart";
}

export interface PowerOffMessage {
  type: "power-off";
}

export interface StdoutCreditMessage {
  type: "stdout-credit";
  bytes: number;
}

export interface DisposeMessage {
  type: "dispose";
}

export type MainToWorker =
  | { type: "control-input"; data: Uint8Array }
  | { type: "display-frame-ack" }
  | { type: "display-resize"; width: number; height: number }
  | { type: "network-attach"; port: MessagePort; connected: boolean }
  | BootMessage
  | DisplayAttachMessage
  | DisplayVisibleMessage
  | DisplayInputMessage
  | StdinMessage
  | ResetDiskMessage
  | RestartMessage
  | PowerOffMessage
  | StdoutCreditMessage
  | DisposeMessage;

export interface ReadyMessage {
  type: "ready";
}

export interface StdoutMessage {
  type: "stdout";
  data: Uint8Array;
}

// I/O byte counters are cumulative since boot; the main thread derives rates.
export interface EmulatorStats {
  workerSliceP95?: number;
  inputQueueP95?: number;
  inputToGuestP95?: number;
  inputPendingEvents?: number;
  displayFrameP95?: number;
  executionPercent?: number;
  displayCopyMs?: number;
  displayUploadBytesPerSec?: number;
  displayCommitted?: boolean;
  displayBackend?: "canvas-worker" | "canvas-main";
  displayFrames: number;
  mips: number;
  memoryBytes: number;
  ramBytes: number;
  // Bytes ever written: a high-water mark, not live guest memory usage.
  ramTouchedBytes: number;
  diskBytesRead: number;
  diskBytesWritten: number;
  networkConnections?: Connection[];
  netBytesRx: number;
  netBytesTx: number;
  decodeCacheHits?: number;
  decodeCacheMisses?: number;
}

export interface StatsSampleMessage {
  type: "stats-sample";
  stats: EmulatorStats;
}

export interface DiskNoticeMessage {
  type: "disk-notice";
  message: string;
}

// rows[i] identifies the framebuffer row for the i-th width-pixel run in rgba.
export interface DisplayFrameMessage {
  type: "display-frame";
  width: number;
  height: number;
  rows: Uint32Array;
  rgba: Uint8ClampedArray<ArrayBuffer>;
}

export interface ShutdownMessage {
  type: "shutdown";
  code: number;
}

export interface ErrorMessage {
  type: "error";
  kind: "boot" | "disk" | "runtime" | "protocol";
  message: string;
  fatal: boolean;
}

export type WorkerToMain =
  | { type: "control-output"; data: Uint8Array }
  | { type: "network-status"; connected: boolean }
  | { type: "session-state"; state: SessionState }
  | ReadyMessage
  | DisplayFrameMessage
  | StdoutMessage
  | StatsSampleMessage
  | DiskNoticeMessage
  | ShutdownMessage
  | ErrorMessage;
