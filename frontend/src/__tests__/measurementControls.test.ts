/**
 * Measurement sessions and their consumers.
 *
 * A measurement result belongs to the parameter application that asked for it, not to the
 * moment it arrives: every SET_PNM / SET_HARM carries a session_id, the STATUS echo of that
 * command confirms the session, and only a confirmed, still-current session may fill the
 * display. Pinned here: a fresh apply clears the old result and orphans the old id (even a
 * late acknowledgement of the old command must not revive it), the channel power is computed
 * from the *selected* absolute trace with no fallback, normalization rebuilds its reference
 * from the absolute reference (never from the already-relative display), and the limit CSV
 * exports the active absolute trace or nothing.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';


import { resetAll } from '../core/params';
import * as S from '../core/store';
vi.mock('../core/wsSend', () => ({ send: vi.fn(), onCommandSent: vi.fn() }));
import { send } from '../core/wsSend';
import { resetGraphMode, graphMode, deviceMode } from '../ui/graphMode';
import { centerHz } from '../ui/freqState';
import { displayUnit } from '../ui/displayState';
import { getDisplayRef } from '../ui/displayRef';
import { refLevel } from '../ui/refState';
import { endMeasurementSessions, confirmMeasurementSession } from '../meas/session';
import { measPnmApply, onPnmResult, updatePnmTable } from '../meas/phaseNoise';
import { measHarmApply, onHarmResult } from '../meas/harmonic';
import { measureChannel } from '../meas/channel';
import { exitMeasMode, measToggle } from '../ui/measure';
import { applyMeasUI, setMeasButtons } from '../ui/measureUi';
import { measureAmp } from '../meas/amplitude';
import { waterfallOn } from '../ui/waterfallState';
import { getActiveAbsolutePowers } from '../dsp/absoluteTrace';
import {
  normalizeActiveTrace,
  resetActiveTraceNormalize,
  setNormalizeReferenceWindow,
} from '../dsp/normalize';
import { processTraces } from '../dsp/traces';
import { exportLimitCsv } from '../ui/limits';
import { fmtHzUnit } from '../core/fmt';
import { setRenderer } from '../render/redraw';

const N = 100;
const SPACING = 10e3;
const ORIG_CLICK = HTMLAnchorElement.prototype.click;

type SentMsg = Record<string, unknown>;

function sentOf(cmd: string): SentMsg[] {
  return vi
    .mocked(send)
    .mock.calls.map(([m]) => m as unknown as SentMsg)
    .filter(m => m.cmd === cmd);
}

function lastSent(cmd: string): SentMsg {
  const calls = sentOf(cmd);
  expect(calls.length, `expected a ${cmd} send`).toBeGreaterThan(0);
  return calls[calls.length - 1];
}

function makeFreqs(): Float64Array {
  return new Float64Array(Array.from({ length: N }, (_, i) => 1e6 + i * SPACING));
}

function flat(level: number): Float32Array {
  return new Float32Array(N).fill(level);
}

function pnmDom(centerMHz: string): void {
  document.body.innerHTML =
    `<input id="input-pnm" value="${centerMHz}">` +
    '<input id="input-pnm-thr" value="-50">' +
    '<input id="input-pnm-avg" value="4">' +
    '<table id="pnm-table" style="display:none"><tbody id="pnm-tbody"></tbody></table>';
}

function harmDom(f0MHz: string): void {
  document.body.innerHTML =
    `<input id="input-harm-f0" value="${f0MHz}">` +
    '<input id="input-harm-count" value="5">' +
    '<select id="select-harm-span"><option value="10000000">10 MHz</option></select>' +
    '<table id="harmonic-table" style="display:none"></table>';
}

function chanDom(channelBwMHz: string): void {
  document.body.innerHTML =
    '<input id="input-chan-center" value="">' +
    `<input id="input-chan-bw" value="${channelBwMHz}">` +
    '<input id="input-chan-offset" value="2">' +
    '<input id="input-chan-adjbw" value="1">' +
    '<select id="select-chan-obw"><option value="99">99%</option></select>' +
    '<table id="chan-table" style="display:none"></table>';
}

interface PnmFrame {
  cmd: string;
  session_id: string;
  done: boolean;
  offset: Float32Array;
  pn: Float32Array;
  carrier_freq: number;
  carrier_power: number;
  progress: number;
}

/** A ready PNM result: full offset axis, no invalid bins, one carrier sample. */
function pnmFrame(sessionId: string, carrierFreq: number, carrierPower: number): PnmFrame {
  return {
    cmd: 'PNM',
    session_id: sessionId,
    done: true,
    offset: new Float32Array([100, 1000, 10e3, 100e3, 1e6, 10e6]),
    pn: new Float32Array([-80, -90, -100, -110, -120, -130]),
    carrier_freq: carrierFreq,
    carrier_power: carrierPower,
    progress: 100,
  };
}

interface HarmRow {
  n: number;
  f: number;
  amp: number;
}

const harmList = (amps: number[]): HarmRow[] =>
  amps.map((amp, i) => ({ n: i + 1, f: 1e9 * (i + 1), amp }));

function applyPnm(centerMHz: string): SentMsg {
  pnmDom(centerMHz);
  S.setMeasTabSel('pnm');
  measPnmApply();
  return lastSent('SET_PNM');
}

function armLimitDownloadCapture(): { name: string; text: string }[] {
  const downloads: { name: string; text: string }[] = [];
  let lastBlobText = '';
  const OrigBlob = globalThis.Blob;
  vi.stubGlobal(
    'Blob',
    class extends OrigBlob {
      constructor(parts: BlobPart[], opts?: BlobPropertyBag) {
        super(parts, opts);
        lastBlobText = parts.join('');
      }
    },
  );
  vi.stubGlobal('URL', {
    createObjectURL: () => 'blob:fake',
    revokeObjectURL: () => {},
  });
  HTMLAnchorElement.prototype.click = function (this: HTMLAnchorElement) {
    downloads.push({ name: this.download, text: lastBlobText });
  };
  return downloads;
}

beforeEach(() => {
  resetAll();
  resetGraphMode();
  graphMode.confirm('std');
  S.setRtaMode(false);
  S.setSdrMode(false);
  S.setMeasOn(true);
  S.setViewMode('std');
  S.setStdSnap(null);
  S.setMeasTabSel('amp');
  S.setActiveTraceIdx(0);
  S.setPnmData(null);
  S.setPnmCarAcc(null);
  S.setHarm(null);
  S.setHarmAccum(null);
  S.setLastHarmList(null);
  S.setChanRes(null);
  S.setFreqArray(makeFreqs());
  S.setFreqVersion(0);
  centerHz.confirm(1.5e6);
  S.traces.forEach((t, i) => {
    t.mode = i === 0 ? 'CLEAR_WRITE' : 'OFF';
    t.prevMode = undefined;
    t.raw = null;
    t.powers = null;
    t.avgSum = null;
    t.avgCount = 0;
    t.reference = null;
    t.isNormalized = false;
  });
  endMeasurementSessions();
  document.body.replaceChildren();
  vi.mocked(send).mockClear();
});

afterEach(() => {
  setRenderer(null);
  vi.unstubAllGlobals();
  HTMLAnchorElement.prototype.click = ORIG_CLICK;
});

describe('PNM session lifecycle', () => {
  it('carries the session id on the wire and ignores results before the acknowledgement', () => {
    const setPnm = applyPnm('1000');

    expect(String(setPnm.session_id)).toContain('-pnm-');
    expect(setPnm.center).toBe(1e9);
    expect(setPnm.start).toBe(100);
    expect(setPnm.stop).toBe(10e6);
    expect(S.viewMode).toBe('pnm');
    expect(lastSent('SET_MODE').mode).toBe('pnm');
    expect(S.pnmData).toBeNull();

    // Result arrives before the STATUS echo: unconfirmed session, nothing is applied.
    onPnmResult(pnmFrame(String(setPnm.session_id), 1e9, -10));
    expect(S.pnmData).toBeNull();
    expect(S.pnmCarAcc).toBeNull();

    confirmMeasurementSession({ response_to: 'SET_PNM', session_id: String(setPnm.session_id) });
    onPnmResult(pnmFrame(String(setPnm.session_id), 1e9 + 37, -10));
    expect(S.pnmData).not.toBeNull();
    expect(S.pnmCarAcc.f).toEqual([1e9 + 37]);
    expect(S.pnmCarAcc.sumP).toBeCloseTo(-10);
  });

  it('a re-apply at 200 MHz clears the old result and orphans the old session id', () => {
    const first = applyPnm('1000');
    confirmMeasurementSession({ response_to: 'SET_PNM', session_id: String(first.session_id) });
    onPnmResult(pnmFrame(String(first.session_id), 1e9, -10));
    expect(S.pnmData).not.toBeNull();

    // The user retunes to 200 MHz: a new session, and the stale result goes away.
    const second = applyPnm('200');
    expect(second.session_id).not.toBe(first.session_id);
    expect(S.pnmData).toBeNull();
    expect(S.pnmCarAcc).toBeNull();

    // The late acknowledgement of the OLD command must not confirm the new session,
    // and the old id stays dead even though its own ack was recorded.
    confirmMeasurementSession({ response_to: 'SET_PNM', session_id: String(first.session_id) });
    onPnmResult(pnmFrame(String(first.session_id), 1e9, -10));
    expect(S.pnmData).toBeNull();
    expect(S.pnmCarAcc).toBeNull();

    confirmMeasurementSession({ response_to: 'SET_PNM', session_id: String(second.session_id) });
    onPnmResult(pnmFrame(String(second.session_id), 2e8 + 10, -12));
    expect(S.pnmCarAcc.f).toEqual([2e8 + 10]);

    // The table's offset is measured against the NEWLY APPLIED center (200 MHz),
    // not the first apply's 1 GHz: 2e8 + 10 - 2e8 = +10 Hz.
    updatePnmTable();
    expect(document.getElementById('pnm-tbody')!.innerHTML).toContain(fmtHzUnit(10));
  });

  it('the carrier accumulator keeps a bounded rolling window', () => {
    const first = applyPnm('1000');
    confirmMeasurementSession({ response_to: 'SET_PNM', session_id: String(first.session_id) });
    for (let i = 1; i <= 31; i++) onPnmResult(pnmFrame(String(first.session_id), 1e9 + i, -10 - i));
    expect(S.pnmCarAcc.f.length).toBe(30);
    // The oldest sample (i = 1) was evicted: the sums describe the last 30 only.
    expect(S.pnmCarAcc.sumF).toBeCloseTo(30 * 1e9 + (2 + 31) * 15);
    expect(S.pnmCarAcc.sumP).toBeCloseTo(-795);   // powers -12..-41 over the kept 30
  });

  it('does nothing while measurements are off, and ending the mode kills the session', () => {
    S.setMeasOn(false);
    pnmDom('1000');
    S.setMeasTabSel('pnm');
    measPnmApply();
    expect(sentOf('SET_PNM')).toHaveLength(0);
    expect(sentOf('SET_MODE')).toHaveLength(0);
    expect(S.viewMode).toBe('std');

    S.setMeasOn(true);
    const first = applyPnm('1000');
    confirmMeasurementSession({ response_to: 'SET_PNM', session_id: String(first.session_id) });

    // Leaving measurement mode ends the session: even the confirmed id is dead.
    exitMeasMode();
    expect(S.viewMode).toBe('std');
    onPnmResult(pnmFrame(String(first.session_id), 1e9, -10));
    expect(S.pnmData).toBeNull();
  });
});

describe('harmonic session lifecycle', () => {
  it('acknowledged results fill S.harm; a stale id after a re-apply is rejected', () => {
    harmDom('1000');
    S.setMeasTabSel('harm');
    measHarmApply();
    const first = lastSent('SET_HARM');
    expect(String(first.session_id)).toContain('-harm-');
    expect(first.f0).toBe(1e9);
    expect(S.harm).toBeNull();

    // Unconfirmed: rejected.
    onHarmResult(harmList([-10, -30]), String(first.session_id));
    expect(S.harm).toBeNull();

    confirmMeasurementSession({ response_to: 'SET_HARM', session_id: String(first.session_id) });
    onHarmResult(harmList([-10, -30]), String(first.session_id));
    expect(S.harm).not.toBeNull();
    expect(S.harm!.p0).toBeCloseTo(-10);
    expect(S.harm!.list.map(x => x.dbc)).toEqual([0, -20]);

    // Retune to 2 GHz: the display and accumulator are cleared and the old id is dead
    // even though its acknowledgement was recorded.
    (document.getElementById('input-harm-f0') as HTMLInputElement).value = '2000';
    measHarmApply();
    const second = lastSent('SET_HARM');
    expect(second.session_id).not.toBe(first.session_id);
    expect(second.f0).toBe(2e9);
    expect(S.harm).toBeNull();
    expect(S.harmAccum).toBeNull();

    onHarmResult(harmList([-5]), String(first.session_id));
    expect(S.harm).toBeNull();

    confirmMeasurementSession({ response_to: 'SET_HARM', session_id: String(second.session_id) });
    onHarmResult(harmList([-7]), String(second.session_id));
    expect(S.harm!.p0).toBeCloseTo(-7);
  });
});

describe('channel power source selection', () => {
  it('sums the active trace: 100 bins at -60 dBm give -40 dBm in a 10 MHz channel', () => {
    chanDom('10');
    S.setMeasTabSel('chan');
    S.traces[0].powers = flat(-60);
    S.traces[1].powers = flat(-90); // a quieter inactive trace must be ignored
    measureChannel();
    const res = S.chanRes;
    expect(res).not.toBeNull();
    expect(res!.mainDbm).toBeCloseTo(-40, 0);
    expect(res!.centerHz).toBeCloseTo(1.5e6);
  });

  it('normalization of the active trace clears the result with no fallback to trace 2', () => {
    chanDom('10');
    S.setMeasTabSel('chan');
    S.traces[0].powers = flat(-60);
    measureChannel();
    expect(S.chanRes!.mainDbm).toBeCloseTo(-40, 0);

    normalizeActiveTrace();
    expect(S.traces[0].isNormalized).toBe(true);
    // The active trace now has no absolute powers; trace 2's display must NOT be used.
    expect(getActiveAbsolutePowers()).toBeNull();
    measureChannel();
    expect(S.chanRes).toBeNull();
  });

  it('RTA mode reads rtaDisplays, and the swept channel handler does not run there', () => {
    chanDom('10');
    S.setRtaMode(true);
    const rta = flat(-50);
    S.setRtaDisplays([rta, flat(-90), null, null]);
    S.traces[0].powers = flat(-90); // stale swept display must be ignored
    const active = getActiveAbsolutePowers();
    expect(active).toBe(rta);

    // The swept channel handler is guarded off in RTA: the stored result is untouched.
    S.setChanRes({
      centerHz: 1, channelBw: 1, obwPercent: 99, acpOffset: 0, acpBw: 1,
      mainDbm: null, lowerDbc: null, upperDbc: null, obw: null, obwLow: null, obwHigh: null,
    });
    measureChannel();
    expect(S.chanRes!.centerHz).toBe(1);
    S.setRtaMode(false);
  });
});

describe('normalization calibration loop', () => {
  it('a flat -60 reference normalizes to ~0 and stays there; the window rebuild keeps it absolute', () => {
    const repaint = vi.fn();
    setRenderer(repaint);
    S.traces[0].powers = flat(-60);

    normalizeActiveTrace();
    expect(displayUnit.get()).toBe('dB');
    expect(getDisplayRef()).toBe(0);
    expect(Array.from(S.traces[0].reference!)).toEqual(Array.from(flat(-60)));
    const repaintsAfterNorm = repaint.mock.calls.length;
    expect(repaintsAfterNorm).toBeGreaterThan(0);

    processTraces(flat(-60));
    expect(Array.from(S.traces[0].powers!).every(v => Math.abs(v) < 1e-4)).toBe(true);
    // A second identical frame must not drift off zero.
    processTraces(flat(-60));
    expect(Array.from(S.traces[0].powers!).every(v => Math.abs(v) < 1e-4)).toBe(true);

    // Rebuilding the reference window works from the stored ABSOLUTE reference (-60),
    // never from the relative display (which is ~0 and would silently re-baseline).
    setNormalizeReferenceWindow(7);
    expect(repaint.mock.calls.length).toBeGreaterThan(repaintsAfterNorm);
    expect(Array.from(S.traces[0].reference!)).toEqual(Array.from(flat(-60)));
    processTraces(flat(-60));
    expect(Array.from(S.traces[0].powers!).every(v => Math.abs(v) < 1e-4)).toBe(true);

    // Reset restores the absolute domain and repaints.
    const repaintsBeforeReset = repaint.mock.calls.length;
    resetActiveTraceNormalize();
    expect(repaint.mock.calls.length).toBeGreaterThan(repaintsBeforeReset);
    expect(S.traces[0].isNormalized).toBe(false);
    expect(S.traces[0].reference).toBeNull();
    expect(displayUnit.get()).toBe('dBm');
    expect(getDisplayRef()).toBe(refLevel.get());
  });
});

describe('limit CSV export', () => {
  it('reports the real 10 dB margin of the active absolute trace against a -50 limit', () => {
    const downloads = armLimitDownloadCapture();
    S.traces[0].powers = flat(-40);
    S.setLimits({
      on: true, tol: 0,
      points: [{ freqHz: 1e6, level: -50 }, { freqHz: 2e6, level: -50 }],
    });
    exportLimitCsv();
    expect(downloads).toHaveLength(1);
    const lines = downloads[0].text.split('\n');
    expect(lines[0]).toBe('# limit_violations');
    // Every bin violates by exactly 10 dB (data rows start with the numeric frequency).
    const rows = lines.filter(l => /^\d/.test(l));
    expect(rows).toHaveLength(N);
    for (const row of rows) {
      const [, level, limit, margin] = row.split(',');
      expect(Number(level)).toBeCloseTo(-40, 3);
      expect(Number(limit)).toBeCloseTo(-50, 3);
      expect(Number(margin)).toBeCloseTo(10, 3);
    }
  });

  it('a normalized active trace downloads nothing and never falls back to trace 2', () => {
    const downloads = armLimitDownloadCapture();
    S.traces[0].powers = flat(-40);
    S.traces[1].powers = flat(-10); // would violate hard if used as a fallback
    S.setLimits({
      on: true, tol: 0,
      points: [{ freqHz: 1e6, level: -50 }, { freqHz: 2e6, level: -50 }],
    });

    normalizeActiveTrace(); // the active trace becomes relative (~0 dB)
    exportLimitCsv();
    expect(downloads).toHaveLength(0);
  });
});

describe('native measurement availability', () => {
  function mountControls(): HTMLButtonElement[] {
    document.body.innerHTML = `
      <section id="meas-panel">
        <button id="btn-meas-onoff">Off</button>
        <button id="btn-pnm-set" data-action="meas-pnm">Set</button>
        <button id="btn-pnm-thr" data-action="meas-pnm">Set threshold</button>
        <button id="btn-pnm-avg" data-action="meas-pnm">Apply average</button>
      </section>
      <button id="btn-waterfall">Waterfall</button>
      <button id="btn-wf-pause">Pause</button>`;
    return Array.from(document.querySelectorAll<HTMLButtonElement>('[data-action="meas-pnm"]'));
  }

  afterEach(() => {
    S.setMeasOn(false);
    S.setRtaMode(false);
    S.setSdrMode(false);
    graphMode.confirm('std');
    deviceMode.confirm('std');
    S.setViewMode('std');
    applyMeasUI();
    waterfallOn.set(false);
  });

  it('disables every PNM Set and Apply button while Off and restores them when On', () => {
    const buttons = mountControls();
    deviceMode.confirm('std');
    S.setMeasOn(false);
    setMeasButtons(false);
    for (const button of buttons) expect(button.disabled, button.id).toBe(true);
    expect((document.getElementById('btn-meas-onoff') as HTMLButtonElement).disabled).toBe(false);

    S.setMeasOn(true);
    setMeasButtons(true);
    for (const button of buttons) expect(button.disabled, button.id).toBe(false);
  });

  it.each(['rta', 'sdr'] as const)('%s refuses measurement handlers without taking over waterfall', mode => {
    const buttons = mountControls();
    graphMode.confirm(mode);
    deviceMode.confirm(mode);
    S.setRtaMode(mode === 'rta');
    S.setSdrMode(mode === 'sdr');
    S.setViewMode(mode);
    S.setMeasOn(false);
    waterfallOn.set(true);
    S.setAmpRes(null);
    S.setChanRes(null);
    S.traces[0].powers = flat(-60);

    applyMeasUI();
    measToggle();
    S.setMeasTabSel('amp');
    measureAmp();
    S.setMeasTabSel('chan');
    measureChannel();
    expect(S.measOn).toBe(false);
    expect(waterfallOn.get()).toBe(true);
    expect(S.ampRes).toBeNull();
    expect(S.chanRes).toBeNull();
    for (const button of buttons) expect(button.disabled, button.id).toBe(true);
    expect((document.getElementById('btn-waterfall') as HTMLButtonElement).disabled).toBe(false);

    // A stale On state also cannot let a measurement consume the unsupported view.
    S.setMeasOn(true);
    applyMeasUI();
    S.setMeasTabSel('amp');
    measureAmp();
    S.setMeasTabSel('chan');
    measureChannel();
    expect(waterfallOn.get()).toBe(true);
    expect(S.ampRes).toBeNull();
    expect(S.chanRes).toBeNull();

    graphMode.confirm('std');
    deviceMode.confirm('std');
    S.setRtaMode(false);
    S.setSdrMode(false);
    S.setViewMode('std');
    applyMeasUI();
    for (const button of buttons) expect(button.disabled, button.id).toBe(false);
  });
});
