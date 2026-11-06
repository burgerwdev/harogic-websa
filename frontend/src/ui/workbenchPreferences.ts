export interface WorkbenchPreferences { masterGain: number; monitorMode: 'manual' | 'follow' }
const key = ['websa', 'workbench', 'audio', 'v1'].join('-');
export const workbenchAudioDefaults = (): WorkbenchPreferences => ({ masterGain: 0.7, monitorMode: 'manual' });
export function loadWorkbenchPreferences(): WorkbenchPreferences {
  const defaults = workbenchAudioDefaults();
  try {
    const saved = JSON.parse(localStorage.getItem(key) ?? 'null');
    if (saved && typeof saved.masterGain === 'number' && Number.isFinite(saved.masterGain) && saved.masterGain >= 0 && saved.masterGain <= 1)
      defaults.masterGain = saved.masterGain;
    if (saved?.monitorMode === 'manual' || saved?.monitorMode === 'follow') defaults.monitorMode = saved.monitorMode;
  } catch { /* storage unavailable or invalid */ }
  return defaults;
}
export function saveWorkbenchPreferences(preferences: WorkbenchPreferences): void {
  try { localStorage.setItem(key, JSON.stringify(preferences)); } catch { /* session only */ }
}
export function resetWorkbenchPreferences(): void {
  try { localStorage.removeItem(key); } catch { /* session only */ }
}
