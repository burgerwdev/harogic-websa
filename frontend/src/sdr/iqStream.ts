// SDR IQ stream — the DSP worker's lifecycle, parameters and diagnostics.
//
// The main thread owns neither the socket nor the DSP: it starts/stops the worker, pushes the
// demodulator parameters the backend confirmed, and mirrors what the worker reports. Once the
// worker says its WASM pipeline is producing PCM, the AudioWorklet port is handed to it, so
// playback is fed by the browser DSP; until then the Python audio path keeps playing.
//
// A separate module from the audio path because the IQ ingress is a different stream with a
// different lifetime: it runs while SDR mode is active, independent of whether the speaker is on.
import { enablePythonAudioFallback, routeWorkletPortTo } from '../audio/sdrAudio';
import { wasmDspAllowed, wasmDspReason } from './capability';
import { addFt8Spot, clearFt8Spots } from './ft8Log';
import { addCwText, setCwLevel } from './cwLog';
import { setDrmDecode, setDrmConstellation } from './drmLog';
import { dspWasmUrl } from './wasm';
import type { Ft8Report, PipelineParams } from './types';
import { t } from '../core/i18n';

let worker: Worker | null = null;
let enabled = false;
let blocks = 0;
let samples = 0;
let rate = 0;
let centerHz = 0;
let dropped = 0;
let flushes = 0;
let pcmFrames = 0;
let pcmSamples = 0;
let pipelineReady = false;
let handedOff = false;
let lastError = '';
//: Why the browser DSP is off (empty when it is on): 'requested', 'no-webassembly',
//: 'stored-preference' or a fetch/instantiation failure. The Python path is playing in that case.
let fallbackReason = '';
//: The digital path's output: the last decoded message and the timing it came with.
let ft8Text = '';
let ft8Count = 0;
let ft8Detail = '';
//: Digital-path diagnostics (see the worker): is the decoder being fed, and is it getting through
//: a transmission, or is its buffer restarting?
let digitalDiagnostics = '';
/// Characters the CW decoder has produced (diagnostics for a session that has no other evidence).
let cwCharsSeen = 0;
/// The CW decoder's state from the worker ('off' | 'loading' | 'ready'): published always, because a
/// decoder that never built is the failure worth seeing.
let dspCwState = 'off';
//: True when the DSP worker owns the AudioWorklet port (i.e. the browser is what you hear).
let dspOwnsWorklet = false;
let dspWorkletAvailable = 0;
let dspWorkletUnderruns = 0;
let dspWorkletReceived = 0;
let dspError = '';
let dspRingResets = 0;
let dspPending = 0;
let dspDelivered = 0;
let dspMode = '';
let dspSlipped = 0;
let dspRatio = 1;
let dspNr = false;
let dspNrStrength = 0.6;
let dspSquelch = -110;
/// DeepFilterNet3 stage status reported by the worker ('', 'loading', 'ready', 'fallback').
let dspDfnState = '';
let dspDfnReason = '';
/// The PCM level the browser DSP measured (the S-meter reading when it owns playback).
let dspRms = 0;
/// The de-emphasis the browser chain is running (microseconds; < 0 = the mode's default).
let dspDeemph = -1;
/// The playback fill's range in the last window (samples); a periodic dip shows an audible hitch.
let dspFillMin = 0;
let dspFillMax = 0;
/// Pipeline (re)builds this session; a number that keeps climbing is a periodic transient.
let dspBuilds = 0;
/// PCM-level artifact counters (a silent block in a running stream, a step between blocks).
let dspSilent = 0;
let dspDisc = 0;
let dspPcmBlocks = 0;
/// The PCM level's range over the last window (a breathing noise floor shows a wide range).
let dspRmsMin = 0;
let dspRmsMax = 0;
/// True while the listener's audio switch is on (the DSP's own playback gate).
let dspAudio = true;

/** IQ-only WebSocket URL for the worker (the display connection carries no IQ). */
function iqWorkerUrl(): string {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const token = sessionStorage.getItem('web-sa-token');
  const q = token ? `&token=${encodeURIComponent(token)}` : '';
  return `${protocol}//${location.host}/ws?iq=1${q}`;
}

/** Publish the worker's counters where the e2e and a bug report can read them. */
function publishIqDebug(): void {
  const cv = document.getElementById('spectrum') as HTMLCanvasElement | null;
  if (cv) {
    const ms = rate > 0 ? (samples / rate) * 1000 : 0;
    cv.dataset.sdrIq =
      `enabled=${enabled} blocks=${blocks} samples=${samples} buffered_ms=${ms.toFixed(0)}` +
      ` rate=${rate.toFixed(0)} center_hz=${centerHz.toFixed(0)} dropped=${dropped}` +
      ` flushes=${flushes} pcm_frames=${pcmFrames} pcm_samples=${pcmSamples}` +
      ` pipeline=${pipelineReady}` + (lastError ? ` error=${lastError}` : '') +
      ` fallback=${fallbackReason} ft8=${ft8Count} dsp_worklet=${dspOwnsWorklet ? 1 : 0}` +
      ` dsp_avail=${dspWorkletAvailable} dsp_underruns=${dspWorkletUnderruns}` +
      ` dsp_delivered=${dspDelivered} dsp_received=${dspWorkletReceived}` +
      ` dsp_pending=${dspPending} dsp_ring_resets=${dspRingResets}` +
      ` dsp_mode=${dspMode} dsp_slip=${dspSlipped} dsp_ratio=${dspRatio.toFixed(4)}` +
      ` dsp_audio=${dspAudio ? 1 : 0} dsp_deemph=${dspDeemph}` +
      ` dsp_fill_ms=${(dspFillMin / 48).toFixed(0)}..${(dspFillMax / 48).toFixed(0)}` +
      ` dsp_builds=${dspBuilds} dsp_pcm_blocks=${dspPcmBlocks}` +
      ` dsp_silent_blocks=${dspSilent} dsp_discontinuities=${dspDisc}` +
      ` dsp_pcm_rms=${dspRmsMin.toFixed(4)}..${dspRmsMax.toFixed(4)}` +
      ` dsp_nr=${dspNr ? 1 : 0}/${dspNrStrength.toFixed(2)} dsp_squelch=${dspSquelch}` +
      (dspDfnState ? ` dsp_nr_algo=${dspDfnState}` + (dspDfnReason ? `:${dspDfnReason}` : '') : '') +
      ` dsp_cw=${dspCwState} dsp_cw_chars=${cwCharsSeen}` +
      (dspError ? ` dsp_error=${dspError}` : '') + (digitalDiagnostics ? ` ${digitalDiagnostics}` : '');
  }
}

/** Show the DeepFilterNet3 stage's loading/fallback state in the NR row. */
function renderDfnStatus(): void {
  const el = document.getElementById('nr-dfn-status');
  if (!el) return;
  if (dspDfnState === 'loading') {
    el.style.display = '';
    el.textContent = t('nr_dfn_loading');
    el.title = '';
  } else if (dspDfnState === 'fallback') {
    el.style.display = '';
    el.textContent = t('nr_dfn_fallback');
    el.title = dspDfnReason || '';
  } else {
    el.style.display = 'none';
    el.textContent = '';
    el.title = '';
  }
}

function startWorker(): void {
  if (worker) return;
  // jsdom (unit tests) and pre-Worker browsers have no DSP host. That is not an error: the
  // Python-side DSP path is the documented fallback when the browser cannot run the pipeline.
  if (typeof Worker === 'undefined') return;
  try {
    worker = new Worker(new URL('./iqWorker.ts', import.meta.url), { type: 'module' });
  } catch {
    worker = null;
    return;
  }
  worker.onerror = (event: ErrorEvent) => {
    dspError = `worker: ${event.message || 'error'}`;
    publishIqDebug();
  };
  worker.onmessage = (event: MessageEvent) => {
    const d = (event.data || {}) as Record<string, any>;
    if (d.type === 'error') {
      lastError = String(d.message || 'error');
      // The browser DSP cannot run (no module, no kernel for the mode, a delivery failure): the
      // Python path is the documented fallback and only now is it started.
      fallbackReason = 'dsp-error';
      enablePythonAudioFallback();
      publishIqDebug();
      return;
    }
    if (d.type === 'ft8') {
      renderFt8Message(d as unknown as Ft8Report);
      return;
    }
    if (d.type === 'cw') {
      // The CW decoder's characters: the log keeps them, the decode window renders them. The level
      // and the gate state are separate state: they arrive with every block (tens per second) and
      // drive the window's meter and keyed lamp without touching the text.
      setCwLevel(Number(d.rms) || 0, Boolean(d.keyed));
      addCwText(String(d.text || ''), Boolean(d.endsLine), Number(d.share) || 0);
      return;
    }
    if (d.type === 'drm') {
      setDrmDecode(Array.isArray(d.lines) ? d.lines.map(String) : [String(d.lines || '')], Number(d.snrDb) || null);
      return;
    }
    if (d.type === 'drm-constellation') {
      setDrmConstellation(Array.isArray(d.points) ? (d.points as { re: number; im: number }[]) : []);
      return;
    }
    if (d.type === 'dfn-status') {
      dspDfnState = String(d.state || '');
      dspDfnReason = String(d.reason || '');
      renderDfnStatus();
      return;
    }
    if (d.type !== 'stats') return;
    if (typeof d.blocks === 'number') blocks = d.blocks;
    if (typeof d.samples === 'number') samples = d.samples;
    if (typeof d.rate === 'number') rate = d.rate;
    if (typeof d.centerHz === 'number') centerHz = d.centerHz;
    if (typeof d.dropped === 'number') dropped = d.dropped;
    if (typeof d.flushes === 'number') flushes = d.flushes;
    // The digital path's own counters: is the decoder being fed, how many windows has it searched,
    // and how often was one thrown away? (A digital mode produces no audio, so these are the only
    // evidence.) `dsp_attempts` counts searches only; a reset costs a whole `window >= hop + burst`
    // accumulation, so the two belong apart - reporting them together hid a session in which most
    // "attempts" were resets.
    if (typeof d.cwChars === 'number') cwCharsSeen = d.cwChars;
    if (typeof d.cw === 'string') dspCwState = d.cw;
    if (typeof d.digitalPushes === 'number') {
      digitalDiagnostics =
        `dsp_pushes=${d.digitalPushes} dsp_buffered=${d.digitalBuffered}` +
        ` dsp_decodes=${d.ft8Messages ?? 0}` +
        ` dsp_attempts=${d.digitalAttempts ?? 0} dsp_resets=${d.decoderResets ?? 0}` +
        ` dsp_audio_resets=${d.digitalResets ?? 0} dsp_dropped=${d.digitalDropped ?? 0}`;
    }
    if (typeof d.worklet === 'number') dspOwnsWorklet = d.worklet === 1;
    if (typeof d.workletAvailable === 'number') dspWorkletAvailable = d.workletAvailable;
    if (typeof d.workletUnderruns === 'number') dspWorkletUnderruns = d.workletUnderruns;
    if (typeof d.workletReceived === 'number') dspWorkletReceived = d.workletReceived;
    if (typeof d.workletError === 'string') dspError = d.workletError;
    if (typeof d.workletRingResets === 'number') dspRingResets = d.workletRingResets;
    if (typeof d.pcmPending === 'number') dspPending = d.pcmPending;
    if (typeof d.deliveredSamples === 'number') dspDelivered = d.deliveredSamples;
    if (typeof d.workletSlipped === 'number') dspSlipped = d.workletSlipped;
    if (typeof d.workletRatio === 'number') dspRatio = d.workletRatio;
    if (typeof d.nr === 'boolean') dspNr = d.nr;
    if (typeof d.nrStrength === 'number') dspNrStrength = d.nrStrength;
    if (typeof d.squelchDbfs === 'number') dspSquelch = d.squelchDbfs;
    if (typeof d.rms === 'number') dspRms = d.rms;
    if (typeof d.mode === 'string' && d.mode) dspMode = d.mode;
    if (typeof d.deemphUs === 'number') dspDeemph = d.deemphUs;
    if (typeof d.fillMin === 'number') dspFillMin = d.fillMin;
    if (typeof d.fillMax === 'number') dspFillMax = d.fillMax;
    if (typeof d.pipelineBuilds === 'number') dspBuilds = d.pipelineBuilds;
    // `dsp_silent_blocks` / `dsp_discontinuities` are cumulative since the stream started: a number
    // that climbs while listening is the artifact the ear reports.
    if (typeof d.silentBlocks === 'number') dspSilent = d.silentBlocks;
    if (typeof d.discontinuities === 'number') dspDisc = d.discontinuities;
    if (typeof d.pcmBlocks === 'number') dspPcmBlocks = d.pcmBlocks;
    if (typeof d.pcmRmsMin === 'number') dspRmsMin = d.pcmRmsMin;
    if (typeof d.pcmRmsMax === 'number') dspRmsMax = d.pcmRmsMax;
    if (typeof d.audioOn === 'boolean') dspAudio = d.audioOn;
    if (typeof d.pcmFrames === 'number') pcmFrames = d.pcmFrames;
    if (typeof d.pcmSamples === 'number') pcmSamples = d.pcmSamples;
    pipelineReady = Boolean(d.pipeline);
    // One handoff, and only once the DSP is actually producing PCM: handing the worklet to a
    // worker whose pipeline failed would silence the radio, which is the failure this ordering
    // (and the Python fallback behind it) exists to prevent.
    if (pipelineReady && !handedOff && worker) {
      handedOff = routeWorkletPortTo(worker);
    }
    publishIqDebug();
  };
  // The browser DSP is used only when the policy allows it: with `?wasm=0` (or without WebAssembly)
  // the worker is started without a module, so it never produces PCM and never takes the worklet
  // port — the Python audio path (`?audio=1`) keeps playing, which is the documented fallback.
  fallbackReason = wasmDspAllowed() ? '' : wasmDspReason();
  worker.postMessage({
    type: 'init',
    url: iqWorkerUrl(),
    wasmUrl: fallbackReason ? '' : dspWasmUrl(),
  });
  worker.postMessage({ type: 'enabled', value: enabled });
}

/** Push the confirmed demodulator parameters (and listener preferences) to the DSP worker. */
export function configureSdrPipeline(
  params: PipelineParams,
  options: {
    volume?: number;
    audioEnabled?: boolean;
    nr?: boolean;
    nrStrength?: number;
    nrAlgo?: 'wiener' | 'dfn';
    nrAtten?: number;
    squelch?: number;
  } = {},
): void {
  worker?.postMessage({
    type: 'configure',
    params,
    volume: options.volume,
    audioEnabled: options.audioEnabled,
    nr: options.nr,
    nrStrength: options.nrStrength,
    nrAlgo: options.nrAlgo,
    nrAtten: options.nrAtten,
    squelch: options.squelch,
  });
}

/** Noise reduction on/off, its strength, algorithm and the DFN attenuation limit: applied live. */
export function setSdrPipelineNr(on: boolean, strength: number, algo: 'wiener' | 'dfn', atten: number): void {
  worker?.postMessage({ type: 'nr', enabled: on, strength, algo, atten });
}

/**
 * The listener's audio switch, when the browser DSP owns playback.
 *
 * The worklet is the thing that produces sound, and the browser DSP is the one holding its port, so
 * the switch has to be sent here - `sdrAudio.ts` only knows the Python path (that was the "the On/Off
 * button does nothing" report).
 */
export function setSdrDspAudioEnabled(on: boolean): void {
  worker?.postMessage({ type: 'audio-enabled', value: on });
}

/** De-emphasis in microseconds for the browser chain (< 0 = the mode's default). */
export function setSdrPipelineDeemph(tauUs: number): void {
  worker?.postMessage({ type: 'deemph', value: tauUs });
}

/** The squelch threshold in dBFS: applied live. */
export function setSdrPipelineSquelch(dbfs: number): void {
  worker?.postMessage({ type: 'squelch', value: dbfs });
}

/** Volume is a listener preference: applied in the worker, where the PCM is produced. */
export function setSdrPipelineVolume(volume: number): void {
  worker?.postMessage({ type: 'volume', value: volume });
}

/** Turn the WASM audio-enhancement chain on/off (RAW/digital comparisons flip this). */
export function setSdrPipelineAudio(enabled: boolean): void {
  worker?.postMessage({ type: 'audio', value: enabled });
}

/**
 * Enter/leave the IQ ingress. Called on the SDR mode edge: the stream is pointless outside SDR
 * (the backend only produces IQ while an SDR session runs) and a lingering socket would keep the
 * backend encoding raw IQ for nobody.
 */
export function setSdrIqEnabled(on: boolean): void {
  enabled = on;
  if (on) {
    startWorker();
    worker?.postMessage({ type: 'enabled', value: true });
  } else {
    worker?.postMessage({ type: 'enabled', value: false });
  }
  publishIqDebug();
}

/** Drop the worker's stream state (preset/reset): counters restart with the new capture geometry. */
export function resetSdrIq(): void {
  ft8Text = '';
  ft8Count = 0;
  ft8Detail = '';
  clearFt8Spots();
  lastFt8 = '';
  blocks = 0;
  samples = 0;
  dropped = 0;
  flushes = 0;
  rate = 0;
  centerHz = 0;
  pcmFrames = 0;
  pcmSamples = 0;
  lastError = '';
  worker?.postMessage({ type: 'reset' });
  publishIqDebug();
}

/**
 * Record a decoded transmission.
 *
 * The panel used to carry a one-line readout of the newest decode; the decode table replaced it
 * (a single line is unreadable on a busy band), so this appends to the log the window renders and
 * keeps the diagnostic string for the status line. Exported so it is unit tested without a worker.
 */
export function renderFt8Message(report: Ft8Report): void {
  addFt8Spot(report);
  ft8Text = String(report.text || '');
  ft8Count = Number(report.count) || ft8Count + 1;
  ft8Detail =
    `${ft8Text}  @ ${(Number(report.frequencyHz) || 0).toFixed(0)} Hz, ` +
    `+${(Number(report.timeOffsetS) || 0).toFixed(2)} s, ${(Number(report.snrDb) || 0).toFixed(0)} dB`;
  lastFt8 = ft8Detail;
  publishIqDebug();
}

/// The newest decode, for the status line and tests (the window shows the log).
let lastFt8 = '';

/** The last decoded FT8 message (tests and diagnostics). */
export function lastFt8Message(): { text: string; count: number; detail: string } {
  return { text: ft8Text, count: ft8Count, detail: lastFt8 };
}

/**
 * The level of what the browser DSP is playing, in dBFS, or NaN when it is not the audio source.
 *
 * The S-meter used to come from the backend's Python demodulator; when the browser owns playback
 * that demodulator is not running (that is the point of the fallback split), so the level comes
 * from the PCM the browser produced.
 */
export function dspLevelDbfs(): number {
  if (!dspOwnsWorklet || dspRms <= 0) return Number.NaN;
  return 20 * Math.log10(dspRms);
}

export type { Ft8Report };

/** Diagnostics for the e2e and for tests: the transport counters and the DSP state. */
export function sdrIqStats(): {
  enabled: boolean;
  blocks: number;
  samples: number;
  dropped: number;
  pcmFrames: number;
  pipelineReady: boolean;
  fallbackReason: string;
} {
  return { enabled, blocks, samples, dropped, pcmFrames, pipelineReady, fallbackReason };
}
