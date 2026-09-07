import { evdevButton, evdevKey, isBrowserReserved } from "./keymap";
import { POINTER_FLUSH_MS, type DisplayInput } from "./protocol";
const ABS_MAX = 32767;

export function bindDisplayInput(
  canvas: HTMLCanvasElement,
  onInput: (event: DisplayInput) => void,
): () => void {
  const events = new AbortController();
  const { signal } = events;
  const keys = new Set<number>();
  const buttons = new Set<number>();
  function releaseInput() {
    flushPointer();
    for (const code of keys) onInput({ kind: "key", code, pressed: false });
    for (const code of buttons)
      onInput({ kind: "button", code, pressed: false });
    keys.clear();
    buttons.clear();
  }

  canvas.addEventListener(
    "blur",
    () => {
      releaseInput();
    },
    { signal },
  );
  canvas.addEventListener(
    "keydown",
    (event) => {
      if (isBrowserReserved(event)) return;
      event.preventDefault();
      // The guest handles key repeat.
      if (event.repeat) return;
      const code = evdevKey(event.code);
      if (!code) return;
      flushPointer();
      keys.add(code);
      onInput({ kind: "key", code, pressed: true });
    },
    { signal },
  );

  canvas.addEventListener(
    "keyup",
    (event) => {
      if (isBrowserReserved(event)) return;
      event.preventDefault();
      const code = evdevKey(event.code);
      if (!code) return;
      flushPointer();
      keys.delete(code);
      onInput({ kind: "key", code, pressed: false });
    },
    { signal },
  );

  // Coalesce pointer moves; flush their position before sending buttons or keys.

  let pending: { x: number; y: number } | null = null;
  let flushTimer: ReturnType<typeof setTimeout> | null = null;
  let lastPointerAt = 0;

  function flushPointer(): void {
    if (flushTimer !== null) {
      clearTimeout(flushTimer);
      flushTimer = null;
    }
    if (!pending) return;
    lastPointerAt = performance.now();
    onInput({ kind: "pointer", x: pending.x, y: pending.y });
    pending = null;
  }

  function trackPointer(event: PointerEvent): boolean {
    const box = canvas.getBoundingClientRect();
    if (box.width === 0 || box.height === 0) return false;
    const x = (event.clientX - box.left) / box.width;
    const y = (event.clientY - box.top) / box.height;
    pending = {
      x: Math.round(Math.min(Math.max(x, 0), 1) * ABS_MAX),
      y: Math.round(Math.min(Math.max(y, 0), 1) * ABS_MAX),
    };
    return true;
  }

  canvas.addEventListener(
    "pointermove",
    (event) => {
      if (!trackPointer(event)) return;
      const due = lastPointerAt + POINTER_FLUSH_MS - performance.now();
      if (due <= 0) {
        flushPointer();
      } else if (flushTimer === null) {
        flushTimer = setTimeout(flushPointer, due);
      }
    },
    { signal },
  );

  canvas.addEventListener(
    "pointerdown",
    (event) => {
      canvas.focus();
      trackPointer(event);
      flushPointer();
      const code = evdevButton(event.button);
      if (code) {
        buttons.add(code);
        onInput({ kind: "button", code, pressed: true });
      }
      canvas.setPointerCapture(event.pointerId);
      event.preventDefault();
    },
    { signal },
  );

  canvas.addEventListener(
    "pointerup",
    (event) => {
      trackPointer(event);
      flushPointer();
      const code = evdevButton(event.button);
      if (code) {
        buttons.delete(code);
        onInput({ kind: "button", code, pressed: false });
      }
      if (canvas.hasPointerCapture(event.pointerId))
        canvas.releasePointerCapture(event.pointerId);
      event.preventDefault();
    },
    { signal },
  );

  canvas.addEventListener("contextmenu", (event) => event.preventDefault(), {
    signal,
  });

  canvas.addEventListener(
    "wheel",
    (event) => {
      event.preventDefault();
      // evdev wheel detents have the opposite sign to DOM deltaY.
      const delta = event.deltaY > 0 ? -1 : event.deltaY < 0 ? 1 : 0;
      if (!delta) return;
      flushPointer();
      onInput({ kind: "wheel", delta });
    },
    { passive: false, signal },
  );

  canvas.addEventListener("pointercancel", releaseInput, { signal });
  window.addEventListener("blur", releaseInput, { signal });
  document.addEventListener(
    "visibilitychange",
    () => {
      if (document.hidden) releaseInput();
    },
    { signal },
  );
  return () => {
    releaseInput();
    events.abort();
    if (flushTimer !== null) clearTimeout(flushTimer);
  };
}
