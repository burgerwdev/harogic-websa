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
}
export interface DecoderImageStrip { png: string; y: number; rows: number; pw: number; ph: number }
export interface DecoderImage {
  id: string | number; label: string; w: number; h: number; final: boolean;
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
const channels = new Map<number, { entries: DecoderLogEntry[]; bytes: number; evicted: number }>();
const listeners = new Set<() => void>();
let serial = 0;
function channelLog(channel: number) {
  let log = channels.get(channel);
  if (!log) { log = { entries: [], bytes: 0, evicted: 0 }; channels.set(channel, log); }
  return log;
}
export function decoderLog(channel: number, mode: string) {
  const log = channelLog(channel);
  return { entries: log.entries.filter(entry => entry.mode === mode), evicted: log.evicted };
}
function gpsSnapshot(entry: DecoderResult): string | null {
  if (entry.mode !== 'gps' || entry.kind !== 'table' || !entry.data || typeof entry.data !== 'object' || !('table' in entry.data)) return null;
  return entry.data.table === 'satellites' || entry.data.table === 'pvt' ? entry.data.table : null;
}
/** Remove live GPS state when its receiver stops; navigation messages remain history. */
export function clearGpsSnapshots(channel: number, provider?: string, source?: string): void {
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
export function addDecoderResult(result: DecoderResult, source: string, channel: number): void {
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
      log.entries[index] = { ...entry, data: { ...picture, final: true } };
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
    while ((bytes > DECODER_LOG_BYTES || strips.length > 512) && strips.length > 1) {
      const removed = strips.shift()!;
      bytes -= removed.png.length * 2 + removed.pw * removed.ph * 4;
      log.evicted++;
    }
    result = { ...result, data: { id: data.id, label: String(data.label ?? ''), w: data.w, h: data.h, final: data.final === true, strips } };
    if (old) id = old.id;
  } else {
    bytes = JSON.stringify(result.data).length * 2;
    // Live snapshots replace their previous view; message batches remain incremental.
    if (gpsSnapshot(result)) {
      previous = log.entries.findIndex(entry => entry.provider === result.provider && gpsSnapshot(entry) === data?.table);
    } else if (result.kind === 'status' || (result.provider === 'onair' && ['vessels', 'packets', 'aircraft'].includes(String(data?.type)))) {
      previous = log.entries.findIndex(entry => entry.kind === result.kind && entry.provider === result.provider && entry.mode === result.mode);
    }
  }
  if (previous >= 0) { log.bytes -= log.entries[previous].bytes; log.entries.splice(previous, 1); }
  if (bytes <= DECODER_LOG_BYTES) {
    log.entries.unshift({ ...result, id, at: Date.now(), source, bytes });
    log.bytes += bytes;
  } else log.evicted++;
  while (log.entries.length > DECODER_LOG_ENTRIES || log.bytes > DECODER_LOG_BYTES) {
    log.bytes -= log.entries.pop()!.bytes;
    log.evicted++;
  }
  listeners.forEach(fn => fn());
}
export function clearDecoderLog(channel: number, mode?: string): void {
  const log = channelLog(channel);
  log.entries = mode ? log.entries.filter(entry => entry.mode !== mode) : [];
  log.bytes = log.entries.reduce((bytes, entry) => bytes + entry.bytes, 0);
  log.evicted = 0;
  listeners.forEach(fn => fn());
}
export function subscribeDecoderLog(fn: () => void): () => void {
  listeners.add(fn);
  return () => { listeners.delete(fn); };
}
