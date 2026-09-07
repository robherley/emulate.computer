export const MUX_WINDOW = 256 * 1024;
export const MUX_FRAME = 64 * 1024;
export const MUX_SOCKET_BUFFER = 1024 * 1024;

export function encodeData(id: number, data: Uint8Array): Uint8Array {
  const frame = new Uint8Array(data.length + 4);
  new DataView(frame.buffer).setUint32(0, id);
  frame.set(data, 4);
  return frame;
}

export function decodeData(frame: Uint8Array): { id: number; data: Uint8Array } {
  if (frame.length < 5 || frame.length > MUX_FRAME + 4) throw new Error("invalid data frame");
  const id = new DataView(frame.buffer, frame.byteOffset, frame.byteLength).getUint32(0);
  if (id === 0) throw new Error("invalid channel");
  return { id, data: frame.subarray(4) };
}

export function decodeControl(text: string): { id: number; command: string } {
  if (text.length > 4096) throw new Error("control frame too large");
  const match = /^([1-9][0-9]*) (.+)$/.exec(text);
  if (!match) throw new Error("invalid control frame");
  const id = Number(match[1]);
  if (!Number.isSafeInteger(id) || id > 0xffff_ffff) throw new Error("invalid channel");
  return { id, command: match[2] };
}

export function credit(command: string, prefix: string): number | null {
  if (!command.startsWith(prefix + " ")) return null;
  const value = command.slice(prefix.length + 1);
  if (!/^[1-9][0-9]*$/.test(value)) throw new Error("invalid credit");
  const bytes = Number(value);
  if (!Number.isSafeInteger(bytes) || bytes > MUX_WINDOW) throw new Error("invalid credit");
  return bytes;
}
