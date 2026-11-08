import type { WaterfallPalette } from '../ui/waterfallState';

// Independent ramps, informed by OpenWebRX's level/LUT separation and the dark
// noise floors in SDRoxide and OnAir. Positions reserve colour detail for weak
// signals. Classic retains the familiar colour progression by signal strength;
// the other ramps offer alternatives with increasing brightness.
type Stop = readonly [number, number, number, number];
const PALETTES: Record<WaterfallPalette, readonly Stop[]> = {
  classic: [
    [0, 0, 0, 0], [0.12, 0, 0, 48], [0.24, 0, 42, 128],
    [0.38, 0, 144, 192], [0.52, 0, 192, 96], [0.64, 96, 224, 48],
    [0.76, 240, 224, 0], [0.86, 255, 112, 0], [0.94, 240, 32, 16],
    [1, 255, 252, 238],
  ],
  blue: [
    [0, 0, 0, 0], [0.10, 5, 13, 36], [0.22, 12, 39, 91],
    [0.36, 22, 86, 147], [0.52, 35, 153, 184], [0.68, 109, 205, 192],
    [0.83, 234, 224, 128], [1, 255, 252, 238],
  ],
  viridis: [
    [0, 0, 0, 0], [0.12, 48, 18, 72], [0.28, 63, 65, 126],
    [0.45, 43, 115, 142], [0.62, 35, 159, 131],
    [0.80, 121, 202, 82], [1, 253, 231, 37],
  ],
  amber: [
    [0, 0, 0, 0], [0.18, 48, 24, 8], [0.36, 103, 52, 14],
    [0.55, 170, 94, 24], [0.73, 231, 156, 48],
    [0.88, 255, 211, 119], [1, 255, 249, 227],
  ],
};

// More than 256 entries keeps sub-dB colour differences in the 16-bit SDR history.
export const WATERFALL_COLOURS = 1024;
let cached: { palette: WaterfallPalette; lut: Uint32Array } | null = null;

/** Automatic levels reserve colour headroom above the weak-signal region.
 * A fixed soft knee avoids pumping the entire image when a strong carrier
 * appears, and keeps signals above the nominal upper level distinguishable. */
export function waterfallHeadroom(level: number): number {
  const knee = 36 / 55;
  if (level <= knee) return level;
  const excess = level - knee, headroom = 1 - knee;
  return knee + headroom * excess / (headroom + excess);
}

/** A short black toe, as in Gqrx, keeps background dark at every contrast.
 * Contrast belongs to the dB limits; it never changes palette luminance. */
export function waterfallLUT(palette: WaterfallPalette): Uint32Array {
  if (cached?.palette === palette) return cached.lut;
  const stops = PALETTES[palette] ?? PALETTES.classic;
  const lut = new Uint32Array(WATERFALL_COLOURS);
  for (let i = 0; i < lut.length; i++) {
    const level = Math.max(0, (i / (lut.length - 1) - 0.06) / 0.94);
    let j = 0;
    while (j < stops.length - 2 && level > stops[j + 1][0]) j++;
    const a = stops[j], b = stops[j + 1];
    const f = (level - a[0]) / (b[0] - a[0]);
    const r = Math.round(a[1] + (b[1] - a[1]) * f);
    const g = Math.round(a[2] + (b[2] - a[2]) * f);
    const blue = Math.round(a[3] + (b[3] - a[3]) * f);
    lut[i] = (255 << 24) | (blue << 16) | (g << 8) | r;
  }
  cached = { palette, lut };
  return lut;
}
