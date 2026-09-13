// Measurement results (the data store, as opposed to parameters which use slots).

//

// The parameter/result split from the review: user parameters own a slot with

// confirmed/desired/epoch semantics, measurement results are plain data with an

// explicit setter. Keeping them here stops core/store.ts from being a single growing

// bag of everything (report finding P1-6).

import type { ChanResult, HarmResult, M3dBResult, StdSnap } from './model';



export let harm: HarmResult | null = null;

export function setHarm(v: HarmResult | null) { harm = v; }

export let harmAccum: any = null;

export function setHarmAccum(v: any) { harmAccum = v; }

export let pnmData: any = null;

export function setPnmData(v: any) { pnmData = v; }

export let pnmCarAcc: any = null;

export function setPnmCarAcc(v: any) { pnmCarAcc = v; }

export let ampRes: any = null;

export function setAmpRes(v: any) { ampRes = v; }

export let chanRes: ChanResult | null = null;

export function setChanRes(v: ChanResult | null) { chanRes = v; }

export let lastHarmList: any = null;

export function setLastHarmList(v: any) { lastHarmList = v; }

export let m3dB: M3dBResult | null = null;

export function setM3dB(v: M3dBResult | null) { m3dB = v; }

export let peakMarks: any[] | null = null;

export function setPeakMarks(v: any[] | null) { peakMarks = v; }

export let rtaData: any = null;

export function setRtaData(v: any) { rtaData = v; }

export let rtaDisplays: (Float32Array | null)[] = [null, null, null, null];

export function setRtaDisplays(v: (Float32Array | null)[]) { rtaDisplays = v; }

export let rtaAvgN: number[] = [0, 0, 0, 0];

export let rtaAvgSum: (Float32Array | null)[] = [null, null, null, null];

export let rtaDone: boolean[] = [false, false, false, false];

export let rtaDensity2d: Float32Array | null = null;

export function setRtaDensity2d(v: Float32Array | null) { rtaDensity2d = v; }

export let stdSnap: StdSnap | null = null;

export function setStdSnap(v: StdSnap | null) { stdSnap = v; }

export let freqArray: Float64Array | null = null;

export function setFreqArray(v: Float64Array | null) { freqArray = v; }

export let freqVersion = -1;

export function setFreqVersion(v: number) { freqVersion = v; }

export let activeTraceIdx = 0;

export function setActiveTraceIdx(v: number) { activeTraceIdx = v; }

export let activeMkrId = 1;

export function setActiveMkrId(v: number) { activeMkrId = v; }
