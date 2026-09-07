// Per-client-IP concurrency and daily byte limits, stored in memory or Redis.

import { RedisClient } from "bun";

export interface LimitConfig {
  /** Concurrent flows per IP; 0 disables. */
  maxPerIp: number;
  /** Bytes per IP per UTC day, both directions, all flows; 0 disables. */
  bytesPerDay: number;
}

/** Why a handshake was refused, and when retrying might work. */
export interface Refusal {
  reason: "concurrency" | "quota";
  retryAfterSeconds: number;
}

export interface Limits {
  /** Take a flow slot for ip, or explain the refusal. release() every success. */
  admit(ip: string): Promise<Refusal | null>;
  release(ip: string): Promise<void>;
  /**
   * Charge n bytes before forwarding; false refuses the chunk. The caller waits
   * for asynchronous stores and bounds any traffic queued in the meantime.
   */
  charge(ip: string, n: number): boolean | Promise<boolean>;
}

const DAY = 86400;

/** Seconds until the fixed window containing `nowSeconds` ends. */
function untilWindowEnd(nowSeconds: number, span: number): number {
  return Math.max(1, span - (Math.floor(nowSeconds) % span));
}

/** Byte counter reset at UTC midnight. */
class DailyWindow {
  dayStart = 0;
  day = 0;

  roll(now: number) {
    const d = Math.floor(now / DAY);
    if (d !== this.dayStart) {
      this.dayStart = d;
      this.day = 0;
    }
  }
}

export class MemoryLimits implements Limits {
  private live = new Map<string, number>();
  private windows = new Map<string, DailyWindow>();
  /** Overridable clock, in seconds. */
  now: () => number = () => Date.now() / 1000;

  constructor(private cfg: LimitConfig) {}

  private exhausted(ip: string): Refusal | null {
    const w = this.windows.get(ip);
    if (!w) return null;
    const now = this.now();
    w.roll(now);
    if (this.cfg.bytesPerDay > 0 && w.day >= this.cfg.bytesPerDay)
      return { reason: "quota", retryAfterSeconds: untilWindowEnd(now, DAY) };
    return null;
  }

  async admit(ip: string): Promise<Refusal | null> {
    const live = this.live.get(ip) ?? 0;
    if (this.cfg.maxPerIp > 0 && live >= this.cfg.maxPerIp)
      return { reason: "concurrency", retryAfterSeconds: 1 };
    const over = this.exhausted(ip);
    if (over) return over;
    this.live.set(ip, live + 1);
    return null;
  }

  async release(ip: string): Promise<void> {
    const live = this.live.get(ip) ?? 0;
    if (live <= 1) this.live.delete(ip);
    else this.live.set(ip, live - 1);
  }

  charge(ip: string, n: number): boolean {
    let w = this.windows.get(ip);
    if (!w) {
      w = new DailyWindow();
      this.windows.set(ip, w);
    }
    w.roll(this.now());
    w.day += n;
    return this.exhausted(ip) === null;
  }

  /** Drop counters that can no longer refuse anything. */
  sweep() {
    const now = this.now();
    for (const [ip, w] of this.windows) {
      w.roll(now);
      if (w.day === 0 && !this.live.has(ip)) this.windows.delete(ip);
    }
  }
}

/**
 * Redis-backed limits. Keys:
 *   relay:live:<ip>          concurrent flows (TTL as a leak guard)
 *   relay:d:<ip>:<day>       bytes today
 * Store errors refuse admission and forwarding; release failures are logged.
 */
export class RedisLimits implements Limits {
  private redis: RedisClient;
  private warned = false;
  now: () => number = () => Date.now() / 1000;

  constructor(
    private cfg: LimitConfig,
    url: string,
    /** Longest a flow can live, so a lost release() cannot pin a slot forever. */
    private slotTtlSeconds: number,
  ) {
    this.redis = new RedisClient(url, { connectionTimeout: 2000, autoReconnect: false, enableOfflineQueue: false });
  }

  private keys(ip: string, now: number) {
    return {
      live: `relay:live:${ip}`,
      day: `relay:d:${ip}:${Math.floor(now / DAY)}`,
    };
  }

  private unavailable(err: unknown) {
    if (!this.warned) {
      this.warned = true;
      console.error(`[relay] redis unavailable: ${err}`);
    }
  }

  async admit(ip: string): Promise<Refusal | null> {
    const now = this.now();
    const k = this.keys(ip, now);
    try {
      if (!this.redis.connected) await this.redis.connect();
      const day = await this.redis.get(k.day);
      if (this.cfg.bytesPerDay > 0 && Number(day ?? 0) >= this.cfg.bytesPerDay)
        return { reason: "quota", retryAfterSeconds: untilWindowEnd(now, DAY) };
      const live = await this.redis.incr(k.live);
      await this.redis.expire(k.live, this.slotTtlSeconds);
      if (this.cfg.maxPerIp > 0 && live > this.cfg.maxPerIp) {
        await this.redis.decr(k.live);
        return { reason: "concurrency", retryAfterSeconds: 1 };
      }
      return null;
    } catch (err) {
      this.unavailable(err);
      throw err;
    }
  }

  async release(ip: string): Promise<void> {
    try {
      if (!this.redis.connected) await this.redis.connect();
      const k = this.keys(ip, this.now());
      if ((await this.redis.decr(k.live)) < 0) await this.redis.del(k.live);
    } catch (err) {
      this.unavailable(err);
    }
  }

  async charge(ip: string, n: number): Promise<boolean> {
    const now = this.now();
    const k = this.keys(ip, now);
    try {
      if (!this.redis.connected) await this.redis.connect();
      const day = await this.redis.incrby(k.day, n);
      // First writer in a window sets its expiry; an extra EXPIRE is harmless.
      if (day === n) await this.redis.expire(k.day, DAY);
      if (this.cfg.bytesPerDay > 0 && day >= this.cfg.bytesPerDay) return false;
      return true;
    } catch (err) {
      this.unavailable(err);
      return false;
    }
  }

  close() {
    this.redis.close();
  }
}

/** Parse a byte count with an optional K/M/G/T suffix (binary units). */
export function parseSize(s: string): number {
  const m = /^\s*(\d+)\s*([kmgtKMGT]?)\s*$/.exec(s);
  if (!m) throw new Error(`bad size ${JSON.stringify(s)}`);
  const shift = { "": 0, k: 10, m: 20, g: 30, t: 40 }[m[2].toLowerCase()]!;
  const n = Number(m[1]) * 2 ** shift;
  if (!Number.isSafeInteger(n)) throw new Error(`size ${JSON.stringify(s)} overflows`);
  return n;
}
