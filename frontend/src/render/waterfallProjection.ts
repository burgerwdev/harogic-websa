/** One viewport of immutable level projections; colour changes reuse these codes. */
export class WaterfallProjectionCache {
  private rows = new WeakMap<Uint16Array, Uint16Array>();
  private order: { row: WeakRef<Uint16Array>; pixels: Uint16Array }[] = [];
  private width = 0;
  private lo = NaN;
  private hi = NaN;
  bytes = 0;

  constructor(private readonly budget = 4 * 1024 * 1024) {}

  get(row: Uint16Array, width: number, lo: number, hi: number): Uint16Array | undefined {
    if (this.width !== width || this.lo !== lo || this.hi !== hi) {
      this.clear(); this.width = width; this.lo = lo; this.hi = hi;
    }
    return this.rows.get(row);
  }

  put(row: Uint16Array, pixels: Uint16Array): void {
    // Sparse/zoomed rows are cheaper to project than to retain at display width.
    if (pixels.byteLength >= row.byteLength || pixels.byteLength > this.budget || this.rows.has(row)) return;
    const saved = pixels.slice();
    while (this.bytes + saved.byteLength > this.budget) {
      const old = this.order.shift()!;
      this.bytes -= old.pixels.byteLength;
      const key = old.row.deref();
      if (key) this.rows.delete(key);
    }
    this.rows.set(row, saved);
    this.order.push({ row: new WeakRef(row), pixels: saved });
    this.bytes += saved.byteLength;
  }

  clear(): void {
    this.rows = new WeakMap(); this.order = []; this.bytes = 0;
  }
}
