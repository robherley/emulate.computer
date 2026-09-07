import { test } from "node:test";
import assert from "node:assert/strict";
import { diskSeed } from "../src/session/seed.ts";

test("seed identity and URL come from the same build manifest", () => {
  const hash = "a".repeat(64);
  const disk = { hash, id: `sha256:${hash}`, logicalBytes: 512 * 1024 * 1024 };
  const url = `/guest/rootfs.${"b".repeat(64)}.ext4.gz`;
  assert.deepEqual(diskSeed({ disk, files: { "rootfs.ext4.gz": { url } } }), { ...disk, url });
});
test("inconsistent identities are rejected", () => {
  assert.throws(() => diskSeed({ disk: { hash: "bad", id: "sha256:bad", logicalBytes: 0 },
    files: { "rootfs.ext4.gz": { url: "/bad" } } }), /identity/);
});
