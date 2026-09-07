export class DisplayBuffer {
  readonly width: number;
  readonly height: number;
  readonly pixels: Uint8ClampedArray<ArrayBuffer>;
  private dirty: Uint8Array;
  private count = 0;

  constructor(width: number, height: number) {
    this.width = width;
    this.height = height;
    this.pixels = new Uint8ClampedArray(width * height * 4);
    this.dirty = new Uint8Array(height);
  }

  update(rows: Uint32Array, rgba: Uint8Array | Uint8ClampedArray): void {
    const stride = this.width * 4;
    for (let i = 0; i < rows.length; i++) {
      const row = rows[i];
      this.pixels.set(rgba.subarray(i * stride, (i + 1) * stride), row * stride);
      if (!this.dirty[row]) { this.dirty[row] = 1; this.count++; }
    }
  }

  takeRows(): Uint32Array {
    const rows = new Uint32Array(this.count);
    let at = 0;
    for (let row = 0; row < this.height; row++) {
      if (this.dirty[row]) { rows[at++] = row; this.dirty[row] = 0; }
    }
    this.count = 0;
    return rows;
  }

  copyRows(rows: Uint32Array): Uint8ClampedArray<ArrayBuffer> {
    const stride = this.width * 4;
    const rgba = new Uint8ClampedArray(rows.length * stride);
    for (let i = 0; i < rows.length; i++) {
      const start = rows[i] * stride;
      rgba.set(this.pixels.subarray(start, start + stride), i * stride);
    }
    return rgba;
  }
}
