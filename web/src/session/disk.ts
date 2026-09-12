import type { WasmMachine } from "../wasm/emulate_wasm";
import { countBytes, gunzipIfNeeded } from "./streams.ts";

const SESSION_DISK_PREFIX = "emulate-session-root-";
const SESSION_DISK_NAME = `${SESSION_DISK_PREFIX}${crypto.randomUUID()}.img`;
const errorMessage = (error: unknown): string =>
  error instanceof Error ? error.message : String(error);
type IterableDirectory = FileSystemDirectoryHandle &
  AsyncIterable<[string, FileSystemHandle]>;
export interface DiskAttachmentStatus {
  mode: "ephemeral" | "memory";
  reason?: string;
}

export type DownloadProgress = (loaded: number, done: boolean) => void;

export class BrowserDisk {
  private readonly seedProgress?: DownloadProgress;
  constructor(seedProgress?: DownloadProgress) {
    this.seedProgress = seedProgress;
  }
  private diskFile: FileSystemFileHandle | null = null;
  private diskClean = false;
  private diskWriteBaseline = 0;
  private abandonedDisksCleaned = false;
  private releaseLease: (() => void) | null = null;

  private async protectSession(): Promise<void> {
    if (this.releaseLease || !navigator.locks) return;
    let acquired!: () => void;
    let failed!: (error: unknown) => void;
    const ready = new Promise<void>((resolve, reject) => {
      acquired = resolve;
      failed = reject;
    });
    // The tab owns this lease even while its machine is powered off.
    void navigator.locks
      .request(`emulate-disk:${SESSION_DISK_NAME}`, async () => {
        const released = new Promise<void>((resolve) => {
          this.releaseLease = resolve;
        });
        acquired();
        await released;
      })
      .catch(failed);
    await ready;
  }

  get clean(): boolean {
    return this.diskClean;
  }
  started(machine: WasmMachine): void {
    this.diskWriteBaseline = machine.disk_bytes_written();
  }
  async reset(): Promise<void> {
    this.diskFile = null;
    this.diskClean = false;
    let root: FileSystemDirectoryHandle;
    try {
      root = await navigator.storage.getDirectory();
    } catch {
      return;
    }
    await this.removeIfPresent(root, SESSION_DISK_NAME);
  }
  async dispose(): Promise<void> {
    try {
      await this.reset();
    } catch {
    } finally {
      this.releaseLease?.();
      this.releaseLease = null;
    }
  }
  isNotFound(error: unknown): boolean {
    return error instanceof DOMException && error.name === "NotFoundError";
  }

  async removeIfPresent(
    root: FileSystemDirectoryHandle,
    name: string,
  ): Promise<void> {
    try {
      await root.removeEntry(name);
    } catch (error) {
      if (!this.isNotFound(error)) throw error;
    }
  }

  // Snapshot overlay writes are host-side; only guest writes invalidate the seed baseline.
  watchDiskWrites(target: WasmMachine): void {
    if (!this.diskClean) return;
    const written = target.disk_bytes_written();
    if (written > this.diskWriteBaseline) this.diskClean = false;
  }

  // Stream one chunk at a time to bound memory while seeding the disk.
  async streamSeed(target: WasmMachine, seedUrl: string): Promise<void> {
    const response = await fetch(seedUrl);
    if (!response.ok || response.body === null) {
      throw new Error(
        `disk seed ${seedUrl} is unavailable (HTTP ${response.status})`,
      );
    }
    let loaded = 0;
    this.seedProgress?.(0, false);
    try {
      const counted = countBytes(response.body, (bytes) => {
        loaded = bytes;
        this.seedProgress?.(bytes, false);
      });
      const reader = (await gunzipIfNeeded(counted)).getReader();
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        target.seed_disk_write(value);
      }
    } finally {
      this.seedProgress?.(loaded, true);
    }
  }

  async seedSessionDisk(
    target: WasmMachine,
    root: FileSystemDirectoryHandle,
    seedUrl: string,
    logicalBytes: number,
  ): Promise<FileSystemFileHandle> {
    await this.removeIfPresent(root, SESSION_DISK_NAME);
    const file = await root.getFileHandle(SESSION_DISK_NAME, { create: true });
    let syncHandle: FileSystemSyncAccessHandle | null = null;
    try {
      syncHandle = await file.createSyncAccessHandle();
      target.seed_disk_begin_opfs(syncHandle, logicalBytes);
      syncHandle = null;
      await this.streamSeed(target, seedUrl);
      target.seed_disk_finish();
      this.diskClean = true;
      return file;
    } catch (error) {
      syncHandle?.close();
      target.seed_disk_abort();
      try {
        await this.removeIfPresent(root, SESSION_DISK_NAME);
      } catch {
        // Preserve the seed failure; the next visit cleans the partial file.
      }
      throw error;
    }
  }

  managedDiskName(name: string): boolean {
    return name.startsWith(SESSION_DISK_PREFIX) || name.startsWith("alpine-3.");
  }

  async cleanupAbandonedDisks(root: FileSystemDirectoryHandle): Promise<void> {
    // Without tab leases, an unlocked file may belong to a powered-off tab.
    if (!navigator.locks) return;
    for await (const [name, entry] of root as IterableDirectory) {
      if (entry.kind !== "file" || !this.managedDiskName(name)) continue;
      await navigator.locks.request(
        `emulate-disk:${name}`,
        { ifAvailable: true },
        async (lease) => {
          if (!lease) return;
          const file = entry as FileSystemFileHandle;
          let probe: FileSystemSyncAccessHandle;
          try {
            probe = await file.createSyncAccessHandle();
          } catch {
            return;
          }
          probe.close();
          try {
            await this.removeIfPresent(root, name);
          } catch {}
        },
      );
    }
  }

  async attachSessionDisk(
    target: WasmMachine,
    root: FileSystemDirectoryHandle,
    seedUrl: string,
    logicalBytes: number,
  ): Promise<void> {
    if (!this.abandonedDisksCleaned) {
      this.abandonedDisksCleaned = true;
      await this.cleanupAbandonedDisks(root);
    }

    let file = this.diskFile;
    for (let attempt = 0; attempt < 2; attempt += 1) {
      if (!file) {
        file = await this.seedSessionDisk(target, root, seedUrl, logicalBytes);
      }
      let syncHandle: FileSystemSyncAccessHandle | null = null;
      try {
        syncHandle = await file.createSyncAccessHandle();
        target.set_disk_opfs(syncHandle, logicalBytes);
        syncHandle = null;
        this.diskFile = file;
        return;
      } catch (error) {
        syncHandle?.close();
        this.diskFile = null;
        await this.removeIfPresent(root, SESSION_DISK_NAME);
        file = null;
        if (attempt === 1) throw error;
      }
    }
  }

  async attachDisk(
    target: WasmMachine,
    seedUrl: string,
    logicalBytes: number,
  ): Promise<DiskAttachmentStatus> {
    try {
      await this.protectSession();
      const root = await navigator.storage.getDirectory();
      await this.attachSessionDisk(target, root, seedUrl, logicalBytes);
      return { mode: "ephemeral" };
    } catch (error) {
      const reason = errorMessage(error);
      try {
        target.seed_disk_begin_volatile(logicalBytes);
        await this.streamSeed(target, seedUrl);
        target.seed_disk_finish();
        this.diskClean = true;
      } catch (fallbackError) {
        target.seed_disk_abort();
        throw new Error(
          `${reason}; the volatile fallback also failed: ${errorMessage(fallbackError)}`,
        );
      }
      return { mode: "memory", reason };
    }
  }
}
