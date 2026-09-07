import { expect, test } from "bun:test";

function config(env: Record<string, string>): { trustProxy: boolean; allowPrivate: boolean } {
  const child = Bun.spawnSync([
    process.execPath, "--eval",
    'import { relay } from "./config.ts"; console.log(JSON.stringify(relay));',
  ], {
    cwd: import.meta.dir,
    env: { ...process.env, VERCEL: "", RELAY_TRUST_PROXY: "", RELAY_ALLOW_PRIVATE: "", ...env },
  });
  expect(child.exitCode).toBe(0);
  return JSON.parse(child.stdout.toString());
}

test("forwarded client IPs require explicit proxy trust on every host", () => {
  expect(config({}).trustProxy).toBe(false);
  expect(config({ VERCEL: "1" }).trustProxy).toBe(false);
  expect(config({ RELAY_TRUST_PROXY: "true" }).trustProxy).toBe(true);
  expect(config({ VERCEL: "1", RELAY_TRUST_PROXY: "true" }).trustProxy).toBe(true);
  expect(config({ VERCEL: "1", RELAY_TRUST_PROXY: "false" }).trustProxy).toBe(false);
});

test("private destinations require explicit opt-in", () => {
  expect(config({}).allowPrivate).toBe(false);
  expect(config({ RELAY_ALLOW_PRIVATE: "false" }).allowPrivate).toBe(false);
  expect(config({ RELAY_ALLOW_PRIVATE: "1" }).allowPrivate).toBe(false);
  expect(config({ RELAY_ALLOW_PRIVATE: "true" }).allowPrivate).toBe(true);
});
