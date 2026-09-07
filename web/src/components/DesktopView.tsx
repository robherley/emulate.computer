import type { RefObject } from "react";
import type { EmulatorClient } from "../client";
import type { SessionState } from "../session/session";
import type { DesktopState } from "../session/guest-control";
import { DisplayPanel, type FrameSink } from "./DisplayPanel";

export function DesktopView({ client, state, desktop, error, frames, viewport, visible, onOpenConsole }: {
  client: EmulatorClient | null;
  state: SessionState;
  desktop: DesktopState;
  error: string;
  frames: RefObject<FrameSink>;
  viewport: RefObject<HTMLElement | null>;
  visible: boolean;
  onOpenConsole(): void;
}) {
  const ready = state === "running" && desktop === "ready";
  const failed = state === "failed" || desktop === "failed";
  const stopped = state === "stopped";
  const desktopStopped = state === "running" && desktop === "stopped";
  const title = stopped
    ? "Powered off"
    : failed
      ? "Unable to start the desktop"
      : desktopStopped
        ? "Desktop stopped"
        : state === "stopping"
          ? "Shutting down"
          : state === "running" ? "Starting desktop" : "Starting computer";
  const detail = failed
    ? error || "Open the console for details, or restart the computer."
    : desktopStopped
      ? "Switch to the console and back to start the desktop."
      : stopped
        ? "Your session disk is kept while this tab stays open."
        : "";
  return (
    <section
      id="desktop-view"
      className="desktop-stage"
      role="tabpanel"
      aria-labelledby="desktop-tab"
      hidden={!visible}
    >
      {client && (
        <DisplayPanel
          client={client}
          frames={frames}
          viewport={viewport}
          interactive={ready && visible}
          visible={visible}
        />
      )}
      {!ready && (
        <div className="desktop-cover">
          <div>
            <span className="boot-symbol" aria-hidden="true">
              {failed ? "!" : stopped ? "○" : "▧"}
            </span>
            <h2>{title}</h2>
            {detail && <p>{detail}</p>}
            <div className="cover-actions">
              {(failed || stopped || desktopStopped) && (
                <button
                  type="button"
                  disabled={!client}
                  onClick={() => client?.power("restart")}
                >
                  {stopped ? "Power on" : "Restart computer"}
                </button>
              )}
              <button
                type="button"
                onClick={() => {
                  onOpenConsole();
                  requestAnimationFrame(() => client?.focus());
                }}
              >
                Switch to console
              </button>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}
