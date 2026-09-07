import { Relay } from "../relay/relay.ts";
import { relay as defaults, perIp } from "../relay/config.ts";
import { RedisLimits } from "../relay/limits.ts";
import { relayError, validateUpgrade } from "../relay/deployment.ts";

const relay = new Relay({
  ...defaults, path: "/api/relay", origins: "same-origin", allowPrivate: false, trustProxy: true,
}, process.env.REDIS_URL ? new RedisLimits(perIp, process.env.REDIS_URL, 600) : {
  admit: async () => { throw new Error("REDIS_URL required"); },
  charge: () => false, release: async () => {},
});
const options = relay.serveOptions();
Bun.serve({
  ...options,
  async fetch(request, server) {
    try {
      const refused = validateUpgrade(request);
      if (refused) return refused;
      if (!process.env.REDIS_URL) return relayError(503, "Networking unavailable");
      return options.fetch(request, server);
    } catch { return relayError(503, "Networking unavailable"); }
  },
});
