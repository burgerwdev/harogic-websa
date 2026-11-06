import type { ReceiverState } from '../core/multichannel';
import type { DecoderLogEntry } from '../sdr/decoderLog';

/** Decode processing identities differ from the IQ stream, notably after recovery. */
export function belongsToReceiverStream(receiver: ReceiverState, entry: DecoderLogEntry): boolean {
  if (!receiver.stream) return false;
  const baseband = entry.timing?.baseband_stream as Partial<ReceiverState['stream']> | undefined;
  return baseband ? baseband.source_id === receiver.stream.source_id && String(baseband.epoch) === receiver.stream.epoch
    && String(baseband.channel_id) === receiver.stream.channel_id : entry.source === `${receiver.stream.epoch}:${receiver.stream.channel_id}`;
}

/** A real input/audio reading; missing or stale telemetry must never look like a signal. */
export function receiverLevel(receiver: ReceiverState, entries: readonly DecoderLogEntry[], playback?: Record<string, unknown>, source?: string | null): { db: number; source: 'input' | 'audio' } | null {
  if (receiver.state !== 'running' || receiver.fault || receiver.processing_state === 'failed'
    || receiver.reception && receiver.reception.state !== 'receiving') return null;
  const mode = receiver.confirmed.processors[0]?.mode;
  for (const entry of entries) {
    if (source !== undefined && entry.source !== source) continue;
    if (!belongsToReceiverStream(receiver, entry)) continue;
    if (entry.mode !== mode || entry.kind !== 'status' || Date.now() - entry.at > 3000) continue;
    const data = entry.data as Record<string, unknown> | null;
    if (!data || typeof data !== 'object') continue;
    for (const key of ['inputDbFs', 'rmsDbFs', 'peakDbFs'] as const) {
      const value = data[key];
      if (value != null && value !== '' && Number.isFinite(Number(value))) return { db: Number(value), source: key === 'inputDbFs' ? 'input' : 'audio' };
    }
    if (typeof data.rms === 'number' && Number.isFinite(data.rms) && data.rms >= 0) return { db: 20 * Math.log10(Math.max(1e-8, data.rms)), source: 'audio' };
  }
  if (playback?.enabled && playback.playing && typeof playback.rms === 'number' && Number.isFinite(playback.rms))
    return { db: 20 * Math.log10(Math.max(1e-8, playback.rms)), source: 'audio' };
  return null;
}
