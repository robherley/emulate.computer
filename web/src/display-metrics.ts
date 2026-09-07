function p95(samples: number[]): number {
  if (!samples.length) return 0;
  const sorted = [...samples].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil(sorted.length * 0.95) - 1)] ?? 0;
}

export class DisplayMetrics {
  private intervals: number[] = [];
  private lastPaint = 0;
  private execution = 0;
  private copy = 0;
  private upload = 0;
  private since = performance.now();
  private slices: number[] = [];
  private inputQueue: number[] = [];
  private inputToGuest: number[] = [];
  private diagnostics = false;

  reset(): void {
    this.intervals = [];
    this.lastPaint = 0;
    this.execution = this.copy = this.upload = 0;
    this.since = performance.now();
    this.slices = [];
    this.inputQueue = [];
    this.inputToGuest = [];
    this.diagnostics = false;
  }

  executed(ms: number): void {
    this.execution += ms;
    if (this.diagnostics) {
      this.slices.push(ms);
      if (this.slices.length > 120) this.slices.shift();
    }
  }
  inputReceived(ms: number): void {
    this.diagnostics = true;
    this.inputQueue.push(Math.max(0, ms));
    if (this.inputQueue.length > 120) this.inputQueue.shift();
  }
  inputInjected(ms: number): void {
    this.inputToGuest.push(Math.max(0, ms));
    if (this.inputToGuest.length > 120) this.inputToGuest.shift();
  }
  copied(ms: number): void { this.copy += ms; }
  uploaded(bytes: number): void { this.upload += bytes; }
  painted(now = performance.now()): void {
    const elapsed = now - this.lastPaint;
    // Idle time is not a missed animation frame.
    if (this.lastPaint && elapsed < 250) {
      this.intervals.push(elapsed);
      if (this.intervals.length > 120) this.intervals.shift();
    }
    if (elapsed >= 250) this.intervals = [];
    this.lastPaint = now;
  }

  sample(now = performance.now()) {
    const elapsed = Math.max(1, now - this.since);
    const result = {
      workerSliceP95: p95(this.slices),
      inputQueueP95: p95(this.inputQueue),
      inputToGuestP95: p95(this.inputToGuest),
      displayFrameP95: now - this.lastPaint < 250 ? p95(this.intervals) : 0,
      executionPercent: this.execution / elapsed * 100,
      displayCopyMs: this.copy,
      displayUploadBytesPerSec: this.upload / elapsed * 1000,
    };
    this.execution = this.copy = this.upload = 0;
    this.since = now;
    return result;
  }
}
