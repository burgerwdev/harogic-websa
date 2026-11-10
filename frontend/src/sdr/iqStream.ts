import { currentSdrChannel, knownSdrChannelNumbers } from '../ui/sdrChannels';
// Native SDR decoded output. The browser controls, renders and plays; it performs no receiver DSP.
import { send } from '../core/wsSend';
import { setSdrReceiverAudioError } from '../audio/sdrAudio';
import { decodeChannel, ingestDecodeEvents, setAmInfo, setDecodeChannelMode, setDecodeError, setDecodeTiming, setWfmInfo } from '../ui/decodeWindow';
import type { AmInfo } from '../ui/decodeWindow';
import { addFt8Spot, clearFt8Spots, setFt8DecoderState } from './ft8Log';
import { addCwText, clearCwLog, setCwLevel } from './cwLog';
import { clearDrm, setDrmDecode, setDrmConstellation } from './drmLog';
import { clearSstv, setSstvDecoderState, updateSstvPicture } from './sstvLog';
import { appendRadiofaxRows, endRadiofaxStream, parseRadiofaxTransmissionId, setRadiofaxDecoderState } from './radiofaxLog';
import type { DecodeEnvelope, ReceiverEvent } from './decodeProtocol';
import type { Ft8Report } from './types';
import { addDecoderResult, clearDecoderLog, clearGpsSnapshots } from './decoderLog';
import { nativeDecodeOwned, onDecodeOwnership, onNativeDecode } from './decodeOwnership';

export interface ChannelRegistryEntry {
  channel: number;
  channel_id: number;
  epoch: number;
  frequency_hz: number;
  output_rate: number;
  input_rate: number;
  group_delay_samples: number;
  state: string;
}
let confirmedRegistry: readonly ChannelRegistryEntry[] | null = null;
let enabled = false;
let worker: Worker | null = null;
let streamFailed = false;
let streamFailedRegistry: readonly ChannelRegistryEntry[] | null = null;
const transportErrors = new Set<number>();
let connected = false;
let blocks = 0;
let lastError = '';
let current: DecodeEnvelope | null = null;
let failedRegistry: readonly ChannelRegistryEntry[] | null = null;
interface ProcessingChannel {
  channel: number; enabled: boolean; processing_enabled?: boolean;
  control?: { service_id?: string; receiver_uid?: string };
  processing_runtime?: { processing_generation?: string; state?: string; fault?: unknown } | null;
}
let processing = new Map<number, string>();
let failedProcessing = new Map<number, string>();
let identityChanges = 0;
let ft8Text = '', ft8Count = 0, ft8Detail = '';
const pictures = new Map<number, { identity: string; serial: string; width: number; height: number; rgb: Uint8Array; nextRow: number }>();
document.addEventListener('websa:sdr-stream-error', (event) => {
  const detail = (event as CustomEvent).detail;
  // A UID speaker subscription belongs to the workbench, not every legacy
  // channel. Never turn one socket close into an all-receivers failure.
  if (detail?.stream === 'audio' && typeof detail.channel === 'number') {
    setDecodeError(detail.channel, detail.message, 'transport');
  }
});

export function confirmSdrChannelRegistry(registry: readonly ChannelRegistryEntry[] | null): void {
  confirmedRegistry = registry;
  publishIqDebug();
}

export function failSdrReceiver(message: string): void {
  if (failedRegistry !== null && lastError
      && (message === 'Decoded-output receiver failed' || message === 'Backend audio receiver failed')) return;
  if (failedRegistry !== null && lastError === message) return;
  if (failedRegistry === null) failedRegistry = confirmedRegistry ?? (current ? [{ channel: current.channel,
    channel_id: current.channel_id, epoch: current.epoch } as ChannelRegistryEntry] : []);
  worker?.postMessage({ type: 'stop' });
  worker?.terminate();
  worker = null;
  pictures.clear();
  for (const channel of knownSdrChannelNumbers()) {
    setSdrReceiverAudioError(channel, message);
    setCwLevel(0, false, {}, channel);
    clearDrm(channel);
    setWfmInfo(null, channel);
    setFt8DecoderState(null, channel);
    setSstvDecoderState(null, channel);
    endRadiofaxStream(channel);
  }
  reportError(message, 'receiver');
}

export function confirmSdrReceiverError(error: string | null | undefined, channels?: readonly ProcessingChannel[]): boolean {
  const active = channels?.filter(row => row.enabled && row.processing_enabled !== false) ?? [];
  const nextProcessing = new Map(active.filter(row => row.processing_runtime?.processing_generation)
    .map(row => [row.channel, `${row.control?.service_id}:${row.control?.receiver_uid}:${row.processing_runtime!.processing_generation}`]));
  const healthyProcessing = active.length > 0 && active.every(row => !row.processing_runtime?.fault
    && ['active', 'backlogged'].includes(row.processing_runtime?.state ?? ''));
  if (channels) processing = nextProcessing;
  if (typeof error === 'string') { failSdrReceiver(error); return false; }
  const processingChanged = healthyProcessing && failedProcessing.size > 0
    && [...failedProcessing].some(([channel, key]) => nextProcessing.get(channel) !== key);
  const replacedProcessing = processingChanged && [...failedProcessing].every(([channel, key]) => nextProcessing.get(channel) !== key);
  if (error === null && streamFailed && confirmedRegistry?.some(entry => entry.state === 'active')
      && (processingChanged || streamFailedRegistry !== null && !streamFailedRegistry.some(old => confirmedRegistry!.some(next => next.state === 'active'
        && old.channel_id === next.channel_id && old.epoch === next.epoch)))) {
    streamFailed = false;
    streamFailedRegistry = null;
    if (failedRegistry === null) failedProcessing.clear();
    lastError = '';
  }
  if (error !== null || failedRegistry === null || !confirmedRegistry?.some(entry => entry.state === 'active')) return false;
  // A healthy report for the failed identities cannot revive their latched workers.
  if (!replacedProcessing && failedRegistry.some(old => confirmedRegistry!.some(next => next.state === 'active'
    && old.channel_id === next.channel_id && old.epoch === next.epoch))) return false;
  failedRegistry = null;
  streamFailed = false;
  streamFailedRegistry = null;
  failedProcessing.clear();
  lastError = '';
  current = null;
  clearAmState();
  for (const channel of knownSdrChannelNumbers()) setSdrReceiverAudioError(channel, null);
  publishIqDebug();
  return true;
}
function publishIqDebug(): void {
  const cv = document.getElementById('spectrum');
  if (cv) cv.dataset.sdrIq = `enabled=${enabled} blocks=${blocks} backend=1 pipeline=${connected}`
    + ` stream_key=${current ? `${current.epoch}:${current.channel_id}` : '-'} channel_id=${current?.channel_id ?? 0}`
    + ` epoch=${current?.epoch ?? 0} input_index=${current?.input_index ?? '0'} identity_changes=${identityChanges}`
    + ` delay_known=${current?.delay_known ?? false} causal_presentation=${current?.causal_presentation ?? false}`
    + (lastError ? ` error=${lastError}` : '');
}
function reportError(message: string, kind: 'receiver' | 'transport' | 'reconnecting' = 'transport'): void {
  if ((streamFailed || failedRegistry !== null) && failedProcessing.size === 0) failedProcessing = new Map(processing);
  if (streamFailed && streamFailedRegistry === null) streamFailedRegistry = confirmedRegistry ?? [];
  lastError = message;
  connected = false;
  for (const channel of knownSdrChannelNumbers()) {
    clearGpsSnapshots(channel);
    endRadiofaxStream(channel); setAmInfo(null, channel); setDecodeError(channel, message, kind);
    if (kind !== 'receiver') transportErrors.add(channel);
  }
  console.error(`SDR decoded stream: ${message}`);
  document.dispatchEvent(new CustomEvent('websa:sdr-stream-error', { detail: { stream: 'decode', message } }));
  const status = document.getElementById('cur-sdr-level');
  if (status) { status.textContent = '--- dBFS'; status.removeAttribute('title'); }
  publishIqDebug();
}
/** Drop AM sync telemetry and decode errors on every channel (unsafe badge must not outlive the stream). */
function clearAmState(): void {
  transportErrors.clear();
  for (const channel of knownSdrChannelNumbers()) { setAmInfo(null, channel); setDecodeError(channel, null); }
}
function startStream(): void {
  if (nativeDecodeOwned() || worker || !enabled || failedRegistry !== null || streamFailed) return;
  clearAmState();
  const url = `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws/decode`;
  let receiver: Worker;
  try { receiver = new Worker(new URL('./decodeWorker.ts', import.meta.url), { type: 'module' }); }
  catch (error) { streamFailed = true; reportError(error instanceof Error ? error.message : String(error)); return; }
  worker = receiver;
  receiver.onerror = (event) => {
    if (receiver !== worker) return;
    streamFailed = true;
    worker = null;
    receiver.terminate();
    reportError(event.message || 'Decoded-output worker failed');
  };
  receiver.onmessage = (message: MessageEvent<{ type: string; envelopes?: DecodeEnvelope[]; message?: string }>) => {
    if (receiver !== worker) return;
    if (message.data.type === 'connected') { connected = true; publishIqDebug(); return; }
    if (message.data.type === 'error' || message.data.type === 'closed') {
      const text = message.data.message ?? 'Decoded-output stream failed';
      if (message.data.type === 'error') { streamFailed = true; worker = null; receiver.terminate(); }
      // A failed aggregate subscription is not evidence that every backend
      // receiver failed. Only the authoritative control status may latch it.
      reportError(text, message.data.type === 'closed' ? 'reconnecting' : 'transport');
      return;
    }
    if (message.data.type !== 'frames') return;
    try {
      const envelopes = message.data.envelopes;
      if (!envelopes?.length) throw new Error('Missing validated receiver events');
      ingestDecodeEvents(() => {
        for (const envelope of envelopes) {
          if (transportErrors.delete(envelope.channel)) setDecodeError(envelope.channel, null);
          consumeDecode(envelope);
          blocks++;
          current = envelope;
        }
      });
      receiver.postMessage({ type: 'ack' });
      lastError = '';
      connected = true;
      publishIqDebug();
    } catch (error) {
      const text = error instanceof Error ? error.message : String(error);
      streamFailed = true;
      worker = null;
      receiver.terminate();
      reportError(text);
    }
  };
  receiver.postMessage({ type: 'start', url });
}
function finite(value: unknown, fallback = 0): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}
/** Preserve an explicitly unknown (null) measurement instead of coercing it to zero. */
function nullableNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}
function samples(value: unknown): Float32Array | null {
  if (!Array.isArray(value)) return null;
  if (value.length > 1024 || value.some((v) => typeof v !== 'number' || !Number.isFinite(v))) throw new Error('Invalid receiver scope');
  return Float32Array.from(value);
}
function consumeRadiofaxImage(d: ReceiverEvent, identity: string, channel: number): void {
  const width = d.width, row = d.row, rows = d.rows, revision = d.revision;
  if (typeof width !== 'number' || !Number.isInteger(width) || width <= 0
    || typeof row !== 'number' || !Number.isInteger(row) || row < 0
    || typeof rows !== 'number' || !Number.isInteger(rows) || rows <= 0 || rows > 64
    || typeof revision !== 'number' || !Number.isInteger(revision) || revision < 0
    || width * rows > 2 * 1024 * 1024) throw new Error('Invalid radiofax rows');
  const meta = d.meta;
  if (!meta || (typeof meta.serial !== 'string' && typeof meta.serial !== 'number')
    || (typeof meta.epoch !== 'string' && typeof meta.epoch !== 'number')
    || typeof meta.transmissionId !== 'string'
    || (meta.hz !== null && (typeof meta.hz !== 'number' || !Number.isFinite(meta.hz)))
    || typeof meta.ioc !== 'number' || !Number.isFinite(meta.ioc)
    || typeof meta.lpm !== 'number' || !Number.isFinite(meta.lpm) || meta.lpm <= 0
    || typeof meta.lines !== 'number' || !Number.isInteger(meta.lines) || meta.lines !== row + rows || meta.lines > 2400
    || typeof meta.complete !== 'boolean' || typeof meta.partial !== 'boolean'
    || !Array.isArray(meta.missingRows) || meta.missingRows.some((value, index, list) =>
      !Number.isInteger(value) || value < 0 || value >= Number(meta.lines) || (index > 0 && value <= list[index - 1]))
    || (meta.reason !== null && typeof meta.reason !== 'string')
    || typeof meta.segmentIndex !== 'number' || !Number.isInteger(meta.segmentIndex) || meta.segmentIndex < 0) {
    throw new Error('Invalid radiofax picture metadata');
  }
  parseRadiofaxTransmissionId(meta.transmissionId);
  if (typeof d.gray !== 'string') throw new Error('Invalid radiofax pixels');
  let bytes: Uint8Array;
  try {
    const decoded = atob(d.gray);
    bytes = Uint8Array.from(decoded, (c) => c.charCodeAt(0));
  } catch { throw new Error('Invalid radiofax pixels'); }
  if (bytes.length !== width * rows) throw new Error('Invalid radiofax pixel count');
  for (const missing of meta.missingRows) {
    if (missing < row || missing >= row + rows) continue;
    const start = (missing - row) * width;
    if (bytes.subarray(start, start + width).some((value) => value !== 128)) throw new Error('Invalid radiofax missing row pixels');
  }
  appendRadiofaxRows({ width, row, rows, gray: bytes, revision }, {
    serial: String(meta.serial), epoch: String(meta.epoch), hz: meta.hz, ioc: meta.ioc, lpm: meta.lpm,
    lines: meta.lines, complete: meta.complete, partial: meta.partial,
    segmentIndex: meta.segmentIndex, transmissionId: meta.transmissionId,
    missingRows: meta.missingRows, reason: meta.reason,
  }, `${identity}:${String(meta.epoch)}`, channel);
}
export function consumeDecode(envelope: DecodeEnvelope): void {
  const d = envelope.event;
  const channel = envelope.channel as number;
  const identity = `${envelope.epoch}:${envelope.channel_id}`;
  setDecodeTiming(channel, { inputIndex: envelope.input_index, fractionNum: envelope.input_fraction_num,
    fractionDen: envelope.input_fraction_den, inputRate: envelope.input_rate, delaySeconds: envelope.delay_seconds,
    delayKnown: envelope.delay_known, causalPresentation: envelope.causal_presentation });
  if (d.type === 'start') {
    identityChanges++;
    pictures.delete(channel);
    clearGpsSnapshots(channel);
    setDecodeChannelMode(channel, envelope.mode);
    setCwLevel(0, false, {}, channel);
    clearDrm(channel);
    clearCwLog(channel);
    setWfmInfo(null, channel);
    setFt8DecoderState(null, channel);
    setSstvDecoderState(null, channel);
    endRadiofaxStream(channel);
    setAmInfo(null, channel);
    setDecodeError(channel, null);
    return;
  }
  switch (d.type) {
    case 'processing-end':
      // Retuning retires the old processor, not the aggregate subscription.
      // Keep decoded history and accept the successor's ordered start marker.
      pictures.delete(channel);
      clearGpsSnapshots(channel);
      endRadiofaxStream(channel);
      setAmInfo(null, channel);
      setWfmInfo(null, channel);
      setCwLevel(0, false, {}, channel);
      setFt8DecoderState(null, channel);
      setSstvDecoderState(null, channel);
      break;
    case 'decoder-result':
      if (!d.provider || d.mode !== envelope.mode || !d.kind || d.data === undefined) throw new Error('Invalid decoder result');
      addDecoderResult({ provider: d.provider, mode: d.mode, kind: d.kind, data: d.data }, identity, channel);
      break;
    case 'am-info': {
      const info: AmInfo = {
        syncEnabled: d.syncEnabled === true, syncLocked: d.syncLocked === true, syncGeneration: d.amSyncGeneration!,
        carrierHz: nullableNumber(d.carrierHz), carrierOffsetHz: nullableNumber(d.carrierOffsetHz), modulationPct: nullableNumber(d.modulationPct),
        inputDbFs: nullableNumber(d.inputDbFs), rmsDbFs: finite(d.rmsDbFs), peakDbFs: finite(d.peakDbFs),
        scope: samples(d.scope),
      };
      setAmInfo(info, channel);
      break;
    }
    case 'cw':
      setCwLevel(finite(d.rms), d.keyed === true, d, channel);
      addCwText(String(d.text ?? ''), d.endsLine === true, finite(d.share), channel);
      break;
    case 'ft8':
      if (typeof d.centerHz !== 'number' || !Number.isFinite(d.centerHz)) throw new Error('FT8 report missing tuned RF frequency');
      renderFt8Message({ ...d, text: String(d.text ?? ''), frequencyHz: finite(d.frequencyHz),
        timeOffsetS: finite(d.timeOffsetS), snrDb: finite(d.snrDb), centerHz: d.centerHz,
        count: finite(d.count), sourceKey: identity, windowStart: String(d.windowStart ?? envelope.input_index) }, channel);
      break;
    case 'wfm-info':
      setWfmInfo({ ...d, stereo: d.stereo === true, peakDevHz: finite(d.peakDevHz), pilotPct: finite(d.pilotPct),
        peakDbFs: String(d.peakDbFs ?? ''), peakHoldDb: String(d.peakHoldDb ?? ''), rdsLocked: d.rdsLocked === true,
        rdsPi: finite(d.rdsPi), rdsGroups: finite(d.rdsGroups), ps: String(d.ps ?? ''), rt: String(d.rt ?? ''),
        carrierHz: finite(d.carrierHz), pilotDevHz: finite(d.pilotDevHz), pilotSnrDb: finite(d.pilotSnrDb),
        scopeL: samples(d.scopeL), scopeR: samples(d.scopeR) }, channel);
      break;
    case 'drm':
      setDrmDecode(Array.isArray(d.lines) ? d.lines.map(String) : [], finite(d.snrDb, Number.NaN), channel, d.status ?? undefined);
      break;
    case 'drm-constellation':
      if (!Array.isArray(d.points) || d.points.length > 4096 || d.points.some((p) => !Number.isFinite(p.re) || !Number.isFinite(p.im))) {
        throw new Error('Invalid DRM constellation');
      }
      setDrmConstellation(d.points, channel);
      break;
    case 'radiofax-image':
      consumeRadiofaxImage(d, identity, channel);
      break;
    case 'radiofax-state':
      // Enum/number validation happened in decodeProtocol; this only coerces missing numbers.
      setRadiofaxDecoderState({ running: d.running === true, stage: d.stage!, hasPicture: d.hasPicture === true,
        signalLevel: finite(d.signalLevel), carrierCoherence: finite(d.carrierCoherence), ioc: d.ioc ?? null,
        lpm: d.lpm ?? null, modulation: d.modulation!, polarity: d.polarity!, width: finite(d.width),
        lines: finite(d.lines), elapsedSeconds: finite(d.elapsedSeconds), clockStatus: d.clockStatus!,
        clockSource: d.clockSource!, phasePixels: finite(d.phasePixels), clockPpm: finite(d.clockPpm),
        clockConfidence: finite(d.clockConfidence), complete: d.complete === true, partial: d.partial === true,
        reason: d.reason ?? null }, channel);
      break;
    case 'sstv-image': {
      const width = d.width, height = d.height, row = d.row, rows = d.rows;
      if (typeof width !== 'number' || typeof height !== 'number' || typeof row !== 'number' || typeof rows !== 'number'
        || !Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0 || width * height * 3 > 2 * 1024 * 1024
        || !Number.isInteger(row) || !Number.isInteger(rows) || row < 0 || rows < 0 || row + rows > height) throw new Error('Invalid SSTV rows');
      const meta = d.meta ?? {};
      const serial = String(meta.serial ?? '');
      let bytes: Uint8Array;
      if (typeof d.rgb === 'string') {
        const decoded = atob(d.rgb);
        bytes = Uint8Array.from(decoded, (c) => c.charCodeAt(0));
      } else if (Array.isArray(d.rgb) && d.rgb.every((v: unknown) => Number.isInteger(v) && Number(v) >= 0 && Number(v) <= 255)) {
        bytes = Uint8Array.from(d.rgb);
      } else throw new Error('Invalid SSTV pixels');
      if (bytes.length !== width * rows * 3) throw new Error('Invalid SSTV pixel count');
      if (typeof meta.hz !== 'number' || !Number.isFinite(meta.hz)) throw new Error('SSTV picture missing tuned RF frequency');
      let picture = pictures.get(channel);
      if (!picture || picture.identity !== identity || picture.serial !== serial) {
        if (row !== 0) throw new Error('SSTV picture missing first rows');
        picture = { identity, serial, width, height, rgb: new Uint8Array(width * height * 3), nextRow: 0 };
        pictures.set(channel, picture);
      }
      if (picture.width !== width || picture.height !== height || row > picture.nextRow) throw new Error('SSTV picture row gap');
      picture.rgb.set(bytes, row * width * 3);
      picture.nextRow = Math.max(picture.nextRow, row + rows);
      updateSstvPicture({ rgb: picture.rgb.slice(), width, height }, {
        name: String(meta.name ?? ''), modeIndex: finite(meta.modeIndex), vis: finite(meta.vis), width, height,
        lines: finite(meta.lines, picture.nextRow), progress: finite(meta.progress), complete: meta.complete === true,
        serial, epoch: identity, hz: meta.hz,
      }, channel);
      break;
    }
    case 'sstv-state':
      setSstvDecoderState({ running: d.running === true, pushes: finite(d.pushes), attempts: finite(d.attempts),
        bufferedSamples: finite(d.bufferedSamples), bufferedSeconds: finite(d.bufferedSeconds), hasPicture: d.hasPicture === true,
        level: finite(d.level), srcLevel: finite(d.srcLevel) }, channel);
      break;
    case 'ft8-state': {
      const canvas = document.getElementById('spectrum');
      if (canvas) canvas.dataset[`ft8Channel${channel}`] = JSON.stringify(d);
      setFt8DecoderState({ bufferedSamples: finite(d.bufferedSamples), bufferedSeconds: finite(d.bufferedSeconds),
        attempts: finite(d.attempts), count: finite(d.count), level: finite(d.level), epoch: envelope.epoch }, channel);
      break;
    }
    case 'error':
      clearGpsSnapshots(channel);
      setAmInfo(null, channel);
      setDecodeError(channel, d.message ?? 'Decoder failed');
      document.dispatchEvent(new CustomEvent('websa:sdr-stream-error', {
        detail: { stream: 'decode', channel, message: d.message ?? 'Decoder failed' } }));
      break;
    case 'warning':
      setAmInfo(null, channel);
      break;
    case 'nr-status':
    case 'sstv-mode':
    case 'sstv':
      break;
    default:
      throw new Error(`Unknown receiver event: ${d.type}`);
  }
}
export function setSstvModePin(index: number | null, channel = decodeChannel()): void {
  send({ cmd: 'SET_SDR_CHANNEL', channel, sstv_mode: index });
}
export function setSdrIqEnabled(on: boolean): void {
  enabled = on;
  if (on) startStream();
  else {
    streamFailed = false;
    streamFailedRegistry = null;
    if (failedRegistry === null) failedProcessing.clear();
    worker?.postMessage({ type: 'stop' });
    worker?.terminate();
    worker = null;
    connected = false;
    for (const channel of knownSdrChannelNumbers()) { clearGpsSnapshots(channel); endRadiofaxStream(channel); }
    if (failedRegistry === null) clearAmState();
  }
  publishIqDebug();
}
export function resetSdrIq(): void {
  ft8Text = ''; ft8Count = 0; ft8Detail = ''; blocks = 0; current = null; identityChanges = 0;
  if (failedRegistry === null) { confirmedRegistry = null; lastError = ''; }
  pictures.clear();
  for (const channel of knownSdrChannelNumbers()) {
    clearFt8Spots(channel); clearCwLog(channel); clearDrm(channel); clearSstv(channel);
    clearDecoderLog(channel);
    endRadiofaxStream(channel);
    setWfmInfo(null, channel); setAmInfo(null, channel); setFt8DecoderState(null, channel);
    setDecodeError(channel, failedRegistry === null ? null : lastError);
  }
  publishIqDebug();
}
export function renderFt8Message(report: Ft8Report, channel: number = 1): void {
  addFt8Spot(report, Date.now(), channel);
  ft8Text = String(report.text || '');
  ft8Count = Number(report.count) || ft8Count + 1;
  ft8Detail = `${ft8Text}  @ ${(Number(report.frequencyHz) || 0).toFixed(0)} Hz, +${(Number(report.timeOffsetS) || 0).toFixed(2)} s, ${(Number(report.snrDb) || 0).toFixed(0)} dB`;
  publishIqDebug();
}
export function lastFt8Message(): { text: string; count: number; detail: string } {
  return { text: ft8Text, count: ft8Count, detail: ft8Detail };
}
export function sdrIqStats() {
  return { enabled, blocks, samples: 0, dropped: 0, pcmFrames: 0, pipelineReady: connected,
    fallbackReason: '', streamKey: current ? `${current.epoch}:${current.channel_id}` : '', channelId: current?.channel_id ?? 0,
    epoch: current?.epoch ?? 0, inputIndex: current?.input_index ?? '0', groupDelay: current?.delay_seconds ?? 0,
    delayKnown: current?.delay_known ?? false, causalPresentation: current?.causal_presentation ?? false,
    identityChanges, identityMixes: 0, confirmedRegistry, lastError, receiverFailed: failedRegistry !== null };
}
onDecodeOwnership(owned => {
  if (owned) { worker?.postMessage({ type: 'stop' }); worker?.terminate(); worker = null; }
  else startStream();
});
onNativeDecode(envelopes => {
  if (!enabled || failedRegistry !== null) return;
  try {
    ingestDecodeEvents(() => {
      for (const envelope of envelopes) {
        // Saved receivers already retain their UID history in the workbench.
        // The main panel presents only its tuning source; copying every scope
        // into another numeric-channel log duplicates work for every receiver.
        if (envelope.channel !== currentSdrChannel()) continue;
        if (transportErrors.delete(envelope.channel)) setDecodeError(envelope.channel, null);
        consumeDecode(envelope); blocks++; current = envelope;
      }
    });
    lastError = ''; connected = true; publishIqDebug();
  } catch (error) { reportError(error instanceof Error ? error.message : String(error), 'transport'); }
});
export type { Ft8Report };
