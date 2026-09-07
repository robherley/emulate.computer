export type SessionState =
  "stopped" | "starting" | "running" | "stopping" | "failed" | "disposed";

export interface SessionDisk {
  reset(): Promise<void>;
  dispose(): Promise<void>;
}

export interface SessionDriver<M, B, D extends SessionDisk> {
  create(boot: B, disk: D, allowSnapshot: boolean): Promise<M>;
  destroy(machine: M): void;
  started(machine: M): void;
  pause(): void;
  changed(state: SessionState): void;
  error(error: unknown): void;
}

/** Owns the machine and disk across serialized, last-request-wins transitions. */
export class Session<M, B, D extends SessionDisk> {
  private current: M | null = null;
  private bootImages: B | null = null;
  private phase: SessionState = "stopped";
  private sequence = 0;
  private pending: Promise<void> = Promise.resolve();
  private closing = false;

  readonly disk: D;
  private readonly driver: SessionDriver<M, B, D>;
  constructor(disk: D, driver: SessionDriver<M, B, D>) {
    this.disk = disk;
    this.driver = driver;
  }

  get state(): SessionState {
    return this.phase;
  }
  get machine(): M | null {
    return this.phase === "running" ? this.current : null;
  }

  private transition(state: SessionState): void {
    this.phase = state;
    this.driver.changed(state);
  }

  private release(): void {
    const machine = this.current;
    this.current = null;
    if (machine !== null) this.driver.destroy(machine);
  }

  private queue(
    state: SessionState,
    operation: (current: () => boolean) => Promise<void>,
  ): Promise<void> {
    if (this.closing) return this.pending;
    const sequence = ++this.sequence;
    this.driver.pause();
    this.transition(state);
    const current = () => sequence === this.sequence;
    this.pending = this.pending
      .then(async () => {
        if (!current()) return;
        await operation(current);
      })
      .catch((error: unknown) => {
        if (!current()) return;
        this.release();
        this.transition("failed");
        this.driver.error(error);
      });
    return this.pending;
  }

  boot(images: B): Promise<void> {
    this.bootImages = images;
    return this.queue("starting", (current) =>
      this.start(images, true, current),
    );
  }

  restart(): Promise<void> {
    const images = this.bootImages;
    if (images === null) return this.pending;
    return this.queue("starting", (current) =>
      this.start(images, false, current),
    );
  }

  private async start(
    images: B,
    allowSnapshot: boolean,
    current: () => boolean,
  ): Promise<void> {
    this.release();
    const candidate = await this.driver.create(
      images,
      this.disk,
      allowSnapshot,
    );
    if (!current()) {
      this.driver.destroy(candidate);
      return;
    }
    this.current = candidate;
    this.transition("running");
    this.driver.started(candidate);
  }

  stop(): Promise<void> {
    return this.queue("stopping", async () => {
      this.release();
      this.transition("stopped");
    });
  }

  reset(): Promise<void> {
    return this.queue("stopping", async (current) => {
      this.release();
      await this.disk.reset();
      if (!current()) return;
      if (this.bootImages === null) {
        this.transition("stopped");
        return;
      }
      this.transition("starting");
      await this.start(this.bootImages, true, current);
    });
  }

  fail(error: unknown): Promise<void> {
    return this.queue("failed", async () => {
      this.release();
      this.driver.error(error);
    });
  }

  dispose(): Promise<void> {
    const done = this.queue("stopping", async () => {
      this.release();
      try {
        await this.disk.dispose();
      } finally {
        this.transition("disposed");
      }
    });
    this.closing = true;
    return done;
  }
}
