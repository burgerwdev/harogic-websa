// Global UI scale: one number that sizes the whole interface, canvas included.
//
// UI scale preserves readable proportions as viewport height grows. It multiplies fonts,
// spacing, control sizes and the canvas CSS box. The canvas backing store tracks UI scale
// and device pixel ratio so the bitmap remains sharp.
//
// How it is applied: `zoom` on the frame. That is the only mechanism that scales a stylesheet
// full of px values - borders, paddings, fixed control widths, popovers - without rewriting
// every declaration, and Chromium re-rasterises zoomed text and zoomed canvases at the zoomed
// resolution instead of stretching a bitmap. Two consequences are handled here and in the
// store:
//   * zoom scales the element's own box too, so the frame divides its viewport-relative
//     width/height by the scale to stay exactly one viewport (style.css .analyzer-card);
//   * a canvas inside a zoomed subtree occupies `scale` times more device pixels than its
//     local clientWidth suggests, so the backing-store ratio is multiplied by the scale
//     (core/store.ts backingRatio) - otherwise the plot would be soft at any scale but 1.
//
// Stored under 'web-sa-ui-scale' like the theme/language. With nothing stored the first visit
// picks the auto value for the screen; a manual pick is remembered across reloads.
export const UI_SCALE_KEY = 'web-sa-ui-scale';

/** The offered steps: below 1 to fit more on a small panel, above 1 for large/high-res ones. */
export const UI_SCALES = [0.5, 0.65, 0.8, 1, 1.25, 1.5, 2] as const;

/** Sentinel for the scale selector: follow the screen again instead of a stored value. */
export const UI_SCALE_AUTO = 'auto';

let current = 1;
const listeners = new Set<(scale: number) => void>();

/**
 * The value a first visit starts from, from the viewport height alone.
 *
 * Height, not width, because that is what fixes how large a fixed-size label looks relative to
 * the screen, and because the fluid columns already absorb extra width. A device pixel ratio
 * of 2 or more already doubles the physical size of a css px (a 4K panel at 200% reports a
 * 1080-high viewport), so what matters is the height the page is actually laid out against.
 * The steps preserve readable label proportions as viewport height grows.
 */
export function autoScale(height = window.innerHeight): number {
  if (height >= 1900) return 2;     // 2160-high viewport: 4K at 100%, or 5K/6K
  if (height >= 1250) return 1.5;    // 1440-high viewport
  if (height >= 1000) return 1.25;   // 1080-high viewport
  return 1;                          // 768 / 900 (and anything the compositor already scaled)
}

export function getUiScale(): number { return current; }

/** A stored value is only honoured when it is one of the offered steps. */
export function parseStoredScale(raw: string | null): number | null {
  if (raw === null) return null;
  const value = Number(raw);
  return (UI_SCALES as readonly number[]).includes(value) ? value : null;
}

/** The scale a first visit should use, given what localStorage holds. */
export function initialScale(stored: string | null, height = window.innerHeight): number {
  return parseStoredScale(stored) ?? autoScale(height);
}

export function setUiScale(scale: number) {
  current = scale;
  const root = document.documentElement;
  root.style.setProperty('--ui', String(scale));
  // Read by the e2e/harness and the store's own tests; the CSS variable alone would be
  // invisible to a screenshot-based check.
  root.dataset.uiScale = String(scale);
  listeners.forEach((fn) => fn(scale));
}

export function onUiScaleChange(fn: (scale: number) => void) {
  listeners.add(fn);
}

/** Apply a scale and remember it. `UI_SCALE_AUTO` forgets the override instead. */
export function chooseUiScale(value: string): number {
  if (value === UI_SCALE_AUTO) {
    localStorage.removeItem(UI_SCALE_KEY);
    const scale = autoScale();
    setUiScale(scale);
    return scale;
  }
  const scale = parseStoredScale(value) ?? autoScale();
  localStorage.setItem(UI_SCALE_KEY, String(scale));
  setUiScale(scale);
  return scale;
}

/** Initialise from localStorage (auto when absent) and bind the footer scale control. */
let uiScaleBinding: AbortController | null = null;
export function initUiScale(saved: string | null = localStorage.getItem(UI_SCALE_KEY)): number {
  const scale = initialScale(saved);
  setUiScale(scale);
  const shown = saved !== null && parseStoredScale(saved) !== null ? String(scale) : UI_SCALE_AUTO;
  // A repeated init (tests, a rebuilt frame) must not stack another listener on the select.
  uiScaleBinding?.abort();
  uiScaleBinding = new AbortController();
  const signal = uiScaleBinding.signal;
  const select = document.querySelector<HTMLSelectElement>('#ui-scale');
  if (select) {
    select.value = shown;
    select.addEventListener('change', () => {
      const applied = chooseUiScale(select.value);
      select.value = select.value !== UI_SCALE_AUTO && parseStoredScale(select.value) !== applied
        ? UI_SCALE_AUTO : select.value === UI_SCALE_AUTO ? UI_SCALE_AUTO : String(applied);
    }, { signal });
  }
  return scale;
}
