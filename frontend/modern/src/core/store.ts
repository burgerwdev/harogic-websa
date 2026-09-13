// Global state (centralized management of the original app.js globals)
//
// Parameters live in ui/*State.ts slots; measurement results live in core/results.ts;
// shared types in core/model.ts. This module keeps the runtime/UI state and re-exports the
// moved names so existing `S.x` / `S.setX` call sites keep working.
import type { MarkerState, TraceState } from './model';
// Canvas
//
// These are assigned by initStore() rather than at import time: importing this module used
// to touch the DOM, which made the import order load-bearing (report finding P2-1). They are
// `let` bindings, so every importer sees the assigned values (ESM live bindings); layout code
// must therefore read them inside functions, not at module scope.
//
// Logical drawing coordinates stay 860x480. The backing store is scaled by the device pixel
// ratio and the context is transformed accordingly, so the canvas is sharp on HiDPI screens
// while all the geometry (plotRect/getX/getY/margins) keeps its logical units (finding P2-4).
export const LOGICAL_W = 860;
export const LOGICAL_H = 480;
export let canvas!: HTMLCanvasElement;
export let ctx!: CanvasRenderingContext2D;
export let W = LOGICAL_W, H = LOGICAL_H;
export let pixelRatio = 1;
export const MARGIN = { left: 10, right: 50, top: 14, bottom: 26 };

/** Bind the spectrum canvas. Must run before the first render (main.ts calls it first). */
export function initStore(): void {
  const el = document.getElementById('spectrum') as HTMLCanvasElement | null;
  if (!el) {
    throw new Error('core/store: #spectrum is missing; initStore() must run after the DOM is ready');
  }
  const ratio = Math.max(1, Math.min(2, window.devicePixelRatio || 1));
  const backingW = Math.round(LOGICAL_W * ratio);
  const backingH = Math.round(LOGICAL_H * ratio);
  if (el.width !== backingW || el.height !== backingH) {
    el.width = backingW;
    el.height = backingH;
  }
  canvas = el;
  W = LOGICAL_W;
  H = LOGICAL_H;
  pixelRatio = ratio;
  ctx = el.getContext('2d') as CanvasRenderingContext2D;
  // Draw in logical pixels; the CSS box still scales the element responsively.
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
}

// Frequency/amplitude state
export let FREQ_MIN = 9e3, FREQ_MAX = 9e9;
export function setFrequencyLimits(minimum: number, maximum: number) {
  if (isFinite(minimum) && isFinite(maximum) && minimum > 0 && maximum > minimum) {
    FREQ_MIN = minimum;
    FREQ_MAX = maximum;
  }
}
export let spanStepHz = 10e6;
export let spanStepAuto = true;
export function setSpanStepHz(v: number) { spanStepHz = v; }
export function setSpanStepAuto(v: boolean) { spanStepAuto = v; }
// (Reference level/mode live in ui/refState.ts; rta_ref_level/rta_ref_mode were unused
// copies - the STATUS `ref` field is already the effective value for the active mode.)
export let configVersion = 0;
export function setConfigVersion(v: number) { configVersion = v; }

// Last centre confirmed by a STATUS while the hardware was in an SWP-family mode.
// STATUS `center` is mode-dependent (in SDR it is the SDR centre), so this is the only
// unambiguous source for "where the swept view is" when handing off to SDR.
export let dbPerDiv = 10.0;
export function setDbPerDiv(v: number) { dbPerDiv = v; }
export const totalDivs = 10;
// (RBW/VBW mode + effective values, points and spur mode live in ui/swpState.ts. Copies
// here were written by the STATUS handler and by the RBW/VBW UI functions at the same time,
// so a user choice could not be told apart from a confirmed value.)
export let currentGapFill = true;
export function setCurrentGapFill(v: boolean) { currentGapFill = v; }
export let sweepMs = 0;
export function setSweepMs(v: number) { sweepMs = v; }
export let deviceConnected = false;
// True while the frames we receive are unusable (saturated/undefined): shown on the canvas
// instead of silently dropping them forever.
export let badData = false;
export function setBadData(v: boolean) { badData = v; }
export function setDeviceConnected(v: boolean) { deviceConnected = v; }
export let lastMeasKey = '';
export function setLastMeasKey(v: string) { lastMeasKey = v; }
// (The display reference level lives in ui/displayRef.ts: it had several writers with no
// arbitration, so a Preset/normalise/trace reset could clobber a manually set SDR Ref.)
export let levelUnit: 'dBm' | 'dBmV' | 'dBuV' | 'dBV' = 'dBm';
export function setLevelUnit(v: 'dBm' | 'dBmV' | 'dBuV' | 'dBV') { levelUnit = v; }
// RTA trigger parameters live in ui/triggerState.ts (slots, report finding P1-6).
// SWP software level trigger: the swept engine has no level trigger, so the condition is
// evaluated in software across consecutive sweeps (see ui/swpTrigger.ts).
export let swpArmed = false;
export function setSwpArmed(v: boolean) { swpArmed = v; }
export let swpHold = false;
export function setSwpHold(v: boolean) { swpHold = v; }
export let swpPrev: Float32Array | null = null;
export function setSwpPrev(v: Float32Array | null) { swpPrev = v; }
export let trigArmed = false;
export function setTrigArmed(v: boolean) { trigArmed = v; }
export let trigWaiting = false;
export function setTrigWaiting(v: boolean) { trigWaiting = v; }
export let trigHit = false;
export function setTrigHit(v: boolean) { trigHit = v; }
export let trigOverlay: string[] = [];      // status chip + warnings, drawn top-right
// Vendor warning lines for the same canvas stack (lines starting with '!' draw as warnings).
export let statusWarnings: string[] = [];
export function setStatusWarnings(v: string[]) { statusWarnings = v; }
export function setTrigOverlay(v: string[]) { trigOverlay = v; }

// Limit line + pass/fail check (state owned by ui/limits.ts, math by dsp/limits.ts)
export interface LimitPointState { freqHz: number; level: number; }
export interface LimitsState { on: boolean; tol: number; points: LimitPointState[]; }
export let limits: LimitsState = { on: false, tol: 0, points: [] };
export function setLimits(v: LimitsState) { limits = v; }

export const units: Record<string, string> = { center: 'MHz', span: 'MHz', start: 'MHz', stop: 'MHz', rbw: 'kHz', vbw: 'kHz', pnm: 'MHz', rta_center: 'MHz' };

// Traces
export const traces: TraceState[] = [
  { id: 1, mode: 'CLEAR_WRITE', raw: null, powers: null, avgSum: null, avgCount: 0, reference: null, isNormalized: false, avgTarget: 16, avgTargetRta: 16, done: false },
  { id: 2, mode: 'OFF', raw: null, powers: null, avgSum: null, avgCount: 0, reference: null, isNormalized: false, avgTarget: 16, avgTargetRta: 16, done: false },
  { id: 3, mode: 'OFF', raw: null, powers: null, avgSum: null, avgCount: 0, reference: null, isNormalized: false, avgTarget: 16, avgTargetRta: 16, done: false },
  { id: 4, mode: 'OFF', raw: null, powers: null, avgSum: null, avgCount: 0, reference: null, isNormalized: false, avgTarget: 16, avgTargetRta: 16, done: false },
];

// Markers
export const MARKER_COLORS = ['#FF4444', '#FFD700', '#1E90FF', '#FFFFFF'];
export const markers: MarkerState[] = [
  { id: 1, enabled: false, mode: 'OFF', idx: 0, refId: 1, freq: null, tracking: false },
  { id: 2, enabled: false, mode: 'OFF', idx: 0, refId: 1, freq: null, tracking: false },
  { id: 3, enabled: false, mode: 'OFF', idx: 0, refId: 1, freq: null, tracking: false },
  { id: 4, enabled: false, mode: 'OFF', idx: 0, refId: 1, freq: null, tracking: false },
];

// Measurement state

export let measOn = false;
export function setMeasOn(v: boolean) { measOn = v; }
export let measTabSel = 'amp';
export function setMeasTabSel(v: string) { measTabSel = v; }
export let viewMode: string = 'std';
export function setViewMode(v: string) { viewMode = v; }

export let harmValMode = 'RT';
// Channel measurements: channel power / OBW / ACPR of the displayed trace

// Smoothing / peak finding
export let valleySeqPos = 0;
export function setValleySeqPos(v: number) { valleySeqPos = v; }

// Peak list
export let peakListOn = false;
export function setPeakListOn(v: boolean) { peakListOn = v; }
export let peakThrUserSet = false;
export function setPeakThrUserSet(v: boolean) { peakThrUserSet = v; }
export let normRefWinUser = 0;
export function setNormRefWinUser(v: number) { normRefWinUser = v; }
// Latest GNSS status (for the detail popover)
// RTA 实时频谱 + 瀑布
   // per-trace RTA accumulation
// Density persistence params (UI-configurable in RTA mode)
   // freq x amp probability density (signal trace path)
export const waterfallRows: Uint16Array[] = [];
export let waterfallPushes = 0;   // monotonic row counter (rows array saturates at 512)
export function pushWaterfallRow(row: Uint16Array) {
  waterfallRows.push(row);
  waterfallPushes++;
  if (waterfallRows.length > 512) waterfallRows.shift();
}
export function resetWaterfall() { waterfallRows.length = 0; waterfallPushes = 0; }
// Waterfall colour range: Auto = per-frame relative (noise texture always visible),
// Fixed = an absolute dBm window shared by both modes.
export let rtaMode = false;
export function setRtaMode(v: boolean) { rtaMode = v; }
// SDR mode: reuses the RTA renderer, but auto-scales the amplitude and draws a
// listen-frequency marker / passband.
export let sdrMode = false;
export function setSdrMode(v: boolean) { sdrMode = v; }
// (Every SDR parameter - centre/decimate/span/listen/demod/ifbw/deemph - lives in
// ui/sdrState.ts. Copies here are what let the store and the form controls disagree.)
// Audio is OFF until the user explicitly enables it ("Listen").
// (The SDR auto-scale and audio preferences live in ui/sdrState.ts.)

export let lastGnss: any = null;
export function setLastGnss(v: any) { lastGnss = v; }
export let dragging = false;
export function setDragging(v: boolean) { dragging = v; }

// Constants
export const NORM_POS_CAP = 0.0;
export const ABSORB_THRESH = 0.3;
export const NORM_REF_RBW_FACTOR = 60;
export const PNM_OFFSETS = [100, 1000, 10000, 100000, 1000000, 10000000];

// ── Re-exports of the moved state (results + types) ──
export type {
  ChanResult, ExtremaItem, HarmResult, M3dBResult, MarkerState, StdSnap, TraceState,
} from './model';
export {
  activeMkrId, activeTraceIdx, ampRes, chanRes, freqArray, freqVersion, harm, harmAccum,
  lastHarmList, m3dB, peakMarks, pnmCarAcc, pnmData, rtaAvgN, rtaAvgSum, rtaData,
  rtaDensity2d, rtaDisplays, rtaDone, setActiveMkrId, setActiveTraceIdx, setAmpRes,
  setChanRes, setFreqArray, setFreqVersion, setHarm, setHarmAccum, setLastHarmList, setM3dB,
  setPeakMarks, setPnmCarAcc, setPnmData, setRtaData,
  setRtaDensity2d, setRtaDisplays, setStdSnap, stdSnap,
} from './results';
