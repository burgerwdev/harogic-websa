// Global state (centralized management of the original app.js globals)
export interface TraceState {
  id: number;
  mode: string;
  prevMode?: string;   // mode before Freeze (View) toggle, to restore on unfreeze
  raw: Float32Array | null;
  powers: Float32Array | null;
  avgSum: Float32Array | null;
  avgCount: number;
  reference: Float32Array | null;
  isNormalized: boolean;
  _settling?: boolean;
  _lastAbsorb?: number;
  _noiseFloorT?: number;
  _floorAt?: number;
  avgTarget: number;      // SWP average depth (0 = continuous)
  avgTargetRta: number;   // RTA average depth (mode-private, 0 = continuous)
  done: boolean;       // finite average completed
}

export interface MarkerState {
  id: number;
  enabled: boolean;
  mode: string; // OFF | NORMAL | DELTA
  idx: number;
  refId: number;
  freq: number | null;
  tracking: boolean;
}

export interface ExtremaItem { i: number; v: number; sv?: number; f: number; a: number; }

// Canvas
export const canvas = document.getElementById('spectrum') as HTMLCanvasElement;
export const ctx = canvas.getContext('2d') as CanvasRenderingContext2D;
export const W = canvas.width, H = canvas.height;
export const MARGIN = { left: 10, right: 50, top: 14, bottom: 26 };

// Frequency/amplitude state
export let FREQ_MIN = 9e3, FREQ_MAX = 9e9;
export function setFrequencyLimits(minimum: number, maximum: number) {
  if (isFinite(minimum) && isFinite(maximum) && minimum > 0 && maximum > minimum) {
    FREQ_MIN = minimum;
    FREQ_MAX = maximum;
  }
}
export let centerHz = 1e9, spanHz = 100e6, refLevel = 0.0;
export let spanStepHz = 10e6;
export let spanStepAuto = true;
export function setSpanStepHz(v: number) { spanStepHz = v; }
export function setSpanStepAuto(v: boolean) { spanStepAuto = v; }
export let refMode: 'manual' | 'auto' = 'manual';
export function setRefMode(v: 'manual' | 'auto') { refMode = v; }
export let configVersion = 0;
export function setConfigVersion(v: number) { configVersion = v; }
export let rtaCenterHz: number = 1e9;   // independent RTA-mode center
export function setRtaCenterHz(v: number) { rtaCenterHz = v; }
export function setCenterHz(v: number) { centerHz = v; }
export function setSpanHz(v: number) { spanHz = v; }
export function setRefLevel(v: number) { refLevel = v; }
export let dbPerDiv = 10.0;
export function setDbPerDiv(v: number) { dbPerDiv = v; }
export const totalDivs = 10;
export let currentRBW = 300e3, currentVBW = 300e3;
export function setCurrentRBW(v: number) { currentRBW = v; }
export function setCurrentVBW(v: number) { currentVBW = v; }
export let rbwMode = 'auto', vbwMode = 'bypass';
export function setRbwMode(v: string) { rbwMode = v; }
export function setVbwMode(v: string) { vbwMode = v; }
export let currentPoints = 1001, currentSpur = 'standard', currentGapFill = true;
export function setCurrentPoints(v: number) { currentPoints = v; }
export function setCurrentSpur(v: string) { currentSpur = v; }
export function setCurrentGapFill(v: boolean) { currentGapFill = v; }
export let sweepMs = 0;
export function setSweepMs(v: number) { sweepMs = v; }
export let deviceConnected = false;
export function setDeviceConnected(v: boolean) { deviceConnected = v; }
export let lastMeasKey = '';
export function setLastMeasKey(v: string) { lastMeasKey = v; }
export let displayUnit: 'dBm' | 'dB' = 'dBm';
export function setDisplayUnit(v: 'dBm' | 'dB') { displayUnit = v; }
export let displayRef = 0.0;
export function setDisplayRef(v: number) { displayRef = v; }
export let levelUnit: 'dBm' | 'dBmV' | 'dBuV' | 'dBV' = 'dBm';
export function setLevelUnit(v: 'dBm' | 'dBmV' | 'dBuV' | 'dBV') { levelUnit = v; }
export let displayOffset = 0.0;
export function setDisplayOffset(v: number) { displayOffset = v; }

// Limit line + pass/fail check (state owned by ui/limits.ts, math by dsp/limits.ts)
export interface LimitPointState { freqHz: number; level: number; }
export interface LimitsState { on: boolean; tol: number; points: LimitPointState[]; }
export let limits: LimitsState = { on: false, tol: 0, points: [] };
export function setLimits(v: LimitsState) { limits = v; }

export let freqArray: Float64Array | null = null;
export function setFreqArray(v: Float64Array | null) { freqArray = v; }
export let freqVersion = -1;
export function setFreqVersion(v: number) { freqVersion = v; }

export const units: Record<string, string> = { center: 'MHz', span: 'MHz', start: 'MHz', stop: 'MHz', rbw: 'kHz', vbw: 'kHz', pnm: 'MHz', rta_center: 'MHz' };

// Traces
export let activeTraceIdx = 0;
export function setActiveTraceIdx(v: number) { activeTraceIdx = v; }
export const TRACE_COLORS = ['#00FF00', '#FFFF00', '#00FFFF', '#FF00FF'];
export const traces: TraceState[] = [
  { id: 1, mode: 'CLEAR_WRITE', raw: null, powers: null, avgSum: null, avgCount: 0, reference: null, isNormalized: false, avgTarget: 16, avgTargetRta: 16, done: false },
  { id: 2, mode: 'OFF', raw: null, powers: null, avgSum: null, avgCount: 0, reference: null, isNormalized: false, avgTarget: 16, avgTargetRta: 16, done: false },
  { id: 3, mode: 'OFF', raw: null, powers: null, avgSum: null, avgCount: 0, reference: null, isNormalized: false, avgTarget: 16, avgTargetRta: 16, done: false },
  { id: 4, mode: 'OFF', raw: null, powers: null, avgSum: null, avgCount: 0, reference: null, isNormalized: false, avgTarget: 16, avgTargetRta: 16, done: false },
];

// Markers
export let activeMkrId = 1;
export function setActiveMkrId(v: number) { activeMkrId = v; }
export const MARKER_COLORS = ['#FF4444', '#FFD700', '#1E90FF', '#FFFFFF'];
export const markers: MarkerState[] = [
  { id: 1, enabled: false, mode: 'OFF', idx: 0, refId: 1, freq: null, tracking: false },
  { id: 2, enabled: false, mode: 'OFF', idx: 0, refId: 1, freq: null, tracking: false },
  { id: 3, enabled: false, mode: 'OFF', idx: 0, refId: 1, freq: null, tracking: false },
  { id: 4, enabled: false, mode: 'OFF', idx: 0, refId: 1, freq: null, tracking: false },
];

// Measurement state
export interface M3dBResult { peak: number; thresh: number; thr: number; pi: number; li: number; ri: number; lf: number; rf: number; lv: number; rv: number; bw: number; peakF: number; }
export let m3dB: M3dBResult | null = null;
export function setM3dB(v: M3dBResult | null) { m3dB = v; }
export interface HarmResult { f0: number; p0: number; count: number; list: { n: number; f: number; amp: number | null; dbc: number | null; idx: number; inSpan: boolean }[]; }
export let harm: HarmResult | null = null;
export function setHarm(v: HarmResult | null) { harm = v; }
export let measOn = false;
export function setMeasOn(v: boolean) { measOn = v; }
export let measTabSel = 'amp';
export function setMeasTabSel(v: string) { measTabSel = v; }
export let viewMode: string = 'std';
export function setViewMode(v: string) { viewMode = v; }
export interface StdSnap { traces: { mode: string }[]; markers: MarkerState[]; }
export let stdSnap: StdSnap | null = null;
export function setStdSnap(v: StdSnap | null) { stdSnap = v; }
export let harmValMode = 'RT';
export function setHarmValMode(v: string) { harmValMode = v; }
export let harmAccum: any = null;
export function setHarmAccum(v: any) { harmAccum = v; }
export let pnmData: any = null;
export function setPnmData(v: any) { pnmData = v; }
export let pnmCarAcc: any = null;
export function setPnmCarAcc(v: any) { pnmCarAcc = v; }
export let ampRes: any = null;
export function setAmpRes(v: any) { ampRes = v; }

// Channel measurements: channel power / OBW / ACPR of the displayed trace
export interface ChanResult {
  centerHz: number; channelBw: number; obwPercent: number; acpOffset: number; acpBw: number;
  mainDbm: number | null;
  lowerDbc: number | null; upperDbc: number | null;
  obw: number | null; obwLow: number | null; obwHigh: number | null;
}
export let chanRes: ChanResult | null = null;
export function setChanRes(v: ChanResult | null) { chanRes = v; }
export let lastHarmList: any = null;
export function setLastHarmList(v: any) { lastHarmList = v; }

// Smoothing / peak finding
export let smoothBins = 1;
export function setSmoothBins(v: number) { smoothBins = v; }
export let valleySeqPos = 0;
export function setValleySeqPos(v: number) { valleySeqPos = v; }

// Peak list
export let peakListOn = false;
export function setPeakListOn(v: boolean) { peakListOn = v; }
export let peakMarks: any[] | null = null;
export function setPeakMarks(v: any[] | null) { peakMarks = v; }
export let peakThrUserSet = false;
export function setPeakThrUserSet(v: boolean) { peakThrUserSet = v; }
export let normRefWinUser = 0;
export function setNormRefWinUser(v: number) { normRefWinUser = v; }
export let defaultMkrDone = false;
export function setDefaultMkrDone(v: boolean) { defaultMkrDone = v; }
// Latest GNSS status (for the detail popover)
// RTA 实时频谱 + 瀑布
export let rtaData: any = null;
export function setRtaData(v: any) { rtaData = v; }
export let rtaDisplays: (Float32Array | null)[] = [null, null, null, null];   // per-trace RTA accumulation
export let rtaAvgN: number[] = [0, 0, 0, 0];
export let rtaAvgSum: (Float32Array | null)[] = [null, null, null, null];
export let rtaDone: boolean[] = [false, false, false, false];
export function setRtaDisplays(v: (Float32Array | null)[]) { rtaDisplays = v; }
// Density persistence params (UI-configurable in RTA mode)
export let RTA_AMP_BINS = 128;                            // amplitude bins (~0.78 dB/bin at 100 dB)
export let rtaFade = 0.98;                                // per-frame density decay (slower = longer persistence)
export function setRtaAmpBins(v: number) { RTA_AMP_BINS = v; }
export function setRtaFade(v: number) { rtaFade = v; }
export let rtaDensity2d: Float32Array | null = null;   // freq x amp probability density (signal trace path)
export function setRtaDensity2d(v: Float32Array | null) { rtaDensity2d = v; }
export const waterfallRows: Uint16Array[] = [];
export let waterfallPushes = 0;   // monotonic row counter (rows array saturates at 512)
export function pushWaterfallRow(row: Uint16Array) {
  waterfallRows.push(row);
  waterfallPushes++;
  if (waterfallRows.length > 512) waterfallRows.shift();
}
export function resetWaterfall() { waterfallRows.length = 0; waterfallPushes = 0; }
export let waterfallOn = false;
export function setWaterfallOn(v: boolean) { waterfallOn = v; }
export let wfPaused = false;
export function setWfPaused(v: boolean) { wfPaused = v; }
export let rtaMode = false;
export function setRtaMode(v: boolean) { rtaMode = v; }

export let lastGnss: any = null;
export function setLastGnss(v: any) { lastGnss = v; }
export let dragging = false;
export function setDragging(v: boolean) { dragging = v; }

// Constants
export const NORM_POS_CAP = 0.0;
export const ABSORB_THRESH = 0.3;
export const NORM_REF_RBW_FACTOR = 60;
export const NORM_SETTLE_MS = 4500;
export const PNM_OFFSETS = [100, 1000, 10000, 100000, 1000000, 10000000];
