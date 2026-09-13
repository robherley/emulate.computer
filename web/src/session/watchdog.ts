const STALL_MS = 2_000;

export class RunLoopWatchdog {
  private lastIteration = 0;
  private lastCheck = 0;

  reset(now: number): void {
    this.lastIteration = now;
    this.lastCheck = now;
  }

  progressed(now: number): void {
    this.lastIteration = now;
  }

  check(now: number): "healthy" | "resume" | "stalled" {
    // If the watchdog was delayed too, there is no evidence that the emulator
    // lost a wake-up: the browser may have suspended the whole worker.
    const watchdogDelayed = now - this.lastCheck >= STALL_MS;
    this.lastCheck = now;
    if (now - this.lastIteration < STALL_MS) return "healthy";
    this.lastIteration = now;
    return watchdogDelayed ? "resume" : "stalled";
  }
}
