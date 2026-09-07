export class ProcessorActivity {
  private busy = false;
  private pendingSince: number | null = null;
  private lastSample: number | null = null;

  reset(): void {
    this.busy = false;
    this.pendingSince = this.lastSample = null;
  }

  update(executionPercent: number | undefined, now: number): boolean {
    if (executionPercent === undefined || !Number.isFinite(executionPercent)) {
      this.reset();
      return false;
    }
    if (this.lastSample !== null && now - this.lastSample > 1500) this.reset();
    this.lastSample = now;
    // Separate thresholds and delays avoid flashing on short bursts or brief pauses.
    const active = executionPercent >= (this.busy ? 30 : 60);
    if (active === this.busy) {
      this.pendingSince = null;
    } else {
      this.pendingSince ??= now;
      if (now - this.pendingSince >= (this.busy ? 700 : 300)) {
        this.busy = active;
        this.pendingSince = null;
      }
    }
    return this.busy;
  }
}
