export type DesktopState = "starting" | "ready" | "failed" | "stopped";
export type GuestStatus =
  | { kind: "desktop"; state: DesktopState }
  | { kind: "network"; address: string | null };
export type GuestAction = "desktop.start" | "desktop.stop" | "network.connect" | "network.disconnect" | "status";

type Request = { action: GuestAction; resolve(): void; reject(error: Error): void; timer: ReturnType<typeof setTimeout> };

export class GuestControl {
  private line = "";
  private overflow = false;
  private ready = false;
  private nextId = 1;
  private requests = new Map<number, Request>();

  private send: (bytes: Uint8Array) => void;
  private report: (status: GuestStatus) => void;
  private onReady: () => void;
  private timeout: number;

  constructor(send: (bytes: Uint8Array) => void, report: (status: GuestStatus) => void, onReady = () => {}, timeout = 120_000) {
    this.send = send;
    this.report = report;
    this.onReady = onReady;
    this.timeout = timeout;
  }

  request(action: GuestAction): Promise<void> {
    if (this.requests.size >= 16) return Promise.reject(new Error("Too many guest requests"));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.requests.delete(id);
        reject(new Error(`Guest did not respond to ${action}`));
      }, this.timeout);
      this.requests.set(id, { action, resolve, reject, timer });
      if (this.ready) this.transmit(id, action);
    });
  }

  reset(): void {
    this.ready = false;
    this.line = "";
    this.overflow = false;
    for (const request of this.requests.values()) {
      clearTimeout(request.timer);
      request.reject(new DOMException("Guest session ended", "AbortError"));
    }
    this.requests.clear();
  }

  push(bytes: Uint8Array): void {
    for (const byte of bytes) {
      if (byte === 10) {
        if (!this.overflow) this.receive(this.line);
        this.line = "";
        this.overflow = false;
      } else if (byte !== 13 && !this.overflow) {
        if (this.line.length >= 160 || byte < 32 || byte > 126) {
          this.line = "";
          this.overflow = true;
        } else this.line += String.fromCharCode(byte);
      }
    }
  }

  private transmit(id: number, action: GuestAction): void {
    this.send(new TextEncoder().encode(`${id} ${action}\n`));
  }

  private receive(line: string): void {
    if (line === "0 ready") {
      if (!this.ready) {
        this.ready = true;
        for (const [id, request] of this.requests) this.transmit(id, request.action);
        this.onReady();
      }
      return;
    }
    const status = /^0 (desktop|network) (\S+)$/.exec(line);
    if (status) {
      const [, kind, value] = status;
      if (kind === "desktop" && ["starting", "ready", "failed", "stopped"].includes(value))
        this.report({ kind, state: value as DesktopState });
      if (kind === "network" && (value === "none" || validAddress(value)))
        this.report({ kind, address: value === "none" ? null : value });
      return;
    }
    const reply = /^(\d+) (ok|error)$/.exec(line);
    if (!reply) return;
    const id = Number(reply[1]);
    const request = this.requests.get(id);
    if (!request) return;
    this.requests.delete(id);
    clearTimeout(request.timer);
    if (reply[2] === "ok") request.resolve();
    else request.reject(new Error(`Guest could not complete ${request.action}`));
  }
}

function validAddress(value: string): boolean {
  const match = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})\/(\d{1,2})$/.exec(value);
  return !!match && match.slice(1, 5).every(n => Number(n) <= 255) && Number(match[5]) <= 32;
}
