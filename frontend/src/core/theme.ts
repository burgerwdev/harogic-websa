import type { I18nKey } from './i18n';

// Keep the original IDs so existing browser preferences retain their appearance.
export const THEMES = [
  { id: 'dark', name: 'theme_dark', scheme: 'dark' },
  { id: 'light', name: 'theme_light', scheme: 'light' },
  { id: 'liquid-glass', name: 'theme_liquid_glass', scheme: 'light' },
  { id: 'liquid-glass-dark', name: 'theme_liquid_glass_dark', scheme: 'dark' },
  { id: 'minimalist-flat', name: 'theme_minimalist_flat', scheme: 'light' },
  { id: 'neumorphism', name: 'theme_neumorphism', scheme: 'light' },
  { id: 'mecha', name: 'theme_mecha', scheme: 'dark' },
  { id: 'pixel-anime', name: 'theme_pixel_anime', scheme: 'dark' },
  { id: 'sci-fi-hud', name: 'theme_sci_fi_hud', scheme: 'dark' },
  { id: 'brutalist-web', name: 'theme_brutalist_web', scheme: 'light' },
  { id: 'neon-tokyo', name: 'theme_neon_tokyo', scheme: 'dark' },
  { id: 'e-ink', name: 'theme_e_ink', scheme: 'light' },
] as const satisfies readonly { id: string; name: I18nKey; scheme: 'dark' | 'light' }[];
export type Theme = typeof THEMES[number]['id'];

export function isTheme(value: unknown): value is Theme {
  return THEMES.some(theme => theme.id === value);
}

let current: Theme = 'dark';
const listeners = new Set<(th: Theme) => void>();
let colors: CanvasColors | null = null;

export function getTheme(): Theme { return current; }
export function isLightTheme(): boolean { return THEMES.find(theme => theme.id === current)!.scheme === 'light'; }

export function setTheme(th: Theme) {
  current = th;
  document.documentElement.dataset.theme = th;
  colors = null;
  listeners.forEach((fn) => fn(th));
}

export function onThemeChange(fn: (th: Theme) => void) {
  listeners.add(fn);
  return () => { listeners.delete(fn); };
}

// Read a CSS variable (for canvas use; called before each render so the new value is picked up after a theme switch)
export function cssVar(name: string, fallback = ''): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

// Share one style snapshot per theme across traces and channel overlays. Reading
// computed styles for every colour on every draw can flush pending UI layout.
function readCanvasColors() {
  const style = getComputedStyle(document.documentElement);
  const cssVar = (name: string, fallback: string): string => style.getPropertyValue(name).trim() || fallback;
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
    rta: cssVar('--sa-rta', '#00ff88'),
    passband: cssVar('--sa-passband', 'rgba(0,255,160,0.24)'),
    passbandEdge: cssVar('--sa-passband-edge', 'rgba(224,242,234,0.8)'),
    passbandOutline: cssVar('--sa-passband-outline', 'rgba(0,0,0,0.45)'),
    hoverPassband: cssVar('--sa-hover-passband', 'rgba(232,121,249,0.24)'),
    hoverPassbandEdge: cssVar('--sa-hover-passband-edge', '#f0abfc'),
  };
}

export type CanvasColors = ReturnType<typeof readCanvasColors>;

export function canvasColors(): CanvasColors {
  return colors ??= readCanvasColors();
}

// Initialize the saved theme, defaulting to dark.
export function initTheme(saved?: unknown) {
  const th = isTheme(saved) ? saved : 'dark';
  setTheme(th);
  return th;
}
