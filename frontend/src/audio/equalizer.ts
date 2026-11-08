// SPDX-License-Identifier: LicenseRef-Harogic-WebSA
// Local speaker EQ; source PCM, recording and decoder paths are untouched.
export const EQ_BANDS = [80, 250, 1000, 4000, 12000] as const;
export interface EqualizerSettings { enabled: boolean; gains: number[]; trim: number }
const storageKey = 'websa-speaker-eq-v1';
export const equalizerDefaults = (): EqualizerSettings => ({ enabled: true, gains: EQ_BANDS.map(() => 0), trim: 0 });
export const EQ_PRESETS: Record<string, number[]> = {
  flat: [0, 0, 0, 0, 0], voice: [-6, -2, 2, 3, -4], warm: [3, 2, 0, -1, -3], bright: [-2, -1, 0, 2, 3],
};
function valid(value: unknown): value is EqualizerSettings {
  const s = value as EqualizerSettings | null;
  return !!s && typeof s.enabled === 'boolean' && Array.isArray(s.gains) && s.gains.length === EQ_BANDS.length
    && s.gains.every(v => Number.isFinite(v) && v >= -12 && v <= 12)
    && Number.isFinite(s.trim) && s.trim >= -12 && s.trim <= 6;
}
function load(): EqualizerSettings {
  try { const saved: unknown = JSON.parse(localStorage.getItem(storageKey) ?? 'null'); if (valid(saved)) return saved; } catch { /* Optional preference. */ }
  return equalizerDefaults();
}
let settings = load();
const listeners = new Set<() => void>();
export const getEqualizer = (): EqualizerSettings => ({ ...settings, gains: [...settings.gains] });
export const equalizerActive = (): boolean => settings.enabled && (settings.trim !== 0 || settings.gains.some(gain => gain !== 0));
export function onEqualizerChange(listener: () => void): () => void { listeners.add(listener); return () => listeners.delete(listener); }
export function setEqualizer(patch: Partial<EqualizerSettings>): void {
  const next = { ...getEqualizer(), ...patch, gains: [...(patch.gains ?? settings.gains)] };
  if (!valid(next)) return;
  settings = next;
  try { localStorage.setItem(storageKey, JSON.stringify(settings)); } catch { /* Session only. */ }
  listeners.forEach(fn => fn());
}
export function resetEqualizer(): void {
  settings = equalizerDefaults();
  try { localStorage.removeItem(storageKey); } catch { /* Session only. */ }
  listeners.forEach(fn => fn());
}

/** Insert one stereo EQ before the existing shared output limiter, once per bus. */
export function connectEqualizer(context: BaseAudioContext, source: AudioNode, destination: AudioNode): void {
  const trim = context.createGain();
  const filters = EQ_BANDS.map((frequency, index) => {
    const filter = context.createBiquadFilter();
    filter.type = index === 0 ? 'lowshelf' : index === EQ_BANDS.length - 1 ? 'highshelf' : 'peaking';
    filter.frequency.value = Math.min(frequency, context.sampleRate * .45); filter.Q.value = .8;
    return filter;
  });
  source.connect(trim); filters.reduce<AudioNode>((previous, filter) => { previous.connect(filter); return filter; }, trim).connect(destination);
  const apply = (initial = false): void => {
    const state = getEqualizer(), now = context.currentTime;
    const update = (param: AudioParam, value: number): void => {
      param.cancelScheduledValues(now); param.setValueAtTime(initial ? value : param.value, now);
      if (!initial) param.linearRampToValueAtTime(value, now + .025);
    };
    update(trim.gain, state.enabled ? 10 ** (state.trim / 20) : 1);
    filters.forEach((filter, index) => update(filter.gain, state.enabled ? state.gains[index] : 0));
  };
  apply(true); onEqualizerChange(() => apply());
}
