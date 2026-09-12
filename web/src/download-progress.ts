// Single-line terminal indicator for guest asset downloads. Renders a dim
// carriage-return-updated status line, so it must own the cursor line: show it
// only between boot output, and clear it before other writes land.

const SHOW_DELAY_MS = 250;
const RENDER_INTERVAL_MS = 100;

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

export function createDownloadIndicator(
  write: (text: string) => void,
): DownloadIndicator {
  // Entries accumulate within one burst of downloads so concurrent fetches
  // aggregate; a download starting after a fully finished burst begins fresh.
  const entries = new Map<string, DownloadEntry>();
  let visible = false;
  let showTimer: ReturnType<typeof setTimeout> | null = null;
  let renderTimer: ReturnType<typeof setTimeout> | null = null;
  let lastRender = -Infinity;

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
    const amount =
      totalKnown && total > 0
        ? `${mb(Math.min(loaded, total))}/${mb(total)} MB (${Math.min(
            Math.round((loaded / total) * 100),
            100,
          )}%)`
        : `${mb(loaded)} MB`;
    return `\r\x1b[K\x1b[2m[downloading ${label}: ${amount}]\x1b[0m`;
  }

  function render(): void {
    lastRender = performance.now();
    write(line());
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
      write("\r\x1b[K");
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
          render();
        }, SHOW_DELAY_MS);
      }
      scheduleRender();
    },
    hide,
  };
}
