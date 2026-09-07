import ipaddr from "ipaddr.js";

export function relayError(status: number, message: string): Response {
  return Response.json({ error: message }, { status, headers: { "Cache-Control": "private, no-store" } });
}

export function validateUpgrade(request: Request): Response | null {
  const url = new URL(request.url);
  if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket")
    return relayError(400, "WebSocket required");
  if (url.pathname !== "/api/relay")
    return relayError(404, "Unknown endpoint");
  if (url.protocol !== "https:" || request.headers.get("origin") !== url.origin)
    return relayError(403, "Origin not allowed");
  // Vercel replaces X-Forwarded-For; this entrypoint is not a standalone server.
  const ip = request.headers.get("x-forwarded-for")?.split(",")[0]?.trim();
  if (!ip || !ipaddr.isValid(ip)) return relayError(403, "Missing client IP");
  return null;
}
