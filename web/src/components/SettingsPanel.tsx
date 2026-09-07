import { getColorMode, setColorMode, subscribeColorMode } from "../color-mode";
import { useEffect, useState, useSyncExternalStore } from "react";
import { InspectorPanel } from "./InspectorPanel";
import type { EmulatorClient } from "../client";
import type { SessionState } from "../session/session";

export function SettingsPanel({
  client,
  state,
}: {
  client: EmulatorClient | null;
  state: SessionState;
}) {
  const colorMode = useSyncExternalStore(subscribeColorMode, getColorMode);
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), 4000);
    return () => clearTimeout(timer);
  }, [armed]);
  function act(action: "restart" | "reset-disk") {
    setArmed(false);
    client?.power(action);
  }
  return (
    <InspectorPanel title="Settings">
      <section aria-labelledby="color-mode-title">
        <h3 id="color-mode-title">Color mode</h3>
        <div className="color-mode" role="radiogroup" aria-labelledby="color-mode-title">
          {(["light", "dark", "auto"] as const).map((mode) => (
            <label key={mode}>
              <input type="radio" name="color-mode" value={mode}
                checked={colorMode === mode} onChange={() => setColorMode(mode)} />
              <span>{mode[0].toUpperCase() + mode.slice(1)}</span>
            </label>
          ))}
        </div>
      </section>
      <section aria-label="Power">
        <h3>Power</h3>
        <div className="power-body">
          <button
            type="button"
            className="power-action"
            disabled={!client || state === "starting" || state === "stopping"}
            onClick={() => act("restart")}
          >
            <div className="power-action-name">
              {state === "stopped" ? "Power on" : "Restart"}
            </div>
            <div className="power-action-desc">
              {state === "stopped" ? "Start the machine with your current disk." : "Fully reboot the machine, keeping your saved files."}
            </div>
          </button>
          <button
            type="button"
            className={`power-action danger${armed ? " armed" : ""}`}
            disabled={!client || state === "starting" || state === "stopping"}
            onClick={() => (armed ? act("reset-disk") : setArmed(true))}
          >
            <div className="power-action-name">
              {armed ? "Click again to erase session" : "Reset"}
            </div>
            <div className="power-action-desc">
              Erase the disk and start fresh.
            </div>
          </button>
        </div>
      </section>
      <section aria-label="Source code">
        <h3>Source code</h3>
        <a className="source-link" href="https://github.com/robherley/emulate.computer" target="_blank" rel="noopener noreferrer">
          <code>robherley/emulate.computer</code>
        </a>
      </section>
    </InspectorPanel>
  );
}
