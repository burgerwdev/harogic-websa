export function niceSpanStep(span: number): number {
  if (!isFinite(span) || span <= 0) return 100;
  const raw = Math.max(100, span / 10);
  const decade = 10 ** Math.floor(Math.log10(raw));
  const normalized = raw / decade;
  const multipliers = [1, 2, 5, 10];
  const multiplier = multipliers.reduce((best, candidate) =>
    Math.abs(Math.log(normalized / candidate)) < Math.abs(Math.log(normalized / best))
      ? candidate : best);
  return multiplier * decade;
}

export function steppedSpan(
  span: number,
  step: number,
  direction: -1 | 1,
  minimumSpan: number,
  maximumSpan: number,
): number {
  if (!isFinite(span) || !isFinite(step) || step <= 0) return span;
  return Math.max(minimumSpan, Math.min(maximumSpan, span + direction * step));
}

// Ref-level stepping: the caller passes the pending target when a previous step has not
// been confirmed yet, so rapid clicks accumulate instead of repeating the same value.
export function steppedRefLevel(
  current: number,
  step: number,
  direction: -1 | 1,
  minimum = -50,
  maximum = 30,
): number {
  const base = isFinite(current) ? current : 0;
  const delta = isFinite(step) && step > 0 ? step : 1;
  return Math.max(minimum, Math.min(maximum, base + direction * delta));
}

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
  const fittedCenter = Math.max(
    minimum + minimumSpan / 2,
    Math.min(maximum - minimumSpan / 2, center),
  );
  const symmetricLimit = 2 * Math.min(fittedCenter - minimum, maximum - fittedCenter);
  const fittedSpan = Math.max(minimumSpan, Math.min(span, symmetricLimit));
  const half = fittedSpan / 2;
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
