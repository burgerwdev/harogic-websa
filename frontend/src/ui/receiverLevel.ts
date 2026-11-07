import type { ReceiverState } from '../core/multichannel';
import type { DecoderLogEntry } from '../sdr/decoderLog';

/** Decode processing identities differ from the IQ stream, notably after recovery. */
export function belongsToReceiverStream(receiver: ReceiverState, entry: DecoderLogEntry): boolean {
  if (!receiver.stream) return false;
  const baseband = entry.timing?.baseband_stream as Partial<ReceiverState['stream']> | undefined;
  return baseband ? baseband.source_id === receiver.stream.source_id && String(baseband.epoch) === receiver.stream.epoch
    && String(baseband.channel_id) === receiver.stream.channel_id : entry.source === `${receiver.stream.epoch}:${receiver.stream.channel_id}`;
}

export interface ReceiverLevel { db: number; peakDb?: number; source: 'input' | 'audio' | 'playback' }
const dbfs = (value: number): number => 20 * Math.log10(Math.max(1e-8, value));

/** Prefer audio-clock measurements; arrival-time telemetry leads buffered playback. */
export function receiverLevel(receiver: ReceiverState, entries: readonly DecoderLogEntry[], playback?: Record<string, unknown>, source?: string | null): ReceiverLevel | null {
  if (receiver.state !== 'running' || receiver.fault || receiver.processing_state === 'failed'
    || receiver.reception && receiver.reception.state !== 'receiving') return null;
  if (playback) {
    if (!playback.enabled || !playback.playing || playback.muted || playback.error
      || playback.contextState && playback.contextState !== 'running'
      || typeof playback.meterAt !== 'number' || Date.now() - playback.meterAt > 500
      || typeof playback.rms !== 'number' || !Number.isFinite(playback.rms) || playback.rms < 0
      || typeof playback.peak !== 'number' || !Number.isFinite(playback.peak) || playback.peak < 0) return null;
    return { db: dbfs(playback.rms), peakDb: dbfs(playback.peak), source: 'playback' };
  }
  const mode = receiver.confirmed.processors[0]?.mode;
  for (const entry of entries) {
    if (source !== undefined && entry.source !== source) continue;
    if (!belongsToReceiverStream(receiver, entry)) continue;
    if (entry.mode !== mode || entry.kind !== 'status' || Date.now() - entry.at > 3000) continue;
    const data = entry.data as Record<string, unknown> | null;
    if (!data || typeof data !== 'object') continue;
    // A listening strip follows programme audio. AM input power includes the
    // steady carrier, so it barely moves even while the demodulated audio varies.
    const keys = receiver.confirmed.purpose === 'listen'
      ? ['rmsDbFs', 'peakDbFs'] as const : ['inputDbFs', 'rmsDbFs', 'peakDbFs'] as const;
    for (const key of keys) {
      const value = data[key];
      if (value != null && value !== '' && Number.isFinite(Number(value))) return { db: Number(value), source: key === 'inputDbFs' ? 'input' : 'audio' };
    }
    if (typeof data.rms === 'number' && Number.isFinite(data.rms) && data.rms >= 0) return { db: dbfs(data.rms), source: 'audio' };
  }
  return null;
}

/** Fixed scales preserve absolute level changes; never auto-normalize a quiet signal. */
export function receiverLevelScale(level: ReturnType<typeof receiverLevel>): { min: number; value: number; percent: number } {
  const min = level?.source === 'input' ? -100 : -60;
  const value = level ? Math.max(min, Math.min(0, level.db)) : min;
  return { min, value, percent: (value - min) / -min * 100 };
}

/** Meter ballistics are time based so hidden tabs never replay old peaks. */
export class ReceiverMeter {
  private identity = '';
  private level = -60;
  private peak = -60;
  private holdUntil = 0;
  private clipUntil = 0;
  private previous = 0;

  update(reading: ReceiverLevel | null, identity: string, now = performance.now()): { level: number; peak: number; clipped: boolean } {
    if (!reading || identity !== this.identity || now - this.previous > 1000) {
      this.level = this.peak = reading?.db ?? -60;
      this.holdUntil = this.clipUntil = 0;
      this.previous = now;
    }
    this.identity = identity;
    const previous = this.previous;
    const seconds = Math.max(0, now - previous) / 1000;
    this.previous = now;
    if (!reading) return { level: -60, peak: -60, clipped: false };
    this.level = Math.max(reading.db, this.level - 18 * seconds);
    const peak = reading.peakDb ?? reading.db;
    if (peak >= this.peak) { this.peak = peak; this.holdUntil = now + 1000; }
    else if (now > this.holdUntil) this.peak = Math.max(peak, this.peak - 24 * (now - Math.max(previous, this.holdUntil)) / 1000);
    if (peak >= 0) this.clipUntil = now + 1000;
    return { level: this.level, peak: this.peak, clipped: this.clipUntil > now };
  }
}
