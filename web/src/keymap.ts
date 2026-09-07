// Physical keys map to linux/input-event-codes.h; the guest applies its layout.

export const EVDEV_KEYS: Readonly<Record<string, number>> = {
  Escape: 1,
  F1: 59,
  F2: 60,
  F3: 61,
  F4: 62,
  F5: 63,
  F6: 64,
  F7: 65,
  F8: 66,
  F9: 67,
  F10: 68,
  F11: 87,
  F12: 88,

  Backquote: 41,
  Digit1: 2,
  Digit2: 3,
  Digit3: 4,
  Digit4: 5,
  Digit5: 6,
  Digit6: 7,
  Digit7: 8,
  Digit8: 9,
  Digit9: 10,
  Digit0: 11,
  Minus: 12,
  Equal: 13,
  Backspace: 14,

  Tab: 15,
  KeyQ: 16,
  KeyW: 17,
  KeyE: 18,
  KeyR: 19,
  KeyT: 20,
  KeyY: 21,
  KeyU: 22,
  KeyI: 23,
  KeyO: 24,
  KeyP: 25,
  BracketLeft: 26,
  BracketRight: 27,
  Backslash: 43,

  CapsLock: 58,
  KeyA: 30,
  KeyS: 31,
  KeyD: 32,
  KeyF: 33,
  KeyG: 34,
  KeyH: 35,
  KeyJ: 36,
  KeyK: 37,
  KeyL: 38,
  Semicolon: 39,
  Quote: 40,
  Enter: 28,

  ShiftLeft: 42,
  KeyZ: 44,
  KeyX: 45,
  KeyC: 46,
  KeyV: 47,
  KeyB: 48,
  KeyN: 49,
  KeyM: 50,
  Comma: 51,
  Period: 52,
  Slash: 53,
  ShiftRight: 54,
  IntlBackslash: 86,

  ControlLeft: 29,
  MetaLeft: 125,
  AltLeft: 56,
  Space: 57,
  AltRight: 100,
  MetaRight: 126,
  ContextMenu: 127,
  ControlRight: 97,

  Insert: 110,
  Delete: 111,
  Home: 102,
  End: 107,
  PageUp: 104,
  PageDown: 109,
  ArrowUp: 103,
  ArrowLeft: 105,
  ArrowRight: 106,
  ArrowDown: 108,

  PrintScreen: 99,
  ScrollLock: 70,
  Pause: 119,

  NumLock: 69,
  NumpadDivide: 98,
  NumpadMultiply: 55,
  NumpadSubtract: 74,
  NumpadAdd: 78,
  NumpadEnter: 96,
  NumpadDecimal: 83,
  Numpad0: 82,
  Numpad1: 79,
  Numpad2: 80,
  Numpad3: 81,
  Numpad4: 75,
  Numpad5: 76,
  Numpad6: 77,
  Numpad7: 71,
  Numpad8: 72,
  Numpad9: 73,
};

export function evdevKey(code: string): number {
  return EVDEV_KEYS[code] ?? 0;
}

export const EVDEV_BUTTONS: readonly number[] = [0x110, 0x112, 0x111];

// DOM button order is left/middle/right; evdev is left/right/middle.
export function evdevButton(button: number): number {
  return EVDEV_BUTTONS[button] ?? 0;
}

// Let the browser handle shortcuts that preventDefault cannot intercept.
export function isBrowserReserved(event: KeyboardEvent): boolean {
  if (event.metaKey) return true;
  if (event.ctrlKey && ["KeyW", "KeyT", "KeyN"].includes(event.code)) {
    return true;
  }
  return ["F5", "F11", "F12"].includes(event.code);
}
