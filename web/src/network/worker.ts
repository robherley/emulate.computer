/// <reference lib="webworker" />
import { RelayBroker, relayUrl } from "./broker";

const brokers = new Map<string, RelayBroker>();
function accept(port: MessagePort) {
  port.onmessage = event => {
    if (event.data?.type !== "attach") return;
    try {
      const url = relayUrl(event.data.url);
      let broker = brokers.get(url);
      if (!broker) {
        broker = new RelayBroker(url, () => brokers.delete(url));
        brokers.set(url, broker);
      }
      const lease = event.data.lease;
      const leased = typeof lease === "string" && !!navigator.locks;
      const detach = broker.attach(port, leased);
      if (leased) void navigator.locks.request(lease, () => detach());
    } catch { port.postMessage({ type: "error" }); port.close(); }
  };
  port.start();
}
const context = self as unknown as DedicatedWorkerGlobalScope & SharedWorkerGlobalScope;
if ("onconnect" in self) context.onconnect = event => accept(event.ports[0]);
else context.onmessage = event => { if (event.data?.port) accept(event.data.port); };
