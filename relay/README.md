# relay

The Bun relay opens TCP sockets and resolves DNS for guests. Browsers share a
persistent multiplexed WebSocket across same-origin tabs with the same relay
URL. The native CLI shares one multiplexed socket per machine.

## Protocol

Clients connect directly to the configured URL: `/` locally or `/api/relay` on
Vercel, without a protocol suffix or version query. Each TCP connection or DNS
lookup has its own channel:

```
client → TEXT   id CONNECT host:port       server → TEXT  id OK | id ERR reason
client → BINARY id + payload …             server → BINARY id + payload …
client → TEXT   id FIN                     server → TEXT  id EOF
client → TEXT   id RESOLVE name            server → TEXT  id IP a.b.c.d … | id ERR nx
```

Socket failures keep their error codes, for example `ERR ECONNREFUSED` or
`ERR ETIMEDOUT`; errors without a code use `ERR EUNKNOWN`.

Text controls have a positive
32-bit channel ID prefix (`1 CONNECT example.com:443`, `1 OK`). Binary frames
start with that ID as four big-endian bytes, followed by 1–65536 payload bytes.
IDs increase when channels start and cannot be reused during a connection.
`id CLOSE` terminates a channel in either direction; server EOF and queued data
precede CLOSE. Connection-level `PING` receives `PONG`.

Each direction starts with 256 KiB of credit per channel. The relay sends
`id ACK bytes` as TCP writes succeed. Each client sends `id WINDOW bytes` as
its emulator polls received data. A stalled guest pauses its TCP reads while
other channels continue. Outstanding uploads are capped at 1 MiB per browser
tab or native transport; relay queued payloads are capped at 1 MiB per WebSocket. Excess traffic closes
the offending channel; malformed framing closes the WebSocket.

The SharedWorker maps each tab's local IDs to wire IDs. Closing or restarting a
guest only closes its channels. The last tab releases the shared connection.
Web Locks detect closed tabs; environments without them use heartbeat expiry.
If SharedWorker cannot start, a dedicated worker uses one multiplexed socket
for that tab. No guest memory, disks, or terminal data are shared.

A dropped WebSocket resets every channel it carries. The broker reconnects with
backoff (1 s to 30 s); new guest requests work after reconnect. Existing TCP
streams are never replayed or resumed. The CLI reconnects on the next guest
request after a socket failure.

## Run locally

```sh
npm ci                      # from the repository root
cd relay
bun run server.ts            # ws://127.0.0.1:7654/ (PORT overrides; 0 picks a free port)
bun test
```

Vite development defaults to this address; override it with build-time `VITE_RELAY_URL`.
Production uses the same-origin `/api/relay` endpoint. Without `REDIS_URL`
the per-IP limits live in this one process.

## Configuration

Standalone local-server environment variables (the Vercel entrypoints fix the
origin and proxy policy and require `REDIS_URL`):

| Variable | Default | Meaning |
| --- | --- | --- |
| `RELAY_ALLOW_PRIVATE` | `false` | Explicit local-development opt-in for private destinations and the gateway-to-loopback alias. |
| `RELAY_ALLOW_ORIGIN` | `*` (warns) | Allowed page origins, comma-separated. Others → 403. With an allowlist set, a missing `Origin` is refused too. |
| `REDIS_URL` | *(none)* | Redis for the per-IP counters. Unset: per-process counters (warns). |
| `RELAY_TRUST_PROXY` | `false` | Set to `true` only behind a trusted proxy that replaces `X-Forwarded-For`; uses its first address for client quotas. |
| `PORT` | `7654` | Local listening port; `0` selects a free port. |

Other limits are configured in `config.ts`:

| Setting | Value |
| --- | --- |
| per-IP concurrent flows | 16 |
| per-IP bytes per UTC day | 1 GiB, both directions, all flows |
| per-flow byte cap | 512 MiB |
| concurrent flows per process | 64 |
| idle timeout | 300 s without traffic either way |
| post-FIN idle | 30 s (see below) |
| destination ports | any (`ports: [80, 443]` restricts to HTTP(S)) |
| private destinations | refused (`RELAY_ALLOW_PRIVATE=true` for local experiments) |

WebSocket session caps are refused with HTTP 429. Per-IP flow limits, exhausted
byte quotas, and process flow caps refuse new channels with `ERR connection limit`
and `CLOSE`. A flow
that exhausts its IP's quota mid-stream is closed at that point. The daily
window resets at UTC midnight. Each chunk is charged before forwarding,
including when Redis answers asynchronously. Only one charge per flow is
pending at a time.

Connection *rate* limiting is left to Vercel's Firewall, which applies to the
upgrade request like any other HTTP request.

## Guardrails

`RELAY_ALLOW_ORIGIN` limits which websites can use the relay from a browser.
It is not authentication: non-browser clients can forge the `Origin` header.
This is a public relay; connection limits, destination restrictions, daily byte
quotas, and deployment-level rate limits bound abuse. A shared token shipped
in the public frontend would be visible to every visitor.

An unguarded relay is an open proxy. By default it refuses loopback, RFC1918,
link-local, CGNAT, multicast, documentation and reserved destinations, judged
on the **resolved** address rather than the name, so a public name pointing at
`127.0.0.1` is still refused. [ipaddr.js](https://github.com/whitequark/ipaddr.js)
parses and classifies addresses, normalizing IPv4-mapped IPv6 before checking
the range. Only ordinary unicast is allowed by default; malformed addresses
are refused even with `allowPrivate` enabled.

The guest gateway `10.0.2.2` is also refused by default. With
`RELAY_ALLOW_PRIVATE=true`, it maps to `127.0.0.1` for local development and
guest integration tests. Do not enable private destinations on a public relay.

If Redis is unreachable the relay logs once and refuses admission and forwarding.
Disconnected stores do not queue operations for replay.

## FIN is not a half-close here

The Bun relay does not currently forward TCP half-closes: `socket.shutdown(true)`, `socket.end()`
and `node:net`'s `end()` all reset the connection and discard whatever the
server was still sending, and `allowHalfOpen` misfires its `end` handler in a
loop. So this relay keeps the socket open after `FIN`, forwards `EOF` when the
server closes, and ends a flow whose server has been silent for 30 seconds
after the FIN (`finIdleMs`). HTTP is unaffected; a
protocol whose server waits for the client's EOF before replying will wait
those 30 seconds instead.

## Deploy on Vercel

Deploy from the **repository root**, alongside the Vite frontend. The root
`vercel.json` configures static output, cache headers, and the Bun function.
`api/relay.ts` requires a same-origin HTTPS WebSocket upgrade and only exposes
the multiplexed transport. It fixes private destinations off and trusts
Vercel's replacement forwarding header; local-server environment flags cannot
weaken this entrypoint.

Set `REDIS_URL` for each deployment environment to enforce shared quotas.
The WebSocket function's maximum duration is 300 seconds, so the broker
reconnects when it ends; existing TCP streams cannot survive that reconnect.
Keep Fluid compute enabled.

See the root [build instructions](../README.md#build-and-deploy) and
[deployment guide](../docs/networking.md#vercel-deployment) for asset releases,
configuration, security boundaries, and live preview verification. A direct
local `server.ts` run does not exercise Vercel.

Pending quota checks and TCP uploads share a 1 MiB per-flow queue limit and
an 8 MiB process limit. Checks time out after 10 seconds. Channel credit and
WebSocket backpressure pause TCP reads; Bun closes sockets that exceed its
1 MiB send-buffer limit.
