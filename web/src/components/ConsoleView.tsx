import { useEffect, type RefObject } from "react";
import type { EmulatorClient } from "../client";

export function ConsoleView({ terminal, client, visible }: {
  terminal: RefObject<HTMLDivElement | null>;
  client: EmulatorClient | null;
  visible: boolean;
}) {
  useEffect(() => {
    if (visible && document.activeElement === document.body) client?.focus();
  }, [visible, client]);
  return (
    <section
      id="console-view"
      className="console-stage"
      role="tabpanel"
      aria-labelledby="console-tab"
      hidden={!visible}
    >
      <div id="terminal" ref={terminal} />
    </section>
  );
}
