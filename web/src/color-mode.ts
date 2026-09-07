export type ColorMode = "light" | "dark" | "auto";

const storageKey = "emulate-color-mode";
const listeners = new Set<() => void>();
let mode: ColorMode = "auto";
let system: MediaQueryList;

function readMode(): ColorMode {
  try {
    const saved = localStorage.getItem(storageKey);
    return saved === "light" || saved === "dark" ? saved : "auto";
  } catch {
    return "auto";
  }
}

function apply() {
  document.documentElement.dataset.colorScheme =
    mode === "auto" ? (system.matches ? "dark" : "light") : mode;
  listeners.forEach((listener) => listener());
}

export function initializeColorMode() {
  system = matchMedia("(prefers-color-scheme: dark)");
  mode = readMode();
  apply();
  system.addEventListener("change", apply);
  window.addEventListener("storage", (event) => {
    if (event.key === storageKey || event.key === null) {
      mode = readMode();
      apply();
    }
  });
}

export const getColorMode = () => mode;

export function subscribeColorMode(listener: () => void) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function setColorMode(next: ColorMode) {
  mode = next;
  try {
    localStorage.setItem(storageKey, mode);
  } catch {
    // Keep the selection for this page when browser storage is unavailable.
  }
  apply();
}
