import { subscribeColorMode } from "./color-mode";
import { createDownloadIndicator } from "./download-progress";
import guestAssets from "./generated/guest.json";
import { diskSeed as manifestDiskSeed } from "./session/seed";
import { welcomeBanner } from "./welcome";
import { connectNetwork, type NetworkAttachment } from "./network/connect";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

import {
  DEFAULT_RAM_MB,
  DISK_LOGICAL_BYTES,
  imageIdentityHash,
  STDOUT_WINDOW_BYTES,
  buildFwDynamicInfo,
  guestMemoryLayout,
  resolveNetConfig,
  type BlobSpec,
  type EmulatorStats,
  type ErrorMessage,
  type MainToWorker,
  type WorkerToMain,
} from "./protocol";
import type { SessionState } from "./session/session";

import { GuestControl, type GuestAction, type GuestStatus } from "./session/guest-control";

const RAM_MB = DEFAULT_RAM_MB;
const memoryLayout = guestMemoryLayout(RAM_MB);

// Expected sizes and labels for the assets the worker streams itself.
const workerDownloads = {
  "disk-seed": {
    label: "root filesystem",
    bytes: guestAssets.files["rootfs.ext4.gz"].bytes,
  },
  snapshot: {
    label: "boot snapshot",
    bytes: guestAssets.files["snapshot.bin.gz"].bytes,
  },
} as const;

const viteEnv = import.meta.env;
const defaultRelay = viteEnv?.DEV ? undefined
  : `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/api/relay`;
const netConfig = resolveNetConfig(viteEnv?.VITE_RELAY_URL ?? defaultRelay);

export const browserNetConfig = netConfig;
export interface EmulatorClient {
  send(message: MainToWorker, transfer?: Transferable[]): void;
  control(action: GuestAction): Promise<void>;
  focus(): void;
  power(action: "restart" | "power-off" | "reset-disk"): void;
  dispose(): void;
}

export function createClient(
  element: HTMLElement,
  handlers: {
    guest(status: GuestStatus): void;
    network(connected: boolean): void;
    error(message: string): void;
    stats(stats: EmulatorStats): void;
    state(state: SessionState): void;
    frame(rows: Uint32Array, rgba: Uint8ClampedArray<ArrayBuffer>, width: number, height: number): void;
  },
): EmulatorClient {
  const control = new GuestControl(bytes => send({ type: "control-input", data: bytes }), handlers.guest, () => {
    if (netConfig.mode === "relay") void control.request("network.connect").catch(error => {
      if (!disposed && error?.name !== "AbortError") handlers.error(String(error));
    });
  });
  let disposed = false;
  const downloads = new AbortController();
  function token(name: string, fallback: string): string {
    const value = getComputedStyle(document.documentElement)
      .getPropertyValue(name)
      .trim();
    return value || fallback;
  }

  function terminalTheme(): ITheme {
    return {
      background: token("--term-bg", "#0a0a0a"),
      foreground: token("--term-fg", "#ededed"),
      cursor: token("--term-cursor", "#3b9eff"),
      cursorAccent: token("--term-cursor-accent", "#0a0a0a"),
      selectionBackground: token(
        "--term-selection",
        "rgba(59, 158, 255, 0.28)",
      ),
      black: token("--term-black", "#4a4a4a"),
      red: token("--term-red", "#ff6166"),
      green: token("--term-green", "#62c073"),
      yellow: token("--term-yellow", "#f0b429"),
      blue: token("--term-blue", "#3b9eff"),
      magenta: token("--term-magenta", "#c08cf0"),
      cyan: token("--term-cyan", "#4fc3d0"),
      white: token("--term-white", "#a1a1a1"),
      brightBlack: token("--term-bright-black", "#666666"),
      brightRed: token("--term-bright-red", "#ff8b8f"),
      brightGreen: token("--term-bright-green", "#86d494"),
      brightYellow: token("--term-bright-yellow", "#ffcc5c"),
      brightBlue: token("--term-bright-blue", "#6fb6ff"),
      brightMagenta: token("--term-bright-magenta", "#d3a8f7"),
      brightCyan: token("--term-bright-cyan", "#7ad8e3"),
      brightWhite: token("--term-bright-white", "#ffffff"),
    };
  }

  const MONO_FALLBACK =
    'ui-monospace, "SF Mono", "Cascadia Code", Menlo, Consolas, monospace';
  const MONO_STACK = `"Geist Mono", ${MONO_FALLBACK}`;
  const monoReady = document.fonts?.check?.('14px "Geist Mono"') ?? false;

  const term = new Terminal({
    cursorBlink: true,
    scrollback: 5000,
    fontSize: 14,
    fontFamily: monoReady ? MONO_STACK : MONO_FALLBACK,
    theme: terminalTheme(),
  });

  const onTheme = () => {
    term.options.theme = terminalTheme();
  };
  const unsubscribeTheme = subscribeColorMode(onTheme);

  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(element);
  if (element.clientHeight > 0) fit.fit();
  term.write(welcomeBanner(term.cols));
  const onResize = () => {
    if (!disposed && element.clientHeight > 0) fit.fit();
  };
  window.addEventListener("resize", onResize);

  const observer = new ResizeObserver(onResize);
  observer.observe(element);

  // A real value change makes xterm re-measure the cell; refit to the new grid.
  if (!monoReady && document.fonts?.ready) {
    void document.fonts.ready.then(() => {
      if (disposed) return;
      term.options.fontFamily = MONO_STACK;
      onResize();
    });
  }

  const benchmark = new URLSearchParams(location.search).has("benchmark");
  const worker = new Worker(new URL("./worker.ts", import.meta.url), {
    type: "module",
  });
  function send(msg: MainToWorker, transfer: Transferable[] = []): void {
    if (benchmark && msg.type === "display-input") {
      msg = { ...msg, sentAt: performance.timeOrigin + performance.now() };
    }
    if (!disposed) worker.postMessage(msg, transfer);
  }
  const pagehide = (event: PageTransitionEvent) => {
    if (!event.persisted) send({ type: "dispose" });
  };
  window.addEventListener("pagehide", pagehide);
  let network: NetworkAttachment | null = null;
  let networkReady = netConfig.mode === "none";
  if (netConfig.mode === "relay" && netConfig.relayUrl) {
    void connectNetwork(netConfig.relayUrl).then(attachment => {
      if (disposed) { attachment.dispose(); return; }
      network = attachment;
      send({ type: "network-attach", port: attachment.port, connected: attachment.connected }, [attachment.port]);
      networkReady = true;
      void tryBoot().catch(error => renderWorkerError({ kind: "boot", message: String(error), fatal: true }));
    }).catch(error => {
      if (!disposed) renderWorkerError({ kind: "boot", message: String(error), fatal: true });
    });
  }
  const indicator = createDownloadIndicator((text) => {
    if (!disposed) term.write(text);
  });
  let workerReady = false;
  let bootAttempted = false;
  let bootConfigured = false;
  let workerFailed = false;
  function renderWorkerError(issue: Omit<ErrorMessage, "type">): void {
    if (issue.fatal && workerFailed) return;
    if (issue.fatal) {
      workerFailed = true;
      control.reset();
      handlers.state("failed");
    }
    if (issue.fatal) handlers.error(issue.message);
    console.error(`[${issue.kind}] ${issue.message}`);
    indicator.hide();
    const printable = issue.message
      .replace(/[\u0000-\u001f\u007f]+/g, " ")
      .trim();
    const severity = issue.fatal ? "error" : "warning";
    const color = issue.fatal ? 31 : 33;
    term.write(
      `\r\n\x1b[${color}m[${issue.kind} ${severity}] ${printable || "unknown worker error"}\x1b[0m\r\n`,
    );
  }

  worker.onmessage = (ev: MessageEvent<WorkerToMain>) => {
    const msg = ev.data;
    switch (msg.type) {
      case "network-status":
        handlers.network(msg.connected);
        break;
      case "session-state":
        if (["starting", "stopping", "stopped", "failed", "disposed"].includes(msg.state)) control.reset();
        workerFailed = msg.state === "failed";
        handlers.state(msg.state);
        break;
      case "ready":
        workerReady = true;
        send({ type: "stdout-credit", bytes: STDOUT_WINDOW_BYTES });
        void tryBoot().catch((error: unknown) =>
          renderWorkerError({
            kind: "boot",
            message: String(error),
            fatal: true,
          }),
        );
        break;
      case "control-output":
        control.push(msg.data);
        break;
      case "stdout": {
        const bytes = msg.data.byteLength;
        term.write(msg.data, () => send({ type: "stdout-credit", bytes }));
        break;
      }
      case "display-frame":
        handlers.frame(msg.rows, msg.rgba, msg.width, msg.height);
        break;
      case "stats-sample":
        handlers.stats(msg.stats);
        if (benchmark) window.dispatchEvent(new CustomEvent("emulate-stats", { detail: msg.stats }));
        break;
      case "disk-notice":
        indicator.hide();
        term.write(`\r\n\x1b[2m[${msg.message}]\x1b[0m\r\n`);
        break;
      case "download-progress": {
        const { label, bytes } = workerDownloads[msg.asset];
        indicator.update(msg.asset, label, msg.loaded, bytes, msg.done);
        break;
      }
      case "shutdown":
        term.write(
          `\r\n\x1b[33m[machine halted, exit code ${msg.code}]\x1b[0m\r\n`,
        );
        break;
      case "error":
        renderWorkerError(msg);
        break;
    }
  };

  worker.onerror = (event: ErrorEvent) => {
    event.preventDefault();
    renderWorkerError({
      kind: "runtime",
      message: event.message || "worker crashed",
      fatal: true,
    });
  };

  worker.onmessageerror = () => {
    renderWorkerError({
      kind: "protocol",
      message: "received an invalid message from the emulator worker",
      fatal: true,
    });
  };

  const encoder = new TextEncoder();
  const input = term.onData((data) => {
    const bytes = encoder.encode(data);
    send({ type: "stdin", data: bytes }, [bytes.buffer]);
  });
  async function fetchBlob(
    name: keyof typeof guestAssets.files,
  ): Promise<ArrayBuffer | null> {
    const file = guestAssets.files[name];
    let loaded = 0;
    const report = (done: boolean) =>
      indicator.update(name, name, loaded, file.bytes, done);
    try {
      const res = await fetch(file.url, { signal: downloads.signal });
      if (!res.ok) return null;
      // Vite's dev server can answer missing paths with an HTML fallback.
      const ct = res.headers.get("content-type") ?? "";
      if (ct.includes("text/html")) return null;
      if (!res.body) return await res.arrayBuffer();
      report(false);
      const reader = res.body.getReader();
      const chunks: Uint8Array[] = [];
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        loaded += value.byteLength;
        report(false);
      }
      const buffer = new Uint8Array(loaded);
      let offset = 0;
      for (const chunk of chunks) {
        buffer.set(chunk, offset);
        offset += chunk.byteLength;
      }
      return buffer.buffer;
    } catch {
      return null;
    } finally {
      report(true);
    }
  }

  function showMissingGuestMessage(missing: string[]): void {
    term.writeln("\x1b[1memulate.computer\x1b[0m");
    term.writeln("");
    term.writeln("\x1b[33mGuest images are not installed yet.\x1b[0m");
    term.writeln("");
    term.writeln(`Missing from web/public/guest/: ${missing.join(", ")}`);
    term.writeln("");
    term.writeln(
      "To fetch/build the guest firmware, kernel, and device tree, run:",
    );
    term.writeln("");
    term.writeln("    \x1b[36mjust guest\x1b[0m");
    term.writeln("");
    term.writeln("then reload this page.");
  }

  async function tryBoot(): Promise<void> {
    if (!workerReady || !networkReady || bootAttempted || disposed) return;
    bootAttempted = true;
    const [fw, kernel, initramfsDtb, rootfsDtb, initrd, diskSeed] =
      await Promise.all([
        fetchBlob("fw.bin"),
        fetchBlob("kernel.bin"),
        fetchBlob("dtb.bin"),
        fetchBlob("dtb-desktop.bin"),
        fetchBlob("initrd.bin"),
        Promise.resolve(manifestDiskSeed(guestAssets)),
      ]);
    if (disposed) return;
    const useRootfs = rootfsDtb !== null && diskSeed !== null;
    const dtb = useRootfs ? rootfsDtb : initramfsDtb;
    // Hash shipped images before changing the command line to match snapshot identity.
    const imageHash =
      useRootfs && fw && kernel && dtb
        ? await imageIdentityHash([fw, kernel, initrd, dtb])
        : undefined;
    const missing: string[] = [];
    if (!fw) missing.push("fw.bin");
    if (!kernel) missing.push("kernel.bin");
    if (!dtb) missing.push("dtb.bin");
    if (!useRootfs)
      missing.push("desktop root filesystem (run just guest-rootfs)");
    if (missing.length > 0) {
      showMissingGuestMessage(missing);
      handlers.error(
        "Guest images are missing. Open the console for setup instructions.",
      );
      handlers.state("failed");
      return;
    }

    const fwInfo = buildFwDynamicInfo(memoryLayout.kernelAddr);
    const blobs: BlobSpec[] = [
      { paddr: memoryLayout.fwAddr, data: fw! },
      { paddr: memoryLayout.kernelAddr, data: kernel! },
      { paddr: memoryLayout.dtbAddr, data: dtb! },
      {
        paddr: memoryLayout.fwDynamicInfoAddr,
        data: fwInfo.buffer as ArrayBuffer,
      },
    ];
    if (initrd) {
      blobs.push({ paddr: memoryLayout.initrdAddr, data: initrd });
    }

    if (disposed) return;
    bootConfigured = true;
    const transfer = blobs.map((b) => b.data);
    send(
      {
        type: "boot",
        ramMB: RAM_MB,
        blobs,
        diskSeedUrl: useRootfs ? diskSeed!.url : undefined,
        diskId: useRootfs ? diskSeed!.id : undefined,
        diskLogicalBytes: useRootfs ? DISK_LOGICAL_BYTES : undefined,
        snapshotUrl: imageHash ? guestAssets.files["snapshot.bin.gz"].url : undefined,
        imageHash,
        net: netConfig,
        pc: memoryLayout.bootPc,
        a0: memoryLayout.bootA0,
        a1: memoryLayout.bootA1,
        a2: memoryLayout.bootA2,
      },
      transfer,
    );
  }

  return {
    send,
    control: action => disposed ? Promise.reject(new Error("Guest session ended")) : control.request(action),
    focus: () => term.focus(),
    power: (type) => {
      if (!bootConfigured && type === "restart") {
        bootAttempted = false;
        workerFailed = false;
        handlers.state("starting");
        void tryBoot().catch((error: unknown) =>
          renderWorkerError({
            kind: "boot",
            message: String(error),
            fatal: true,
          }),
        );
        return;
      }
      if (type !== "power-off")
        term.write(
          `\r\n\x1b[33m[${type === "restart" ? "restart" : "reset"}]\x1b[0m\r\n`,
        );
      send({ type });
    },
    dispose: () => {
      send({ type: "dispose" });
      indicator.hide();
      disposed = true;
      control.reset();
      network?.dispose();
      downloads.abort();
      input.dispose();
      observer.disconnect();
      window.removeEventListener("resize", onResize);
      window.removeEventListener("pagehide", pagehide);
      unsubscribeTheme();
      // xterm 5 queues an uncancellable viewport setup timeout in open().
      // Detach now, then dispose after that timeout (including Strict Mode replay).
      term.element?.remove();
      setTimeout(() => term.dispose(), 0);
      // Give the worker a chance to flush and close OPFS before termination.
      const timeout = setTimeout(() => worker.terminate(), 5000);
      worker.onmessage = ({ data }: MessageEvent<WorkerToMain>) => {
        if (data.type === "session-state" && data.state === "disposed") {
          clearTimeout(timeout);
          worker.terminate();
        }
      };
      worker.onerror = worker.onmessageerror = null;
    },
  };
}
