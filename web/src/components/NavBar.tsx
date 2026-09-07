import { inspectors, type InspectorTab } from "./Inspector";
import { Icon } from "./Icon";
import { ProcessorActivity } from "./ProcessorActivity";
import type { EmulatorStats } from "../protocol";

export type ComputerView = "console" | "desktop";

export function NavBar({ view, inspector, address, netConnected, stats, running, onViewChange, onInspectorChange }: {
  view: ComputerView;
  inspector: InspectorTab | null;
  address: string | null;
  netConnected: boolean;
  stats: EmulatorStats | null;
  running: boolean;
  onViewChange(view: ComputerView): void;
  onInspectorChange(tab: InspectorTab | null): void;
}) {
  const netState = !netConnected ? "disconnected" : address ? "connected" : "pending";
  const netStatus = !netConnected
    ? "Relay disconnected"
    : address
      ? `Connected (${address})`
      : "Guest disconnected";
  return (
    <header className="app-header">
      <div className="view-tabs" role="tablist" aria-label="Computer views">
        {(["console", "desktop"] as const).map((name) => (
          <button
            key={name}
            id={`${name}-tab`}
            type="button"
            role="tab"
            aria-selected={view === name}
            aria-controls={`${name}-view`}
            tabIndex={view === name ? 0 : -1}
            onClick={() => onViewChange(name)}
            onKeyDown={(event) => {
              let next: typeof view;
              if (event.key === "Home") next = "console";
              else if (event.key === "End") next = "desktop";
              else if (event.key === "ArrowLeft" || event.key === "ArrowRight")
                next = name === "console" ? "desktop" : "console";
              else return;
              event.preventDefault();
              onViewChange(next);
              document.getElementById(`${next}-tab`)?.focus();
            }}
          >
            <Icon name={name} />
            {name === "console" ? "Console" : "Desktop"}
          </button>
        ))}
      </div>
      <nav aria-label="Computer controls">
        <ProcessorActivity stats={stats} running={running} />
        {(Object.keys(inspectors) as InspectorTab[]).map((name) => (
          <button
            key={name}
            id={`${name}-control`}
            type="button"
            className="icon-button"
            aria-label={name === "network" ? `Network: ${netStatus}` : inspectors[name]}
            title={name === "network" ? `Network: ${netStatus}` : inspectors[name]}
            aria-pressed={inspector === name}
            aria-controls="inspector"
            onClick={() =>
              onInspectorChange(inspector === name ? null : name)
            }
          >
            <Icon name={name} />
            {name === "network" && (
              <span
                className={`connection-dot ${netState}`}
                aria-hidden="true"
              />
            )}
          </button>
        ))}
      </nav>
    </header>
  );
}
