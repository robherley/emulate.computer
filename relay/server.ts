import * as config from "./config.ts";
import { MemoryLimits, RedisLimits, type Limits } from "./limits.ts";
import { Relay } from "./relay.ts";

const cfg = config.relay;

if (cfg.origins.length === 0) {
  cfg.origins = ["*"];
  console.error("[relay] warning: RELAY_ALLOW_ORIGIN unset, accepting every page origin");
}

let limits: Limits;
if (config.redisUrl) {
  limits = new RedisLimits(config.perIp, config.redisUrl, cfg.idleTimeoutMs / 1000 + 1800);
} else {
  limits = new MemoryLimits(config.perIp);
  console.error("[relay] warning: REDIS_URL unset, per-IP limits are per instance only");
}

const relay = new Relay(cfg, limits);
const server = Bun.serve({ ...config.listen, ...relay.serveOptions() });

console.error(`[relay] listening on ws://${server.hostname}:${server.port}/`);
const shutdown = () => {
  relay.stop();
  server.stop(true);
  process.exit(0);
};
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
