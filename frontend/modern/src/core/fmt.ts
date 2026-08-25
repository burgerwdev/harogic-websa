// Formatting utilities (from the original app.js helpers)
export function formatFreqHz(hz: number): string {
  if (!isFinite(hz)) return '-';
  const a = Math.abs(hz);
  if (a >= 1e9) return (hz / 1e9).toFixed(4) + ' GHz';
  if (a >= 1e6) return (hz / 1e6).toFixed(4) + ' MHz';
  if (a >= 1e3) return (hz / 1e3).toFixed(3) + ' kHz';
  return hz.toFixed(1) + ' Hz';
}
export function fmtAxis(hz: number): string {
  if (!isFinite(hz)) return '-';
  if (hz >= 9.5e8) return Math.round(hz / 1e9) + 'G';
  if (hz >= 9.5e5) return Math.round(hz / 1e6) + 'M';
  if (hz >= 9.5e2) return Math.round(hz / 1e3) + 'k';
  return Math.round(hz) + '';
}
export function formatBWHz(hz: number): string {
  if (hz >= 1e6) return (hz / 1e6).toFixed(3) + ' MHz';
  if (hz >= 1e3) return (hz / 1e3).toFixed(1) + ' kHz';
  return hz.toFixed(1) + ' Hz';
}
export function fmtFMHz(hz: number): string { return (hz / 1e6).toFixed(3) + ' MHz'; }
export function fmtHzUnit(h: number): string {
  const a = Math.abs(h);
  if (a >= 1e6) return (h / 1e6).toFixed(3) + 'MHz';
  if (a >= 1e3) return (h / 1e3).toFixed(3) + 'kHz';
  return h.toFixed(0) + 'Hz';
}
export function fmtF(hz: number): string {
  const a = Math.abs(hz);
  if (a >= 1e9) return (hz / 1e9).toFixed(3) + 'G';
  if (a >= 1e6) return (hz / 1e6).toFixed(3) + 'M';
  if (a >= 1e3) return (hz / 1e3).toFixed(1) + 'k';
  return hz.toFixed(0);
}
export function fmtPnmFreq(f: number): string {
  if (f >= 1e6) return (f / 1e6) + 'M';
  if (f >= 1e3) return (f / 1e3) + 'k';
  return f + '';
}
