import { InspectorPanel } from "./InspectorPanel";
import type { Connection } from "../network/connections";
import type { NetConfig } from "../protocol";

export function NetworkPanel({ net, address, connected, error, busy, disabled, connections, onToggle }: {
  net: NetConfig;
  address: string | null;
  connected: boolean;
  error: string;
  busy: boolean;
  disabled: boolean;
  connections: Connection[];
  onToggle(): void;
}) {
  const status = net.mode === "none" ? "Unavailable"
    : !connected ? "Disconnected" : address ? "Connected" : "Guest disconnected";
  return (
    <InspectorPanel title="Network" action={net.mode !== "none" && (
      <button
        type="button"
        className="network-switch"
        role="switch"
        aria-label="Guest network connection"
        aria-checked={Boolean(address)}
        aria-busy={busy}
        disabled={disabled || busy}
        onClick={onToggle}
      >
        <span aria-hidden="true" />
      </button>
    )}>
      <section aria-label="Network">
        <dl className="network-details">
          <div><dt>Status</dt><dd role="status">{status}</dd></div>
          <div><dt>IP address</dt><dd>{address ?? "—"}</dd></div>
        </dl>
        {error && <p role="alert">{error}</p>}
      </section>
      <section aria-label="Recent connections">
        <h3>Recent connections</h3>
        {connections.length ? (
          <ul className="network-connections">
            {connections.map(connection => (
              <li key={connection.id}>
                <span className="connection-destination">{connection.destination}</span>
                <span className="connection-state" data-state={connection.state} title={connection.error}>
                  {connection.state}
                </span>
              </li>
            ))}
          </ul>
        ) : <p>No TCP connections yet.</p>}
      </section>
    </InspectorPanel>
  );
}
