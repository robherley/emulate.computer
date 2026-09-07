import { useRef, useState } from "react";
import { browserNetConfig } from "../client";
import { useEmulator } from "../hooks/useEmulator";
import { useInspectorTab } from "../hooks/useInspectorTab";
import { NavBar, type ComputerView } from "./NavBar";
import { ConsoleView } from "./ConsoleView";
import { DesktopView } from "./DesktopView";
import { Inspector } from "./Inspector";

export function App() {
  const [view, setView] = useState<ComputerView>("console");
  const [inspector, setInspector] = useInspectorTab();
  const viewport = useRef<HTMLElement>(null);
  const machine = useEmulator(view);
  return (
    <div className="computer-app">
      <NavBar view={view} inspector={inspector} address={machine.address}
        stats={machine.stats} running={machine.state === "running"}
        netConnected={machine.netConnected} onViewChange={setView} onInspectorChange={setInspector} />
      {machine.error && <p className="app-notice" role="alert">{machine.error}</p>}
      <div className="workspace">
        <main ref={viewport} className="screen-column">
          <ConsoleView terminal={machine.terminal} client={machine.client} visible={view === "console"} />
          <DesktopView client={machine.client} state={machine.state} desktop={machine.desktop}
            error={machine.error} frames={machine.frames} viewport={viewport}
            visible={view === "desktop"} onOpenConsole={() => setView("console")} />
        </main>
        <Inspector key={machine.generation} tab={inspector} client={machine.client}
          state={machine.state} stats={machine.stats} net={browserNetConfig}
          address={machine.address} connected={machine.netConnected} busy={machine.controlBusy}
          error={machine.controlError} onToggleNetwork={machine.toggleNetwork} />
      </div>
    </div>
  );
}
