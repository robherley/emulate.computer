export interface NetworkAttachment {
  port: MessagePort;
  connected: boolean;
  dispose(): void;
}

export async function connectNetwork(url: string): Promise<NetworkAttachment> {
  const lease = `emulate-network-${crypto.randomUUID()}`;
  let release = () => {};
  if (navigator.locks) {
    await new Promise<void>((resolve, reject) => {
      void navigator.locks.request(lease, async () => {
        const held = new Promise<void>(done => { release = done; });
        resolve();
        await held;
      }).catch(reject);
    });
  }
  const attach = async (shared: boolean): Promise<NetworkAttachment> => {
    const worker = shared
      ? new SharedWorker(new URL("./worker.ts", import.meta.url), { type: "module" })
      : new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
    const channel = shared ? null : new MessageChannel();
    const port = shared ? (worker as SharedWorker).port : channel!.port1;
    if (channel) (worker as Worker).postMessage({ port: channel.port2 }, [channel.port2]);
    const dispose = () => {
      release();
      port.close();
      if (!shared) (worker as Worker).terminate();
    };
    try {
      const connected = await new Promise<boolean>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("network worker did not start")), 4000);
        worker.onerror = () => { clearTimeout(timeout); reject(new Error("network worker failed")); };
        port.onmessage = event => {
          if (event.data?.type === "ready") { clearTimeout(timeout); resolve(event.data.connected); }
          else if (event.data?.type === "error") { clearTimeout(timeout); reject(new Error("invalid relay configuration")); }
        };
        port.start();
        port.postMessage({ type: "attach", url, lease: navigator.locks ? lease : null });
      });
      port.onmessage = null;
      worker.onerror = null;
      return { port, connected, dispose };
    } catch (error) {
      port.postMessage({ type: "detach" });
      port.close();
      if (!shared) (worker as Worker).terminate();
      throw error;
    }
  };
  try {
    if (typeof SharedWorker !== "undefined") {
      try { return await attach(true); } catch { /* Dedicated transport uses the same protocol. */ }
    }
    return await attach(false);
  } catch (error) { release(); throw error; }
}
