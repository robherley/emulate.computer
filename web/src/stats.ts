import type { EmulatorStats } from "./protocol";

/** Samples retained per metric (~100 ms cadence => ~30 s of history). */
export const HISTORY = 300;
// Smooth rate spikes before they influence the chart scale.
export const RATE_EMA_ALPHA = 0.3;
const SERIES_FALLBACK = "#3987e5";
const SERIES_FILL_FALLBACK = "rgba(57, 135, 229, 0.16)";

let series = SERIES_FALLBACK;
let seriesFill = SERIES_FILL_FALLBACK;

export function readSeriesColors(): void {
  const styles = getComputedStyle(document.documentElement);
  series = styles.getPropertyValue("--chart-series").trim() || SERIES_FALLBACK;
  seriesFill =
    styles.getPropertyValue("--chart-fill").trim() || SERIES_FILL_FALLBACK;
}

export const SPARK_HEIGHT = 24;

type Formatted = { value: string; unit: string };

export interface MetricSpec {
  label: string;
  group: "System" | "Display" | "Network";
  read: (stats: EmulatorStats, rates: Rates) => number;
  format: (value: number, latest: EmulatorStats | null) => Formatted;
}

export interface Rates {
  displayFps: number;
  diskBytesPerSec: number;
  netRxBytesPerSec: number;
  netTxBytesPerSec: number;
  decodeHitRate: number;
}

function round(value: number): string {
  if (!Number.isFinite(value)) return "0";
  if (value >= 100) return value.toFixed(0);
  if (value >= 10) return value.toFixed(1);
  return value.toFixed(value >= 1 ? 1 : 2);
}

function formatRate(bytesPerSec: number): Formatted {
  const bytes = Math.max(0, bytesPerSec);
  if (bytes >= 1e6) return { value: round(bytes / 1e6), unit: "MB/s" };
  if (bytes >= 1e3) return { value: round(bytes / 1e3), unit: "KB/s" };
  return { value: bytes.toFixed(0), unit: "B/s" };
}

export const METRICS: MetricSpec[] = [
  {
    group: "System",
    label: "CPU",
    read: (stats) => stats.mips,
    format: (value) => ({ value: round(value), unit: "MIPS" }),
  },
  {
    group: "System",
    label: "Execution",
    read: (stats) => stats.executionPercent ?? 0,
    format: (value) => ({ value: round(value), unit: "% host time" }),
  },
  {
    group: "System",
    label: "RAM touched",
    read: (stats) => stats.ramTouchedBytes / (1024 * 1024),
    format: (value, latest) => {
      const total = (latest?.ramBytes ?? 0) / (1024 * 1024);
      return {
        value:
          total > 0
            ? `${value.toFixed(0)} / ${total.toFixed(0)}`
            : value.toFixed(0),
        unit: "MiB",
      };
    },
  },
  {
    group: "System",
    label: "Wasm alloc",
    read: (stats) => stats.memoryBytes / (1024 * 1024),
    format: (value) => ({ value: round(value), unit: "MiB" }),
  },
  {
    group: "System",
    label: "Decode cache",
    read: (_stats, rates) => rates.decodeHitRate,
    format: (value) => ({ value: round(value), unit: "% hit" }),
  },
  {
    group: "System",
    label: "Disk I/O",
    read: (_stats, rates) => rates.diskBytesPerSec,
    format: formatRate,
  },
  {
    group: "Display",
    label: "Display",
    read: (_stats, rates) => rates.displayFps,
    format: (value) => ({ value: round(value), unit: "FPS" }),
  },
  {
    group: "Display",
    label: "Frame p95",
    read: (stats) => stats.displayFrameP95 ?? 0,
    format: (value) => ({ value: round(value), unit: "ms" }),
  },
  {
    group: "Display",
    label: "Pixel transfer",
    read: (stats) => stats.displayUploadBytesPerSec ?? 0,
    format: formatRate,
  },
  {
    group: "Network",
    label: "Net RX",
    read: (_stats, rates) => rates.netRxBytesPerSec,
    format: formatRate,
  },
  {
    group: "Network",
    label: "Net TX",
    read: (_stats, rates) => rates.netTxBytesPerSec,
    format: formatRate,
  },
];

interface Point {
  x: number;
  /** Value space here; converted to canvas y once the scale is known. */
  v: number;
}

// Keep each pixel column’s extrema in time order so downsampling preserves peaks.
function buildPoints(values: number[], width: number): Point[] {
  const length = values.length;
  if (length === 0) return [];
  const step = width / (HISTORY - 1);
  const xAt = (index: number): number => width - (length - 1 - index) * step;
  const newest = values[length - 1]!;
  if (length === 1) return [{ x: width, v: newest }];
  if (step >= 1) return values.map((v, index) => ({ x: xAt(index), v }));

  const points: Point[] = [];
  let column = Math.floor(xAt(0));
  let minIndex = 0;
  let maxIndex = 0;
  const flush = (): void => {
    const first = Math.min(minIndex, maxIndex);
    const last = Math.max(minIndex, maxIndex);
    if (first === last) {
      points.push({ x: column + 0.5, v: values[first]! });
    } else {
      points.push({ x: column + 0.25, v: values[first]! });
      points.push({ x: column + 0.75, v: values[last]! });
    }
  };
  for (let index = 1; index < length; index++) {
    const at = Math.floor(xAt(index));
    if (at !== column) {
      flush();
      column = at;
      minIndex = index;
      maxIndex = index;
      continue;
    }
    if (values[index]! < values[minIndex]!) minIndex = index;
    if (values[index]! > values[maxIndex]!) maxIndex = index;
  }
  flush();

  while (points.length > 0 && points[points.length - 1]!.x >= width - 0.5)
    points.pop();
  points.push({ x: width, v: newest });
  return points;
}

// Fritsch-Carlson monotone interpolation avoids inventing values between samples.
function traceMonotone(
  ctx: CanvasRenderingContext2D,
  points: { x: number; y: number }[],
): void {
  const n = points.length;
  if (n === 0) return;
  ctx.moveTo(points[0]!.x, points[0]!.y);
  if (n === 1) return;

  const dx: number[] = [];
  const secant: number[] = [];
  for (let i = 0; i < n - 1; i++) {
    const run = points[i + 1]!.x - points[i]!.x;
    dx.push(run);
    secant.push(run === 0 ? 0 : (points[i + 1]!.y - points[i]!.y) / run);
  }

  const slope: number[] = new Array<number>(n);
  slope[0] = secant[0]!;
  slope[n - 1] = secant[n - 2]!;
  for (let i = 1; i < n - 1; i++) {
    const before = secant[i - 1]!;
    const after = secant[i]!;
    if (before * after <= 0) {
      slope[i] = 0;
    } else {
      const w1 = 2 * dx[i]! + dx[i - 1]!;
      const w2 = dx[i]! + 2 * dx[i - 1]!;
      slope[i] = (w1 + w2) / (w1 / before + w2 / after);
    }
  }

  for (let i = 0; i < n - 1; i++) {
    const h = dx[i]!;
    ctx.bezierCurveTo(
      points[i]!.x + h / 3,
      points[i]!.y + (slope[i]! * h) / 3,
      points[i + 1]!.x - h / 3,
      points[i + 1]!.y - (slope[i + 1]! * h) / 3,
      points[i + 1]!.x,
      points[i + 1]!.y,
    );
  }
}

export function drawSparkline(
  canvas: HTMLCanvasElement,
  samples: number[],
): void {
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const dpr = window.devicePixelRatio || 1;
  const width = canvas.clientWidth || 120;
  const height = SPARK_HEIGHT;
  const pixelWidth = Math.max(1, Math.round(width * dpr));
  const pixelHeight = Math.max(1, Math.round(height * dpr));
  if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
    canvas.width = pixelWidth;
    canvas.height = pixelHeight;
  }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);
  if (samples.length === 0) return;

  let lo = Infinity;
  let hi = -Infinity;
  for (const value of samples) {
    if (value < lo) lo = value;
    if (value > hi) hi = value;
  }
  if (hi === lo) {
    lo = 0;
    hi = hi === 0 ? 1 : hi * 1.25;
  } else {
    const pad = (hi - lo) * 0.12;
    lo = Math.max(0, lo - pad);
    hi += pad;
  }

  hi = Math.max(hi, 1);

  const pad = 4;
  const plotTop = pad;
  const plotBottom = height - pad;
  const yAt = (value: number): number =>
    plotBottom - ((value - lo) / (hi - lo)) * (plotBottom - plotTop);

  const points = buildPoints(samples, width).map((point) => ({
    x: point.x,
    y: yAt(point.v),
  }));
  if (points.length === 0) return;

  if (points.length > 1) {
    ctx.save();
    ctx.beginPath();
    traceMonotone(ctx, points);
    ctx.lineTo(points[points.length - 1]!.x, height);
    ctx.lineTo(points[0]!.x, height);
    ctx.closePath();
    ctx.fillStyle = seriesFill;
    ctx.fill();
    ctx.restore();
  }

  ctx.beginPath();
  traceMonotone(ctx, points);
  ctx.lineWidth = 2;
  ctx.lineJoin = "round";
  ctx.lineCap = "round";
  ctx.strokeStyle = series;
  ctx.stroke();

  const last = points[points.length - 1]!;
  ctx.beginPath();
  ctx.arc(last.x, last.y, 2.5, 0, Math.PI * 2);
  ctx.fillStyle = series;
  ctx.fill();
}

export function indexAt(
  canvas: HTMLCanvasElement,
  x: number,
  length: number,
): number | null {
  if (length === 0) return null;
  const width = canvas.clientWidth || 120;
  const step = width / (HISTORY - 1);
  const fromRight = Math.round((width - x) / step);
  const index = length - 1 - fromRight;
  if (index < 0 || index > length - 1) return null;
  return index;
}
