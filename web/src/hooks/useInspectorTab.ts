import { useEffect, useState } from "react";
import type { InspectorTab } from "../components/Inspector";
import { readStored, writeStored } from "../storage";

export function useInspectorTab() {
  const [tab, setTab] = useState<InspectorTab | null>(() => {
    const stored = readStored("emulate.inspector");
    return stored === "network" || stored === "metrics" || stored === "settings" ? stored : null;
  });
  useEffect(() => { writeStored("emulate.inspector", tab ?? ""); }, [tab]);
  return [tab, setTab] as const;
}
