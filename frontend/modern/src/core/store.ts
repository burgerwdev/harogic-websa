// Global state (centralized management of the original app.js globals)
//
// Parameters live in ui/*State.ts slots; measurement results live in core/results.ts;
// shared types in core/model.ts. This module keeps the runtime/UI state and re-exports the
// moved names so existing `S.x` / `S.setX` call sites keep working.
import type { MarkerState, TraceState } from './model';
import { requestRender } from '../render/redraw';
// Canvas
//
// These are assigned by initStore() rather than at import time: importing this module used
// to touch the DOM, which made the import order load-bearing (report finding P2-1). They are
// `let` bindings, so every importer sees the assigned values (ESM live bindings); layout code
// must therefore read them inside functions, not at module scope.
//
// Drawing coordinates are CSS pixels of the canvas *content box* (W/H below), not a fixed
// 860x480 grid: the frame is fluid, so the plot uses the box it is actually given and the
// backing store follows it (`round(box x ratio)`), which is what keeps it sharp - a fixed
// backing store stretched by CSS is what "soft on a bigger screen" looked like. Geometry
// (plotRect/getX/getY/MARGIN, peak/marker readouts) stays in these logical units, so no
// readout depends on the window size.
//
// The size is re-derived from a ResizeObserver, never from inside a frame: a per-frame
// getBoundingClientRect() is a forced layout every frame, which render/spectrum.ts used to do
// for the waterfall's CSS width (see 'the waterfall never reads layout' in the DOM guard).
// Fallback drawing space in CSS px, used when there is nothing to measure (jsdom, a hidden
// tab, before the first layout); the live values are W/H below.
export const LOGICAL_W = 860;
export const LOGICAL_H = 480;
export let canvas!: HTMLCanvasElement;
export let ctx!: CanvasRenderingContext2D;
export let W = LOGICAL_W, H = LOGICAL_H;
export let pixelRatio = 1;
export const MARGIN = { left: 10, right: 50, top: 14, bottom: 26 };

// ── Backing-store policy: the one place to change it ──
// The bitmap is stretched onto the content box, so the backing store must be
// `round(box x ratio)`; anything else is resampled by the browser and looks soft.
//   * MAX_DPR 2 - past 2x the extra samples are not visible on the panels this runs on, and
//     the canvas is repainted for every delivered frame.
//   * PIXEL_BUDGET 8M device pixels (~32MB for this canvas) - a 3840px-wide window at 2x
//     would otherwise allocate a 25M-pixel bitmap that is cleared whenever the size changes.
//     Past the budget the ratio is reduced, never below 1.
//   * The ratio is clamped *up* to 1: a device scale factor below 1 is real (with a desktop
//     text scale set, Chromium reports 0.906 on this laptop's panel), and an undersampled
//     bitmap is blurrier than an oversized one.
const MAX_DPR = 2;
const PIXEL_BUDGET = 8_000_000;

let lastBox = { w: -1, h: -1, ratio: -1 };
let resizeObserver: ResizeObserver | null = null;

/** The canvas content box in CSS px - the box the bitmap is painted into. */
function cssBox(el: HTMLCanvasElement): { w: number; h: number } {
  // clientWidth/Height exclude the border and are integers, which keeps the
  // `backing == round(box x ratio)` contract exact; the rect is the pre-layout fallback and
  // the constants cover "no layout at all" (jsdom).
  if (el.clientWidth > 0 && el.clientHeight > 0) return { w: el.clientWidth, h: el.clientHeight };
  const rect = el.getBoundingClientRect();
  if (rect.width > 0 && rect.height > 0) return { w: rect.width, h: rect.height };
  return { w: LOGICAL_W, h: LOGICAL_H };
}

function backingRatio(w: number, h: number): number {
  const wanted = Math.min(MAX_DPR, Math.max(1, window.devicePixelRatio || 1));
  if (w * h * wanted * wanted <= PIXEL_BUDGET) return wanted;
  return Math.max(1, Math.sqrt(PIXEL_BUDGET / (w * h)));
}

/**
 * Re-derive the drawing space and the backing store from the current box and DPR, and
 * re-apply the context transform. Returns true when anything changed (the caller repaints).
 */
export function syncCanvasSize(): boolean {
  const { w, h } = cssBox(canvas);
  const ratio = backingRatio(w, h);
  const backingW = Math.round(w * ratio);
  const backingH = Math.round(h * ratio);
  if (w === lastBox.w && h === lastBox.h && ratio === lastBox.ratio
      && canvas.width === backingW && canvas.height === backingH) {
    return false;
  }
  lastBox = { w, h, ratio };
  W = w;
  H = h;
  pixelRatio = ratio;
  // Setting width/height also clears the bitmap, so only do it when the value moves.
  if (canvas.width !== backingW) canvas.width = backingW;
  if (canvas.height !== backingH) canvas.height = backingH;
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  return true;
}

/** Bind the spectrum canvas. Must run before the first render (main.ts calls it first). */
export function initStore(): void {
  const el = document.getElementById('spectrum') as HTMLCanvasElement | null;
  if (!el) {
    throw new Error('core/store: #spectrum is missing; initStore() must run after the DOM is ready');
  }
  canvas = el;
  ctx = el.getContext('2d') as CanvasRenderingContext2D;
  lastBox = { w: -1, h: -1, ratio: -1 };   // force the first derivation to apply
  syncCanvasSize();
  // A fluid frame means the plot box changes with the window, a UI zoom level or a display
  // switch. Repaint on those, and only on those. jsdom has no ResizeObserver and a fixed box.
  if (typeof ResizeObserver !== 'undefined') {
    resizeObserver?.disconnect();
    resizeObserver = new ResizeObserver(() => {
      if (syncCanvasSize()) requestRender();
    });
    resizeObserver.observe(el);
  }
}

// Frequency/amplitude state
//
// `dbPerDiv`, `levelUnit` and `currentGapFill` intentionally stay plain values: they are
// single-writer, but they are read inside per-frame loops (getY / level conversions / gap
// fill), and calling a slot get() there is exactly what saturated the main thread once
// (DEVELOPMENT.md §3, hot-path rule).
export let FREQ_MIN = 9e3, FREQ_MAX = 9e9;
export function setFrequencyLimits(minimum: number, maximum: number) {
  if (isFinite(minimum) && isFinite(maximum) && minimum > 0 && maximum > minimum) {
    FREQ_MIN = minimum;
    FREQ_MAX = maximum;
  }
}
// The Ref range is a device fact, not a UI constant: STATUS carries it in `caps` for every model
// (it used to be hard-coded here and in the backend's profile clamp).
export let REF_MIN_DBM = -50, REF_MAX_DBM = 30;
export function setRefLimits(minimum: number, maximum: number) {
  if (isFinite(minimum) && isFinite(maximum) && minimum < maximum) {
    REF_MIN_DBM = minimum;
    REF_MAX_DBM = maximum;
  }
}
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
//: One transient message drawn in the canvas status stack (below the warnings). Written by the
//: reference Auto Scale feedback, read by render/statusStack.ts; a single slot, so a newer
//: message replaces the previous one instead of piling up.
export let noticeText: string | null = null;
export function setNoticeText(v: string | null) { noticeText = v; }
export function setTrigOverlay(v: string[]) { trigOverlay = v; }

// Limit line + pass/fail check (state owned by ui/limits.ts, math by dsp/limits.ts)
export interface LimitPointState { freqHz: number; level: number; }
export interface LimitsState { on: boolean; tol: number; points: LimitPointState[]; }
export let limits: LimitsState = { on: false, tol: 0, points: [] };
export function setLimits(v: LimitsState) { limits = v; }


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

// Channel measurements: channel power / OBW / ACPR of the displayed trace

// Smoothing / peak finding

// Peak list
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
