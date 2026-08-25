// Theme system: dark/light, CSS variables + canvas color reading
export type Theme = 'dark' | 'light';

let current: Theme = 'dark';
const listeners = new Set<(th: Theme) => void>();

export function getTheme(): Theme { return current; }

export function setTheme(th: Theme) {
  current = th;
  document.documentElement.dataset.theme = th;
  listeners.forEach((fn) => fn(th));
}

export function toggleTheme(): Theme {
  const th = current === 'dark' ? 'light' : 'dark';
  setTheme(th);
  return th;
}

export function onThemeChange(fn: (th: Theme) => void) {
  listeners.add(fn);
}

// Read a CSS variable (for canvas use; called before each render so the new value is picked up after a theme switch)
export function cssVar(name: string, fallback = ''): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

// Canvas color set (resolved from CSS variables by theme)
export function canvasColors() {
  return {
    bg: cssVar('--sa-bg', '#000'),
    grid: cssVar('--sa-grid', '#003300'),
    axis: cssVar('--sa-axis', '#005500'),
    text: cssVar('--sa-text', '#00ff00'),
    label: cssVar('--sa-axis', '#00aa00'),
    band: cssVar('--sa-band', 'rgba(60,140,90,0.10)'),
    traces: [
      cssVar('--sa-trace1', '#00ff00'),
      cssVar('--sa-trace2', '#ffff00'),
      cssVar('--sa-trace3', '#00ffff'),
      cssVar('--sa-trace4', '#ff00ff'),
    ],
    markerColors: [
      cssVar('--sa-marker1', '#ff4444'),
      cssVar('--sa-marker2', '#ffd700'),
      cssVar('--sa-marker3', '#1e90ff'),
      cssVar('--sa-marker4', '#ffffff'),
    ],
    marker: cssVar('--sa-marker', '#ff4d4d'),
    markerFill: cssVar('--sa-marker-fill', '#ff4d4d'),
    dim: cssVar('--sa-dim', '#ffa04d'),
    peak: cssVar('--sa-peak', '#00bbff'),
    harm: cssVar('--sa-harm', '#0088ff'),
    pnm: cssVar('--sa-pnm', '#00ff88'),
    cross: cssVar('--sa-cross', '#ff8b5e'),
    ref: cssVar('--sa-ref', '#00ff00'),
    norm: cssVar('--sa-norm', '#ffd75e'),
    warning: cssVar('--sa-warning', '#ff4d4d'),
// Label/backplate (in light mode switches to a light background with dark text, without affecting readability)
    labelBg: cssVar('--sa-label-bg', 'rgba(0,0,0,0.7)'),
    labelBorder: cssVar('--sa-label-border', '#00aa66'),
    osdStroke: cssVar('--sa-osd-stroke', 'rgba(0,0,0,0.9)'),
    osdFill: cssVar('--sa-osd-fill', '#00ff00'),
  };
}

export type CanvasColors = ReturnType<typeof canvasColors>;

// Theme initialization (default dark, consistent with legacy)
export function initTheme(saved?: Theme | null) {
  const th = saved || 'dark';
  setTheme(th);
  return th;
}
