// Session-only provider output; stream identity remains owned by SDE2.
export type DecoderResultKind = 'text' | 'table' | 'image' | 'status';
export interface DecoderResult {
  provider: string;
  mode: string;
  kind: DecoderResultKind;
  data: unknown;
}
export interface DecoderLogEntry extends DecoderResult {
  id: number;
  at: number;
  source: string;
  bytes: number;
  acknowledged?: boolean;
  timing?: { input_index: string; input_rate: number; fraction_num: string; fraction_den: string;
    delay_seconds: number; delay_known: boolean; causal_presentation: boolean; baseband_stream?: unknown };
}
export interface DecoderImageStrip { png: string; y: number; rows: number; pw: number; ph: number }
export interface DecoderImage {
  id: string | number; label: string; w: number; h: number; final: boolean; partial?: boolean;
  strips: DecoderImageStrip[];
}
export function readDecoderImageStrip(data: Record<string, unknown>): DecoderImageStrip {
  for (const key of ['w', 'h', 'y', 'rows', 'pw', 'ph']) {
    if (!Number.isInteger(data[key]) || Number(data[key]) < (key === 'y' ? 0 : 1)) throw new Error('Invalid decoder image dimensions');
  }
  const { w, h, y, rows, pw, ph } = data as Record<string, number>;
  if (w > 8192 || h > 32768 || y + rows > h || pw * ph > 2_097_152
    || typeof data.png !== 'string' || (typeof data.id !== 'string' && typeof data.id !== 'number')) throw new Error('Invalid decoder image');
  const header = atob(data.png.slice(0, 32));
  const bytes = Uint8Array.from(header, c => c.charCodeAt(0));
  const view = new DataView(bytes.buffer);
  if (bytes.length < 24 || view.getUint32(0) !== 0x89504e47 || view.getUint32(4) !== 0x0d0a1a0a
    || view.getUint32(12) !== 0x49484452 || view.getUint32(16) !== pw || view.getUint32(20) !== ph) throw new Error('Invalid decoder PNG dimensions');
  return { png: data.png, y, rows, pw, ph };
}
export const DECODER_LOG_ENTRIES = 200;
export const DECODER_LOG_BYTES = 8 * 1024 * 1024;
export const DECODER_HISTORY_BYTES = 128 * 1024 * 1024;
const channels = new Map<number | string, { entries: DecoderLogEntry[]; bytes: number; evicted: number }>();
const listeners = new Set<() => void>();
let serial = 0;
function channelLog(channel: number | string) {
  let log = channels.get(channel);
  if (!log) { log = { entries: [], bytes: 0, evicted: 0 }; channels.set(channel, log); }
  return log;
}
export function decoderLog(channel: number | string, mode?: string) {
  const log = channelLog(channel);
  return { entries: log.entries.filter(entry => (mode === undefined || entry.mode === mode)), evicted: log.evicted };
}
export function decoderHistoryUsage(): { bytes: number; limit: number } {
  return { bytes: [...channels.values()].reduce((sum, log) => sum + log.bytes, 0), limit: DECODER_HISTORY_BYTES };
}
export function acknowledgeDecoderLog(channel: string): void {
  const log = channels.get(channel); if (log) for (const entry of log.entries) entry.acknowledged = true;
}
function liveImage(entry: DecoderLogEntry): boolean {
  const image = entry.data as DecoderImage | DecoderRaster;
  return entry.kind === 'image' && !image.final && !image.partial;
}
/** Retain unacknowledged events and active pictures until the consumer applies them. */
function reserveHistory(channel: number | string, bytes: number, replaced: DecoderLogEntry | null): void {
  const log = channelLog(channel), limit = typeof channel === 'string' ? 24 * 1024 * 1024 : DECODER_LOG_BYTES;
  if (bytes > limit) throw new Error('Decoder image exceeds retained history budget; picture is incomplete');
  const added = bytes - (replaced?.bytes ?? 0);
  const count = replaced ? 0 : 1;
  while (log.bytes + added > limit || log.entries.length + count > DECODER_LOG_ENTRIES
    || decoderHistoryUsage().bytes + added > DECODER_HISTORY_BYTES) {
    const local = log.bytes + added > limit || log.entries.length + count > DECODER_LOG_ENTRIES;
    let oldest: { log: typeof log; entry: DecoderLogEntry } | null = null;
    for (const candidate of local ? [log] : channels.values()) for (const entry of candidate.entries) {
      if (entry === replaced || entry.acknowledged === false || liveImage(entry)) continue;
      if (!oldest || entry.id < oldest.entry.id) oldest = { log: candidate, entry };
    }
    if (!oldest) throw new Error('Decoder history budget exhausted by pending events or active images; picture is incomplete');
    oldest.log.entries.splice(oldest.log.entries.indexOf(oldest.entry), 1);
    oldest.log.bytes -= oldest.entry.bytes; oldest.log.evicted++;
  }
}

function gpsSnapshot(entry: DecoderResult): string | null {
  if (entry.mode !== 'gps' || entry.kind !== 'table' || !entry.data || typeof entry.data !== 'object' || !('table' in entry.data)) return null;
  return entry.data.table === 'satellites' || entry.data.table === 'pvt' ? entry.data.table : null;
}
function statusType(data: unknown): string {
  return data && typeof data === 'object' && 'type' in data && typeof data.type === 'string' ? data.type : '';
}
/** Remove live GPS state when its receiver stops; navigation messages remain history. */
export function clearGpsSnapshots(channel: number | string, provider?: string, source?: string): void {
  const log = channels.get(channel);
  if (!log) return;
  const count = log.entries.length;
  log.entries = log.entries.filter(entry => {
    if (!gpsSnapshot(entry) || (provider !== undefined && entry.provider !== provider)
      || (source !== undefined && entry.source !== source)) return true;
    log.bytes -= entry.bytes;
    return false;
  });
  if (log.entries.length !== count) listeners.forEach(fn => fn());
}
export function addDecoderResult(result: DecoderResult, source: string, channel: number | string): void {
  const log = channelLog(channel);
  const data = result.data !== null && typeof result.data === 'object' && !Array.isArray(result.data) ? result.data as Record<string, unknown> : null;
  if (result.mode === 'gps') {
    const fix = data?.fix;
    const noFix = fix !== null && typeof fix === 'object' && 'valid' in fix && fix.valid === false;
    const reset = result.kind === 'status' && (data?.state === 'stopped' || noFix);
    const changedSource = !reset && log.entries.some(entry => entry.provider === result.provider && gpsSnapshot(entry) && entry.source !== source);
    if (reset || changedSource) clearGpsSnapshots(channel, result.provider, reset ? source : undefined);
  }
  if (result.kind === 'status' && data?.type === 'image_end') {
    if ((typeof data.id !== 'number' && typeof data.id !== 'string') || !Number.isInteger(data.rows) || Number(data.rows) < 0) {
      throw new Error('Invalid decoder image completion');
    }
    const index = log.entries.findIndex(entry => entry.kind === 'image' && entry.source === source
      && entry.provider === result.provider && entry.mode === result.mode && (entry.data as DecoderImage).id === data.id);
    if (index >= 0) {
      const entry = log.entries[index];
      const picture = entry.data as DecoderImage;
      if (Number(data.rows) > picture.h) throw new Error('Invalid decoder image completion rows');
      log.entries[index] = { ...entry, data: { ...picture, final: true }, acknowledged: typeof channel === 'number' };
      listeners.forEach(fn => fn());
      return;
    }
  }
  let previous = -1;
  let bytes: number;
  let id = ++serial;
  if (result.kind === 'image') {
    if (!data) throw new Error('Invalid decoder image');
    const strip = readDecoderImageStrip(data);
    previous = log.entries.findIndex(entry => entry.kind === 'image' && entry.source === source
      && entry.provider === result.provider && entry.mode === result.mode && (entry.data as DecoderImage).id === data.id);
    const old = previous >= 0 ? log.entries[previous] : null;
    const picture = old?.data as DecoderImage | undefined;
    if (picture && (picture.w !== data.w || picture.h !== data.h)) throw new Error('Decoder image geometry changed');
    const strips = picture ? [...picture.strips, strip] : [strip];
    bytes = (old?.bytes ?? 0) + strip.png.length * 2 + strip.pw * strip.ph * 4;
    let partial = picture?.partial === true;
    while (strips.length > 512 && strips.length > 1) {
      const removed = strips.shift()!;
      bytes -= removed.png.length * 2 + removed.pw * removed.ph * 4;
      log.evicted++; partial = true;
    }
    result = { ...result, data: { id: data.id, label: String(data.label ?? ''), w: data.w, h: data.h, final: data.final === true && !partial, partial, strips } };
    if (old) id = old.id;
  } else {
    bytes = JSON.stringify(result.data).length * 2;
    // Live snapshots replace their previous view; message batches remain incremental.
    if (gpsSnapshot(result)) {
      previous = log.entries.findIndex(entry => entry.provider === result.provider && gpsSnapshot(entry) === data?.table);
    } else if (result.kind === 'status' || (result.provider === 'onair' && ['vessels', 'packets', 'aircraft'].includes(String(data?.type)))) {
      previous = log.entries.findIndex(entry => entry.kind === result.kind && entry.provider === result.provider && entry.mode === result.mode
        && (result.kind !== 'status' || statusType(entry.data) === statusType(result.data)));
    }
  }
  const old = previous >= 0 ? log.entries[previous] : null;
  const limit = typeof channel === 'string' ? 24 * 1024 * 1024 : DECODER_LOG_BYTES;
  if (bytes > limit && result.kind !== 'image') { log.evicted++; listeners.forEach(fn => fn()); return; }
  reserveHistory(channel, bytes, old);
  if (old) { log.bytes -= old.bytes; id = old.id; log.entries.splice(log.entries.indexOf(old), 1); }
  log.entries.unshift({ ...result, id, at: Date.now(), source, bytes, acknowledged: typeof channel === 'number' });
  log.bytes += bytes;
  listeners.forEach(fn => fn());
}
export function clearDecoderLog(channel: number | string, mode?: string): void {
  const log = channelLog(channel);
  log.entries = mode ? log.entries.filter(entry => entry.mode !== mode) : [];
  log.bytes = log.entries.reduce((bytes, entry) => bytes + entry.bytes, 0);
  log.evicted = 0;
  listeners.forEach(fn => fn());
}
/** Live readings belong to one processing stream; messages and pictures remain history. */
export function clearDecoderStatuses(channel: string): void {
  const log = channels.get(channel); if (!log) return;
  const count = log.entries.length;
  log.entries = log.entries.filter(entry => entry.kind !== 'status');
  log.bytes = log.entries.reduce((bytes, entry) => bytes + entry.bytes, 0);
  if (count !== log.entries.length) listeners.forEach(fn => fn());
}
export function subscribeDecoderLog(fn: () => void): () => void {
  listeners.add(fn);
  return () => { listeners.delete(fn); };
}

/** Raw native image rows retain their original pixels; incomplete/missing rows stay explicit. */
export interface DecoderRaster {
  raster: true; id: string; label: string; w: number; h: number; rgb: Uint8Array;
  rows: number; final: boolean; partial: boolean; missingRows: number[];
}
export function addDecoderRaster(channel: string, mode: string, source: string, image: {
  id: string; label: string; w: number; h: number; row: number; rows: number; rgb: Uint8Array;
  final: boolean; partial: boolean; missingRows?: number[]; grow?: boolean;
}): void {
  const { w, h, row, rows } = image;
  if (![w, h, row, rows].every(Number.isInteger) || w < 1 || h < 1 || row < 0 || rows < 0
    || row + rows > h || w * h * 3 > 20 * 1024 * 1024 || image.rgb.length !== w * rows * 3) throw new Error('Invalid native image rows');
  const log = channelLog(channel), index = log.entries.findIndex(entry => entry.kind === 'image' && entry.source === source
    && entry.mode === mode && (entry.data as DecoderRaster).raster && (entry.data as DecoderRaster).id === image.id);
  const previous = index >= 0 ? log.entries[index] : null;
  const old = previous?.data as DecoderRaster | undefined;
  if (old && (old.w !== w || (!image.grow && old.h !== h))) throw new Error('Native picture geometry changed');
  const height = Math.max(h, old?.h ?? 0);
  const bytes = w * height * 3 + 1024;
  reserveHistory(channel, bytes, previous);
  const rgb = old?.h === height ? old.rgb : new Uint8Array(w * height * 3);
  if (old && rgb !== old.rgb) rgb.set(old.rgb);
  rgb.set(image.rgb, row * w * 3);
  const picture: DecoderRaster = { raster: true, id: image.id, label: image.label, w, h: height, rgb,
    rows: Math.max(old?.rows ?? 0, row + rows), final: image.final && !(old?.partial || image.partial), partial: old?.partial === true || image.partial,
    missingRows: image.missingRows ?? [] };
  if (previous) { log.bytes -= previous.bytes; log.entries.splice(log.entries.indexOf(previous), 1); }
  log.entries.unshift({ id: previous?.id ?? ++serial, at: previous?.at ?? Date.now(), source,
    provider: 'native', mode, kind: 'image', data: picture, bytes, acknowledged: false }); log.bytes += bytes;
  listeners.forEach(fn => fn());
}
export function annotateDecoderResult(channel: string, timing: NonNullable<DecoderLogEntry['timing']>): void {
  const entry = channelLog(channel).entries[0]; if (entry) entry.timing = timing;
}
export function forgetDecoderLog(channel: string): void { channels.delete(channel); }

export function endDecoderRasters(channel: string): void {
  const log = channels.get(channel); if (!log) return;
  log.entries = log.entries.map(entry => entry.kind === 'image' && (entry.data as DecoderRaster).raster && !(entry.data as DecoderRaster).final && !(entry.data as DecoderRaster).partial
    ? { ...entry, data: { ...(entry.data as DecoderRaster), partial: true } } : entry);
}
