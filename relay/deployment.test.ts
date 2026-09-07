import { test, expect } from "bun:test";
import { validateUpgrade } from "./deployment.ts";

function request(path = "/api/relay", headers: Record<string, string> = {}) {
  return new Request(`https://emulate.computer${path}`, {
    headers: { upgrade: "websocket", origin: "https://emulate.computer", "x-forwarded-for": "203.0.113.4", ...headers },
  });
}
test("deployed relay accepts same-origin upgrades", () => {
  expect(validateUpgrade(request())).toBeNull();
});
test("deployed relay rejects foreign origins, invalid IPs, and legacy endpoints", () => {
  for (const origin of ["", "null", "https://evil.example", "https://emulate.computer/"])
    expect(validateUpgrade(request(undefined, { origin }))?.status).toBe(403);
  expect(validateUpgrade(request(undefined, { "x-forwarded-for": "invalid" }))?.status).toBe(403);
  expect(validateUpgrade(request(undefined, { upgrade: "" }))?.status).toBe(400);
  for (const path of ["/api/relay/t", "/t", "/api/relay/m", "/api/relay/m?v=1"])
    expect(validateUpgrade(request(path))?.status).toBe(404);
  expect(validateUpgrade(request("/t"))?.headers.get("cache-control")).toBe("private, no-store");
});
