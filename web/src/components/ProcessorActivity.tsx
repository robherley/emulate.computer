import { useEffect, useRef, useState } from "react";
import type { EmulatorStats } from "../protocol";
import { ProcessorActivity as Activity } from "../processor-activity";

export function ProcessorActivity({ stats, running }: {
  stats: EmulatorStats | null;
  running: boolean;
}) {
  const activity = useRef(new Activity());
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!running || !stats) {
      activity.current.reset();
      setBusy(false);
      return;
    }
    setBusy(activity.current.update(stats.executionPercent, performance.now()));
    const timeout = setTimeout(() => {
      activity.current.reset();
      setBusy(false);
    }, 1500);
    return () => clearTimeout(timeout);
  }, [stats, running]);
  const active = running && busy;
  return (
    <span className={`processor-activity${active ? " active" : ""}`}
      role="status" title={active ? "Guest processor is busy" : undefined}>
      <span className="activity-bars" aria-hidden="true"><i /><i /><i /></span>
      <span className="activity-label">{active ? "Processing" : ""}</span>
    </span>
  );
}
