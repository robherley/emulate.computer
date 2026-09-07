import { describe, expect, test } from "bun:test";
import { MemoryLimits, RedisLimits, parseSize } from "./limits.ts";

function limits(maxPerIp: number, bytesPerDay: number) {
  const l = new MemoryLimits({ maxPerIp, bytesPerDay });
  const clock = { at: 1_699_920_000 + 600 }; // 00:10:00 UTC
  l.now = () => clock.at;
  return { l, clock };
}

describe("MemoryLimits", () => {
  test("daily quota survives hour changes and resets at UTC midnight", async () => {
    const { l, clock } = limits(0, 100);
    expect(await l.admit("1.2.3.4")).toBeNull();
    expect(l.charge("1.2.3.4", 60)).toBe(true);
    clock.at += 3600;
    expect(l.charge("1.2.3.4", 40)).toBe(false);
    await l.release("1.2.3.4");
    expect(await l.admit("5.6.7.8")).toBeNull();
    expect(await l.admit("1.2.3.4")).toEqual({ reason: "quota", retryAfterSeconds: 82200 });

    clock.at += 82199;
    expect(await l.admit("1.2.3.4")).toEqual({ reason: "quota", retryAfterSeconds: 1 });
    clock.at++;
    expect(await l.admit("1.2.3.4")).toBeNull();
    expect(l.charge("1.2.3.4", 99)).toBe(true);
    expect(l.charge("1.2.3.4", 1)).toBe(false);
  });

  test("zero disables the daily quota", async () => {
    const { l } = limits(0, 0);
    expect(l.charge("1.2.3.4", 2 ** 40)).toBe(true);
    expect(await l.admit("1.2.3.4")).toBeNull();
  });

  test("concurrency cap and release", async () => {
    const { l } = limits(2, 0);
    expect(await l.admit("1.2.3.4")).toBeNull();
    expect(await l.admit("1.2.3.4")).toBeNull();
    expect((await l.admit("1.2.3.4"))?.reason).toBe("concurrency");
    await l.release("1.2.3.4");
    expect(await l.admit("1.2.3.4")).toBeNull();
    for (let i = 0; i < 5; i++) await l.release("1.2.3.4"); // no underflow
    expect(await l.admit("1.2.3.4")).toBeNull();
  });

  test("sweep drops expired counters", async () => {
    const { l, clock } = limits(0, 1000);
    l.charge("1.2.3.4", 10);
    clock.at += 3600;
    l.sweep();
    expect(l.charge("1.2.3.4", 0)).toBe(true);
    clock.at += 86400;
    l.sweep();
    // @ts-expect-error private
    expect(l.windows.size).toBe(0);
  });
});

describe("RedisLimits", () => {
  const url = process.env.REDIS_URL;
  test.skipIf(!url)("counts across instances", async () => {
    const ip = `test-${Date.now()}`;
    const a = new RedisLimits({ maxPerIp: 1, bytesPerDay: 100 }, url!, 60);
    const b = new RedisLimits({ maxPerIp: 1, bytesPerDay: 100 }, url!, 60);
    expect(await a.admit(ip)).toBeNull();
    expect((await b.admit(ip))?.reason).toBe("concurrency");
    await a.release(ip);
    expect(await b.charge(ip, 60)).toBe(true);
    expect(await a.charge(ip, 60)).toBe(false);
    expect((await b.admit(ip))?.reason).toBe("quota");
    a.close();
    b.close();
  });
});

test("parseSize", () => {
  expect(parseSize("0")).toBe(0);
  expect(parseSize("512")).toBe(512);
  expect(parseSize("4K")).toBe(4096);
  expect(parseSize("1M")).toBe(1 << 20);
  expect(parseSize("1g")).toBe(2 ** 30);
  expect(parseSize(" 2T ")).toBe(2 * 2 ** 40);
  for (const bad of ["", "-1", "1X", "G", "99999999999999999G"]) expect(() => parseSize(bad)).toThrow();
});

test("Redis outage refuses new flows and byte forwarding", async () => {
  const l = new RedisLimits({ maxPerIp: 1, bytesPerDay: 100 }, "redis://127.0.0.1:1", 60);
  l.close();
  await expect(l.admit("203.0.113.10")).rejects.toThrow();
  expect(await l.charge("203.0.113.10", 10)).toBe(false);
});
