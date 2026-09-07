import type { EmulatorClient } from "../client";
import type { EmulatorStats, NetConfig } from "../protocol";
import type { SessionState } from "../session/session";
import { NetworkPanel } from "./NetworkPanel";
import { SettingsPanel } from "./SettingsPanel";
import { StatsPanel } from "./StatsPanel";
import { useMetrics } from "../hooks/useMetrics";

export const inspectors = {
  network: "Network",
  metrics: "Metrics",
  settings: "Settings",
} as const;
export type InspectorTab = keyof typeof inspectors;

type Props = {
  tab: InspectorTab | null;
  client: EmulatorClient | null;
  state: SessionState;
  stats: EmulatorStats | null;
  net: NetConfig;
  address: string | null;
  connected: boolean;
  busy: boolean;
  error: string;
  onToggleNetwork(): void;
};

export function Inspector({ tab, client, state, stats, net, address, connected, busy, error, onToggleNetwork }: Props) {
  const history = useMetrics({ stats, open: tab === "metrics" });
  function panel() {
    switch (tab) {
      case "network":
        return <NetworkPanel net={net} address={address} connected={connected} busy={busy}
          disabled={state !== "running"} connections={stats?.networkConnections ?? []} error={error} onToggle={onToggleNetwork} />;
      case "metrics":
        return <StatsPanel stats={stats} history={history} />;
      case "settings":
        return <SettingsPanel client={client} state={state} />;
      case null:
        return null;
    }
  }
  return (
    <aside id="inspector" hidden={!tab} aria-labelledby={tab ? "inspector-title" : undefined}>
      {panel()}
    </aside>
  );
}
