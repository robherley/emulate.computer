import { useEffect, useRef, useState } from "react";
import { createClient, type EmulatorClient } from "../client";
import type { EmulatorStats } from "../protocol";
import type { SessionState } from "../session/session";
import type { DesktopState } from "../session/guest-control";
import type { FrameSink } from "../components/DisplayPanel";
import type { ComputerView } from "../components/NavBar";

export function useEmulator(view: ComputerView) {
  const terminal = useRef<HTMLDivElement>(null);
  const frames = useRef<FrameSink>(() => {});
  const [client, setClient] = useState<EmulatorClient | null>(null);
  const [state, setState] = useState<SessionState>("starting");
  const [desktop, setDesktop] = useState<DesktopState>("stopped");
  const desktopRequested = useRef(false);
  const [controlBusy, setControlBusy] = useState(false);
  const [controlError, setControlError] = useState("");
  const [netConnected, setNetConnected] = useState(false);
  const [address, setAddress] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [stats, setStats] = useState<EmulatorStats | null>(null);
  const [generation, setGeneration] = useState(0);
  useEffect(() => {
    const current = createClient(terminal.current!, {
      state: (next) => {
        setState(next);
        if (next === "starting") {
          setDesktop("stopped");
          desktopRequested.current = false;
          setControlError("");
          setAddress(null);
          setError("");
          setStats(null);
          setGeneration((n) => n + 1);
        }
      },
      guest: (status) =>
        status.kind === "desktop"
          ? setDesktop(status.state)
          : setAddress(status.address),
      network: setNetConnected,
      error: setError,
      stats: setStats,
      frame: (rows, rgba, width, height) => frames.current(rows, rgba, width, height),
    });
    setClient(current);
    return () => current.dispose();
  }, []);
  useEffect(() => {
    if (view === "console") { desktopRequested.current = false; return; }
    if (state !== "running" || !client || desktopRequested.current || desktop === "ready") return;
    desktopRequested.current = true;
    setDesktop("starting");
    let cancelled = false;
    void client.control("desktop.start").catch(error => {
      if (!cancelled && error?.name !== "AbortError") { setDesktop("failed"); setError(String(error)); }
    });
    return () => { cancelled = true; };
  }, [view, state, client, generation]);

  async function toggleNetwork() {
    if (!client || controlBusy) return;
    setControlBusy(true);
    setControlError("");
    try { await client.control(address ? "network.disconnect" : "network.connect"); }
    catch (error) { if (!(error instanceof Error && error.name === "AbortError")) setControlError(String(error)); }
    finally { setControlBusy(false); }
  }
  const guestAddress = state === "running" ? address : null;
  return {
    terminal, frames, client, state, desktop, error, stats, generation,
    address: guestAddress, netConnected, controlBusy, controlError, toggleNetwork,
  };
}
