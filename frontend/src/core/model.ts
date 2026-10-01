// Shared state types (leaf: no imports, so store.ts and results.ts can both use them).
// Split out of core/store.ts so measurement results can live in core/results.ts
// without importing the store back (report finding P1-6).

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

export interface M3dBResult { peak: number; thresh: number; thr: number; pi: number; li: number; ri: number; lf: number; rf: number; lv: number; rv: number; bw: number; peakF: number; }

export interface HarmResult { f0: number; p0: number; count: number; list: { n: number; f: number; amp: number | null; dbc: number | null; idx: number; inSpan: boolean }[]; }

export interface StdSnap { traces: { mode: string }[]; markers: MarkerState[]; }

export interface ChanResult {
  centerHz: number; channelBw: number; obwPercent: number; acpOffset: number; acpBw: number;
  mainDbm: number | null;
  lowerDbc: number | null; upperDbc: number | null;
  obw: number | null; obwLow: number | null; obwHigh: number | null;
}

