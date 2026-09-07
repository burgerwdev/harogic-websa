export interface FrequencyWindow {
  center: number;
  span: number;
  start: number;
  stop: number;
}

export function normalizeCenterSpan(
  center: number,
  span: number,
  minimum: number,
  maximum: number,
  minimumSpan = 100,
): FrequencyWindow | null {
  if (!isFinite(center) || !isFinite(span) || maximum <= minimum || span <= 0) return null;
  const fittedSpan = Math.max(minimumSpan, Math.min(span, maximum - minimum));
  const half = fittedSpan / 2;
  const fittedCenter = Math.max(minimum + half, Math.min(maximum - half, center));
  return {
    center: fittedCenter,
    span: fittedSpan,
    start: fittedCenter - half,
    stop: fittedCenter + half,
  };
}

export function normalizeStartStop(
  start: number,
  stop: number,
  minimum: number,
  maximum: number,
  minimumSpan = 100,
): FrequencyWindow | null {
  if (!isFinite(start) || !isFinite(stop) || maximum <= minimum) return null;
  const fittedStart = Math.max(minimum, Math.min(maximum, start));
  const fittedStop = Math.max(minimum, Math.min(maximum, stop));
  if (fittedStop - fittedStart < minimumSpan) return null;
  return {
    center: (fittedStart + fittedStop) / 2,
    span: fittedStop - fittedStart,
    start: fittedStart,
    stop: fittedStop,
  };
}
