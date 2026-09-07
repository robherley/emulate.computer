// Deployment settings and resource limits.

import type { LimitConfig } from "./limits.ts";
import type { Config } from "./relay.ts";

const list = (v: string | undefined) => (v ?? "").split(",").map((s) => s.trim()).filter(Boolean);

const MiB = 1024 * 1024;
const GiB = 1024 * MiB;

export const listen = { hostname: "127.0.0.1", port: Number(process.env.PORT ?? 7654) };

/** Redis for the per-IP counters; unset keeps them in memory, per process. */
export const redisUrl = process.env.REDIS_URL;

export const relay: Config = {
  path: "/",
  // Page origins allowed to connect; empty means all (loopback dev only).
  origins: list(process.env.RELAY_ALLOW_ORIGIN),
  // Let the guest reach loopback / RFC1918 / link-local addresses.
  allowPrivate: process.env.RELAY_ALLOW_PRIVATE === "true",
  // Destination port allowlist; empty means any. [80, 443] restricts to HTTP(S).
  ports: [],
  // Per-flow byte ceiling, both directions combined.
  maxBytes: 512 * MiB,
  // Concurrent flows per process / function instance.
  maxConns: 64,
  // Drop a flow with no traffic either way for this long.
  idleTimeoutMs: 300_000,
  // After the guest's FIN, a peer silent for this long is treated as closed.
  finIdleMs: 30_000,
  // Enable only behind a proxy that replaces untrusted X-Forwarded-For headers.
  trustProxy: process.env.RELAY_TRUST_PROXY === "true",
  // Suppress the one-line-per-flow log.
  quiet: false,
};

/** Per-client-IP abuse limits, shared across instances when Redis is configured. */
export const perIp: LimitConfig = {
  maxPerIp: 16,
  bytesPerDay: GiB,
};
