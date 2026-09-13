// Single-line terminal indicator for guest asset downloads. Renders a dim
// carriage-return-updated status line, so it must own the cursor line: show it
// only between boot output, and clear it before other writes land.

const SHOW_DELAY_MS = 250;
const RENDER_INTERVAL_MS = 100;
const BAR_WIDTH = 12;
const SPINNER = ["|", "/", "-", "\\"];

interface DownloadEntry {
  label: string;
  loaded: number;
  total?: number;
  done: boolean;
}

export interface DownloadIndicator {
  update(
    id: string,
    label: string,
    loaded: number,
    total: number | undefined,
    done: boolean,
  ): void;
  /** Erase the line and cancel pending draws; later updates may redraw. */
  hide(): void;
}

const mb = (bytes: number): string => (bytes / (1024 * 1024)).toFixed(1);

function progressBar(
  loaded: number,
  total: number | undefined,
  marker: string,
): string {
  if (total === undefined || total <= 0) {
    return `[${marker}${".".repeat(BAR_WIDTH - 1)}]`;
  }
  const filled = Math.floor(Math.min(loaded / total, 1) * BAR_WIDTH);
  if (filled === BAR_WIDTH) return `[${"=".repeat(BAR_WIDTH)}]`;
  return `[${"=".repeat(filled)}${marker}${".".repeat(BAR_WIDTH - filled - 1)}]`;
}

export function createDownloadIndicator(
  write: (text: string) => void,
  columns: () => number = () => Infinity,
): DownloadIndicator {
  // Entries accumulate within one burst of downloads so concurrent fetches
  // aggregate; a download starting after a fully finished burst begins fresh.
  const entries = new Map<string, DownloadEntry>();
  let visible = false;
  let showTimer: ReturnType<typeof setTimeout> | null = null;
  let renderTimer: ReturnType<typeof setTimeout> | null = null;
  let lastRender = -Infinity;
  let spinnerFrame = 0;

  function allDone(): boolean {
    for (const entry of entries.values()) if (!entry.done) return false;
    return true;
  }

  function line(): string {
    let loaded = 0;
    let total = 0;
    let totalKnown = true;
    const active: string[] = [];
    for (const entry of entries.values()) {
      loaded += entry.loaded;
      if (entry.total === undefined) totalKnown = false;
      else total += entry.total;
      if (!entry.done) active.push(entry.label);
    }
    const label = active.length === 1 ? active[0] : "guest images";
    // A proxy may re-encode a download, so received bytes can pass the total.
    const percentage =
      totalKnown && total > 0
        ? `${Math.min(Math.round((loaded / total) * 100), 100)}%`
        : undefined;
    const amount = percentage
      ? `${mb(Math.min(loaded, total))}/${mb(total)} MB (${percentage})`
      : `${mb(loaded)} MB`;
    const bar = progressBar(
      loaded,
      totalKnown ? total : undefined,
      SPINNER[spinnerFrame % SPINNER.length],
    );
    const candidates = [
      `${bar} downloading ${label}: ${amount}`,
      `${bar} ${label}: ${amount}`,
      `${bar} ${amount}`,
      percentage ? `${bar} ${percentage}` : bar,
      percentage ?? amount,
    ];
    // Leave the final column unused so xterm never enters its wrapped state.
    const width = Math.max(columns() - 1, 1);
    const content =
      candidates.find((candidate) => candidate.length <= width) ??
      candidates.at(-1)!.slice(0, width);
    return `\r\x1b[K\x1b[2m${content}\x1b[0m`;
  }

  function render(): void {
    lastRender = performance.now();
    write(line());
    spinnerFrame++;
  }

  function scheduleRender(): void {
    if (!visible || renderTimer !== null) return;
    const wait = RENDER_INTERVAL_MS - (performance.now() - lastRender);
    if (wait <= 0) {
      render();
      return;
    }
    renderTimer = setTimeout(() => {
      renderTimer = null;
      render();
      scheduleRender();
    }, wait);
  }

  function hide(): void {
    if (showTimer !== null) {
      clearTimeout(showTimer);
      showTimer = null;
    }
    if (renderTimer !== null) {
      clearTimeout(renderTimer);
      renderTimer = null;
    }
    if (visible) {
      visible = false;
      // Remove both the indicator and the spacer row it reserved on first draw.
      write("\r\x1b[K\x1b[1A\r\x1b[K");
    }
  }

  return {
    update(id, label, loaded, total, done) {
      if (!entries.has(id) && entries.size > 0 && allDone()) entries.clear();
      entries.set(id, { label, loaded, total, done });
      if (allDone()) {
        hide();
        return;
      }
      if (!visible && showTimer === null) {
        showTimer = setTimeout(() => {
          showTimer = null;
          visible = true;
          // Keep one empty row between the welcome message and boot activity.
          write("\r\n");
          render();
          scheduleRender();
        }, SHOW_DELAY_MS);
      }
      scheduleRender();
    },
    hide,
  };
}
