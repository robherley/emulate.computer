import { subscribeColorMode } from "../color-mode";
import { useEffect, useRef, useState } from "react";
import type { EmulatorStats } from "../protocol";
import { HISTORY, METRICS, RATE_EMA_ALPHA, readSeriesColors, type Rates } from "../stats";

export type Metric = { buffer: number[]; from: number; displayed: number };
const zeroRates = (): Rates => ({
  displayFps: 0,
  diskBytesPerSec: 0,
  netRxBytesPerSec: 0,
  netTxBytesPerSec: 0,
  decodeHitRate: 0,
});

export function useMetrics({
  stats,
  open,
}: {
  stats: EmulatorStats | null;
  open: boolean;
}) {
  const metrics = useRef<Metric[]>(
    METRICS.map(() => ({ buffer: [], from: 0, displayed: 0 })),
  );
  const previous = useRef<{ stats: EmulatorStats; at: number } | null>(null);
  const smoothed = useRef<Rates | null>(null);
  const animation = useRef({ start: 0, duration: 100 });
  const [revision, redraw] = useState(0);
  useEffect(() => {
    if (!stats) return;
    const at = performance.now();
    const last = previous.current;
    if (last?.stats === stats) return;
    const seconds = last ? (at - last.at) / 1000 : 0;
    const delta = (value: number, before: number) =>
      seconds > 0 && value >= before ? (value - before) / seconds : 0;
    const next = zeroRates();
    if (last) {
      next.displayFps = delta(stats.displayFrames, last.stats.displayFrames);
      next.diskBytesPerSec =
        delta(stats.diskBytesRead, last.stats.diskBytesRead) +
        delta(stats.diskBytesWritten, last.stats.diskBytesWritten);
      next.netRxBytesPerSec = delta(stats.netBytesRx, last.stats.netBytesRx);
      next.netTxBytesPerSec = delta(stats.netBytesTx, last.stats.netBytesTx);
      const hits =
        (stats.decodeCacheHits ?? 0) - (last.stats.decodeCacheHits ?? 0);
      const misses =
        (stats.decodeCacheMisses ?? 0) - (last.stats.decodeCacheMisses ?? 0);
      next.decodeHitRate =
        hits >= 0 && misses >= 0 && hits + misses > 0
          ? (hits / (hits + misses)) * 100
          : 0;
    }
    for (const key of Object.keys(next) as (keyof Rates)[]) {
      if (smoothed.current && next[key] !== 0)
        next[key] =
          smoothed.current[key] +
          RATE_EMA_ALPHA * (next[key] - smoothed.current[key]);
    }
    smoothed.current = next;
    metrics.current.forEach((metric, i) => {
      const value = METRICS[i].read(stats, next);
      metric.from = metric.buffer.length ? metric.displayed : value;
      metric.buffer.push(value);
      if (metric.buffer.length > HISTORY) metric.buffer.shift();
    });
    animation.current = {
      start: at,
      duration: last ? Math.min(500, Math.max(40, at - last.at)) : 100,
    };
    previous.current = { stats, at };
  }, [stats]);
  useEffect(() => {
    let frame = 0;
    function draw() {
      const phase =
        open && !document.hidden
          ? Math.min(
              1,
              (performance.now() - animation.current.start) /
                animation.current.duration,
            )
          : 1;
      const eased =
        phase < 0.5 ? 2 * phase ** 2 : 1 - (-2 * phase + 2) ** 2 / 2;
      metrics.current.forEach((metric) => {
        metric.displayed =
          metric.from + ((metric.buffer.at(-1) ?? 0) - metric.from) * eased;
      });
      if (open && !document.hidden) redraw((n) => n + 1);
      if (phase < 1) frame = requestAnimationFrame(draw);
    }
    function refresh() {
      cancelAnimationFrame(frame);
      readSeriesColors();
      draw();
    }
    refresh();
    const unsubscribeTheme = subscribeColorMode(refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      cancelAnimationFrame(frame);
      unsubscribeTheme();
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [stats, open]);
  return { metrics: metrics.current, revision };
}

