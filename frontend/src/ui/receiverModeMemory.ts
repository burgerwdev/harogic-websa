import type { ServiceSnapshot } from '../core/multichannel';

/** Only confirmed editor parameters, scoped to a service, receiver and browser tab. */
export interface ReceiverModeSettings {
  passband_hz: number; transition_hz: number; minimum_rate_hz: number;
  bandwidth_hz: number; pcm_gain: number; agc: boolean; squelch_dbm: number;
}
const key = ['websa', 'workbench', 'modes', 'v1'].join('-'), limit = 256 * 1024;
const valid = (value: unknown): value is ReceiverModeSettings => {
  if (!value || typeof value !== 'object') return false;
  const s = value as ReceiverModeSettings;
  return [s.passband_hz, s.transition_hz, s.minimum_rate_hz, s.bandwidth_hz].every(n => Number.isFinite(n) && n > 0)
    && Number.isFinite(s.pcm_gain) && s.pcm_gain >= 0 && s.pcm_gain <= 2
    && typeof s.agc === 'boolean' && Number.isFinite(s.squelch_dbm);
};
const copy = (s: ReceiverModeSettings): ReceiverModeSettings => ({ passband_hz: s.passband_hz,
  transition_hz: s.transition_hz, minimum_rate_hz: s.minimum_rate_hz, bandwidth_hz: s.bandwidth_hz,
  pcm_gain: s.pcm_gain, agc: s.agc, squelch_dbm: s.squelch_dbm });

export class ReceiverModeMemory {
  private service?: string;
  private initialized = false;
  private entries = new Map<string, Map<string, ReceiverModeSettings>>();
  private encoded = '';

  observe(snapshot: ServiceSnapshot): void {
    const active = new Set(snapshot.receivers.map(r => r.receiver_uid));
    const modes = new Map(snapshot.capabilities.mode_descriptors?.map(mode => [mode.id, mode]));
    const allowed = new Set(['', ...modes.keys()]);
    if (!this.initialized || this.service !== snapshot.service_id) {
      this.entries.clear(); this.service = snapshot.service_id; this.initialized = true; this.encoded = '';
      try {
        const raw = this.service ? sessionStorage.getItem(key) : null;
        const saved = raw && raw.length <= limit ? JSON.parse(raw) : null;
        if (saved?.service_id === this.service && Array.isArray(saved?.receivers)) {
          for (const entry of saved.receivers.slice(0, active.size)) {
            if (!Array.isArray(entry) || !active.has(entry[0]) || !Array.isArray(entry[1])) continue;
            const values = new Map<string, ReceiverModeSettings>();
            for (const item of entry[1].slice(0, allowed.size)) {
              if (Array.isArray(item) && allowed.has(item[0]) && valid(item[1])) values.set(item[0], copy(item[1]));
            }
            this.entries.set(entry[0], values);
          }
        }
      } catch { /* storage unavailable or invalid; keep this session's confirmed values */ }
    }
    for (const [uid, values] of this.entries) {
      if (!active.has(uid)) this.entries.delete(uid);
      else for (const mode of values.keys()) if (!allowed.has(mode)) values.delete(mode);
    }
    for (const row of snapshot.receivers) {
      const c = row.confirmed, processor = c.processors[0], mode = processor?.mode ?? '', descriptor = modes.get(mode);
      if (c.processors.length > 1 || !allowed.has(mode)) continue;
      const settings = { passband_hz: c.baseband.passband_hz, transition_hz: c.baseband.transition_hz,
        minimum_rate_hz: c.baseband.minimum_rate_hz, bandwidth_hz: processor?.bandwidth_hz ?? descriptor?.defaultBandwidthHz ?? 6000,
        pcm_gain: processor?.pcm_gain ?? 0.5, agc: processor?.agc !== false, squelch_dbm: processor?.squelch_dbm ?? -160 };
      if (!valid(settings)) continue;
      const values = this.entries.get(row.receiver_uid) ?? new Map<string, ReceiverModeSettings>();
      values.set(mode, settings); this.entries.set(row.receiver_uid, values);
    }
    if (this.service) {
      const encoded = JSON.stringify({ service_id: this.service, receivers: [...this.entries].map(([uid, values]) => [uid, [...values]]) });
      if (encoded !== this.encoded && encoded.length <= limit) {
        try { sessionStorage.setItem(key, encoded); this.encoded = encoded; } catch { /* memory remains usable */ }
      }
    }
  }

  get(uid: string, mode: string): ReceiverModeSettings | undefined {
    const settings = this.entries.get(uid)?.get(mode);
    return settings ? copy(settings) : undefined;
  }
}
