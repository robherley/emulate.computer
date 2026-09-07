import { useEffect, useRef, useState } from "react";
import type { Metric, useMetrics } from "../hooks/useMetrics";
import { InspectorPanel } from "./InspectorPanel";
import type { EmulatorStats } from "../protocol";
import {
  METRICS,
  SPARK_HEIGHT,
  drawSparkline,
  indexAt,
  type MetricSpec,
} from "../stats";

function Tile({
  spec,
  metric,
  latest,
  revision,
}: {
  spec: MetricSpec;
  metric: Metric;
  latest: EmulatorStats | null;
  revision: number;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [hover, setHover] = useState<number | null>(null);
  useEffect(() => {
    const values = metric.buffer.slice();
    if (values.length) values[values.length - 1] = metric.displayed;
    drawSparkline(canvas.current!, values);
  }, [metric, revision]);
  const value =
    hover !== null
      ? (metric.buffer[hover] ?? metric.displayed)
      : metric.displayed;
  const formatted = metric.buffer.length
    ? spec.format(value, latest)
    : { value: "-", unit: "" };
  return (
    <div className="stats-tile">
      <div className="stats-label">{spec.label}</div>
      <div className="stats-value-row">
        <span className={`stats-value${hover !== null ? " hovering" : ""}`}>
          {formatted.value}
        </span>
        <span className="stats-unit">{formatted.unit}</span>
      </div>
      <canvas
        ref={canvas}
        className="stats-spark"
        role="img"
        aria-label={`${spec.label}, last 30 seconds: ${formatted.value} ${formatted.unit}`}
        height={SPARK_HEIGHT}
        onPointerMove={(event) =>
          setHover(
            indexAt(
              event.currentTarget,
              event.clientX - event.currentTarget.getBoundingClientRect().left,
              metric.buffer.length,
            ),
          )
        }
        onPointerLeave={() => setHover(null)}
      />
    </div>
  );
}

export function StatsPanel({ stats, history }: {
  stats: EmulatorStats | null;
  history: ReturnType<typeof useMetrics>;
}) {
  const { metrics, revision } = history;
  return (
    <InspectorPanel title="Metrics">
      {(["System", "Display", "Network"] as const).map((group) => (
        <section key={group} aria-label={group}>
          <div className="stats-group-grid">
            {METRICS.map((spec, i) => spec.group === group && (
              <Tile key={spec.label} spec={spec} metric={metrics[i]}
                latest={stats} revision={revision} />
            ))}
          </div>
        </section>
      ))}
    </InspectorPanel>
  );
}
