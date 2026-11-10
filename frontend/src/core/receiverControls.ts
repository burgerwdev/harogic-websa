/** Persisted mode parameters only. Instant actions never enter this model. */
export interface ModeControls {
  rf_snr?: boolean;
  am_sync?: boolean; pitch?: number; deemph_us?: number; stereo?: boolean; audio_rate?: number; sstv_mode?: number | null;
  radiofax_acquisition?: string; radiofax_ioc?: string; radiofax_lpm?: string; radiofax_modulation?: string;
  radiofax_polarity?: string; radiofax_clock_recovery?: string; radiofax_phase_pixels?: number; radiofax_clock_ppm?: number;
}
export const modeControlDefaults = {
  rf_snr: false,
  am_sync: false, pitch: 700, deemph_us: -1, stereo: true, audio_rate: 48000, sstv_mode: null,
  radiofax_acquisition: 'auto', radiofax_ioc: 'auto', radiofax_lpm: 'auto', radiofax_modulation: 'fm',
  radiofax_polarity: 'normal', radiofax_clock_recovery: 'auto', radiofax_phase_pixels: 0, radiofax_clock_ppm: 0,
} satisfies Required<ModeControls>;
export const modeControlKeys = Object.keys(modeControlDefaults) as (keyof ModeControls)[];
export function copyModeControls(value: ModeControls | undefined): ModeControls {
  return Object.fromEntries(modeControlKeys.map(key => [key, value?.[key] ?? modeControlDefaults[key]]));
}
export const analogEnhancementsApplicable = (mode: string): boolean => !['drm', 'drmplus'].includes(mode);
/** Storage is untrusted; absent optional fields retain compatibility defaults. */
export function validModeControls(value: ModeControls): boolean {
  return modeControlKeys.every(key => {
    const item = value[key];
    if (item === undefined || key === 'sstv_mode' && item === null) return true;
    if (key === 'sstv_mode') return typeof item === 'number' && Number.isInteger(item) && item >= 0;
    if (typeof modeControlDefaults[key] === 'number') return typeof item === 'number' && Number.isFinite(item);
    return typeof item === typeof modeControlDefaults[key];
  });
}
