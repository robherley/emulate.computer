import { test } from "node:test";
import assert from "node:assert/strict";
import { BrowserDisk } from "../src/session/disk.ts";
import { Session } from "../src/session/session.ts";

test("OPFS fallback works and cleanup preserves a powered-off tab's disk lease", async (t) => {
  const fetch = globalThis.fetch;
  const storage = Object.getOwnPropertyDescriptor(navigator, "storage");
  Object.defineProperty(navigator, "storage", {
    configurable: true,
    value: {
      getDirectory: async () => {
        throw new Error("OPFS disabled");
      },
    },
  });
  globalThis.fetch = async () => new Response(new Uint8Array([1, 2, 3]));
  const owner = new BrowserDisk();
  t.after(async () => {
    await owner.dispose();
    globalThis.fetch = fetch;
    if (storage) Object.defineProperty(navigator, "storage", storage);
    else Reflect.deleteProperty(navigator, "storage");
  });
  const chunks: Uint8Array[] = [];
  const machine = {
    seed_disk_begin_volatile() {},
    seed_disk_write(chunk: Uint8Array) {
      chunks.push(chunk);
    },
    seed_disk_finish() {},
    seed_disk_abort() {
      assert.fail("fallback unexpectedly aborted");
    },
  };
  assert.equal(
    (await owner.attachDisk(machine as never, "/seed", 3)).mode,
    "memory",
  );
  assert.deepEqual([...chunks[0]], [1, 2, 3]);
  const session = new Session(owner, {
    create: async (_boot: string) => machine,
    destroy() {},
    started() {},
    pause() {},
    changed() {},
    error(error) {
      throw error;
    },
  });
  await session.boot("linux");
  await session.stop();

  const held = (await navigator.locks.query()).held!.find((lock) =>
    lock.name?.startsWith("emulate-disk:"),
  );
  assert(held, "the powered-off tab must still own its disk lease");
  const ownedName = held.name!.slice("emulate-disk:".length);
  const removed: string[] = [];
  const root = {
    async *[Symbol.asyncIterator]() {
      for (const name of [ownedName, "emulate-session-root-abandoned.img"]) {
        yield [
          name,
          {
            kind: "file",
            createSyncAccessHandle: async () => ({ close() {} }),
          },
        ];
      }
    },
    async removeEntry(name: string) {
      removed.push(name);
    },
  };
  const cleaner = new BrowserDisk();
  await cleaner.cleanupAbandonedDisks(root as never);
  assert.deepEqual(removed, ["emulate-session-root-abandoned.img"]);
  await session.dispose();
  await cleaner.cleanupAbandonedDisks(root as never);
  assert(
    removed.includes(ownedName),
    "closing the tab must release its lease for cleanup",
  );
});
