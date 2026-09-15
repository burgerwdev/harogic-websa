/**
 * VSA client state: the mode handshake, the parameter slots, the measurement payload store and
 * the ambiguity control.
 *
 * The VSA panel is the only place a user can answer the 4-fold carrier ambiguity, so these
 * tests pin that the rotation really is sent as `SET_VSA{phase_rot}` (and wrapped), that a
 * capture-only measurement cannot be selected while streaming, and that a constellation frame
 * is a snapshot (newest wins) rather than a queue.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const sent: any[] = [];
vi.mock('../core/wsSend', () => ({ send: (msg: any) => { sent.push(msg); } }));

import {
  VSA_CAPTURE_ONLY,
  applyVsa,
  hasStoredVsaPrefs,
  resetVsaState,
  setVsaPayload,
  setVsaPhaseRotation,
  setVsaStatus,
  vsaCenterHz,
  vsaDecimate,
  vsaDepth,
  vsaMeasure,
  vsaMetrics,
  vsaPhaseRotDeg,
  vsaView,
  getVsaPayload,
  vsaPayloadSeq,
} from '../core/vsaState';
import { decodeVsad, VSA_KINDS, type VsaFrame } from '../core/frames';

/**
 * Build a VSAD buffer in the test (the decoder itself is locked against the Python-generated
 * fixture in frames.test.ts, so this only has to produce *shapes*: a 2-column payload, an
 * ideal grid and the positional measurement block).
 */
function vsadBytes(measure: string, rows: number, cols = 2) {
  const kind = VSA_KINDS.indexOf(measure as (typeof VSA_KINDS)[number]);
  const ideal = [[0.1, 0.1], [-0.1, 0.1], [-0.1, -0.1], [0.1, -0.1]];
  const keys = 15;
  const size = 48 + (rows * cols + ideal.length * 2 + 1 + keys) * 4;
  const buffer = new ArrayBuffer(size);
  const view = new DataView(buffer);
  const ascii = (offset: number, text: string) => {
    for (let i = 0; i < text.length; i++) view.setUint8(offset + i, text.charCodeAt(i));
  };
  ascii(0, 'VSAD');
  view.setUint32(4, 3, true);
  view.setUint32(8, kind, true);
  view.setUint32(12, rows, true);
  view.setUint32(16, cols, true);
  view.setUint32(20, ideal.length, true);
  view.setUint32(24, 2, true);
  [250e3, 12.5, 0.4, NaN, 21].forEach((v, i) => view.setFloat32(28 + i * 4, v, true));
  let at = 48;
  for (let i = 0; i < rows * cols; i++, at += 4) {
    view.setFloat32(at, ((i % 7) / 10) - 0.3, true);
  }
  for (const point of ideal) {
    for (const v of point) { view.setFloat32(at, v, true); at += 4; }
  }
  view.setUint32(at, keys, true); at += 4;
  for (let i = 0; i < keys; i++, at += 4) view.setFloat32(at, i === 0 ? -25.5 : NaN, true);
  return buffer;
}

function frame(measure = 'constellation', rows = 2, cols = 2): VsaFrame {
  return decodeVsad(vsadBytes(measure, rows, cols))!;
}

/** A minimal 2-D context so the drawing branches run (jsdom has no canvas backend). */
function stubContext(): string[] {
  const calls: string[] = [];
  const ctx: any = {
    fillStyle: '', strokeStyle: '', lineWidth: 1, font: '',
    fillRect: () => calls.push('fillRect'),
    strokeRect: () => calls.push('strokeRect'),
    beginPath: () => calls.push('beginPath'),
    moveTo: () => calls.push('moveTo'),
    lineTo: () => calls.push('lineTo'),
    stroke: () => calls.push('stroke'),
    fillText: () => calls.push('fillText'),
    createImageData: (w: number, h: number) =>
      ({ data: new Uint8ClampedArray(w * h * 4), width: w, height: h }),
    putImageData: () => calls.push('putImageData'),
  };
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(ctx);
  return calls;
}

beforeEach(() => {
  sent.length = 0;
  localStorage.clear();
  resetVsaState();
  document.body.innerHTML = `
    <input id="input-vsa-center" value="100.2">
    <select id="select-vsa-decimate"><option value="16">16</option></select>
    <select id="select-vsa-depth"><option value="131072">128k</option></select>
    <select id="select-vsa-view"><option value="capture">capture</option><option value="stream">stream</option></select>
    <select id="select-vsa-measure">
      <option value="spectrum">spectrum</option><option value="power">power</option>
      <option value="ccdf">ccdf</option><option value="spectrogram">spectrogram</option>
      <option value="constellation">constellation</option>
    </select>
    <select id="select-vsa-modulation"><option value="qpsk">qpsk</option></select>
    <input id="input-vsa-symbol-rate" value="">
    <input id="input-vsa-rolloff" value="0.35">
    <input id="input-vsa-phase-rot" value="0">
    <span id="vsa-metric-ambiguity"></span><span id="vsa-ambiguity-note"></span>
    <span id="vsa-metric-kind"></span><span id="vsa-metric-rate"></span>
    <span id="vsa-metric-cfo"></span><span id="vsa-metric-timing"></span>
    <span id="vsa-metric-evm"></span><span id="vsa-metric-mer"></span>
    <span id="vsa-metric-ser"></span><span id="vsa-metric-symbols"></span>
    <span id="vsa-metric-samples"></span><span id="vsa-metric-error"></span>
    <progress id="vsa-progress"></progress>
    <canvas id="vsa-constellation" width="260" height="200"></canvas>
  `;
});

describe('VSA parameters', () => {
  it('applyVsa sends the whole geometry in one SET_VSA', () => {
    (document.getElementById('input-vsa-center') as HTMLInputElement).value = '433.92';
    (document.getElementById('select-vsa-depth') as HTMLSelectElement).value = '131072';
    (document.getElementById('select-vsa-measure') as HTMLSelectElement).value = 'ccdf';
    (document.getElementById('input-vsa-rolloff') as HTMLInputElement).value = '0.25';
    applyVsa();
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({
      cmd: 'SET_VSA', center: 433.92e6, decimate: 16, depth: 131072,
      measure: 'ccdf', modulation: 'qpsk', rolloff: 0.25, view: 'capture',
    });
    expect(vsaCenterHz.get()).toBeCloseTo(433.92e6, 3);
    expect(vsaMeasure.get()).toBe('ccdf');
  });

  it('a STATUS confirms the slots and copies the measurement summary', () => {
    setVsaStatus({
      center: 100.2e6, decimate: 8, depth: 262144, measure: 'constellation',
      modulation: '16qam', rolloff: 0.3, symbol_rate: 250e3, phase_rot: 90, view: 'capture',
      actual: { bandwidth: 6.25e6, packet_samples: 16240 },
      busy: false, progress: 1,
      last: { kind: 'constellation', evm_percent: 2.4, mer_db: 32.4, ser: 0,
        symbols_n: 4096, resolved_by: 'user', rotation_deg: 90 },
    });
    expect(vsaCenterHz.confirmedValue()).toBe(100.2e6);
    expect(vsaDecimate.confirmedValue()).toBe(8);
    expect(vsaDepth.confirmedValue()).toBe(262144);
    expect(vsaPhaseRotDeg.confirmedValue()).toBe(90);
    expect(vsaMetrics.evm_percent).toBe(2.4);
    expect(document.getElementById('vsa-metric-evm')!.textContent).toContain('2.40');
    expect(document.getElementById('vsa-metric-ambiguity')!.textContent).toContain('user');
  });

  it('the phase-rotation control sends SET_VSA{phase_rot} and wraps', () => {
    setVsaPhaseRotation(360 + 90);
    expect(sent.at(-1)).toMatchObject({ cmd: 'SET_VSA', phase_rot: 90 });
    setVsaPhaseRotation(-90);
    expect(sent.at(-1)).toMatchObject({ cmd: 'SET_VSA', phase_rot: 270 });
    expect(vsaPhaseRotDeg.get()).toBe(270);
  });

  it('stores preferences under the VSA keys only', () => {
    setVsaStatus({ center: 100.2e6, decimate: 16, depth: 1 << 17, view: 'capture' });
    expect(hasStoredVsaPrefs()).toBe(true);
    resetVsaState();
    expect(hasStoredVsaPrefs()).toBe(false);
  });

  it('a capture-only measurement is disabled while the stream view is selected', () => {
    (document.getElementById('select-vsa-view') as HTMLSelectElement).value = 'stream';
    applyVsa();
    const measure = document.getElementById('select-vsa-measure') as HTMLSelectElement;
    for (const kind of VSA_CAPTURE_ONLY) {
      const option = measure.querySelector(`option[value="${kind}"]`) as HTMLOptionElement;
      expect(option.disabled).toBe(true);
    }
    expect((measure.querySelector('option[value="ccdf"]') as HTMLOptionElement).disabled).toBe(false);
    expect(vsaView.get()).toBe('stream');
  });
});

describe('VSA payload store', () => {
  it('keeps the newest payload (a constellation is a snapshot)', () => {
    const first = frame('constellation', 2);
    const second = frame('constellation', 6);
    setVsaPayload(first);
    expect(getVsaPayload()).toBe(first);
    const seq = vsaPayloadSeq();
    setVsaPayload(second);
    expect(getVsaPayload()).toBe(second);
    expect(vsaPayloadSeq()).toBe(seq + 1);
    expect(getVsaPayload()!.rows).toBe(6);
  });

  it('draws the cloud against its ideal grid', () => {
    const calls = stubContext();
    setVsaPayload(frame('constellation', 8));
    expect(calls).toContain('strokeRect');                 // the nominal grid
    expect(calls.filter((c) => c === 'fillRect').length).toBeGreaterThan(3);
    expect(calls).toContain('fillText');                   // the symbol count
  });

  it('draws a curve for a trace and a heatmap for a spectrogram', () => {
    let calls = stubContext();
    setVsaPayload(frame('ccdf', 16));
    expect(calls).toContain('stroke');
    calls = stubContext();
    setVsaPayload(frame('spectrogram', 8, 4));
    expect(calls).toContain('putImageData');
  });

  it('says so when there is no measurement yet instead of drawing nothing', () => {
    const calls = stubContext();
    resetVsaState();
    expect(calls).toContain('fillText');
    expect(getVsaPayload()).toBeNull();
  });
});
