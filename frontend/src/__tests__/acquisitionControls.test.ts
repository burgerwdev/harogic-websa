import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../core/wsSend', () => ({ send: vi.fn(), onCommandSent: vi.fn() }));

import * as S from '../core/store';
import { send } from '../core/wsSend';
import { presetAll, rejectPresetRequest, syncGraphModeStatus, syncPresetStatus } from '../ui/controls';
import { centerHz, spanHz } from '../ui/freqState';
import { resetGraphMode } from '../ui/graphMode';
import { beginFrequencyCommit, resetFrequencyEditors } from '../ui/panels/frequency';
import { axisWheel, previewLevelWindow, resetAxisPreview } from '../ui/axisDrag';
import { setScale } from '../ui/panels/refAmp';
import { rtaHalflife, rtaDensSource, rtaDensLayers, rtaDensContrast, rtaDensWidth, rtaAmpBins } from '../ui/waterfallState';
import { resetRtaDensityCfg } from '../ui/panels/rta';
import { refLevel } from '../ui/refState';
import { SDR_DEFAULT_AVG, rememberTraceMode, resetTraceFamilyModes, setTraceAverage, setTraceMode, switchTraceTab, toggleFreeze } from '../ui/traceOps';

beforeEach(() => {
  vi.useFakeTimers();
  resetGraphMode();
  S.setRtaMode(false);
  S.setSdrMode(false);
  resetTraceFamilyModes();
  resetFrequencyEditors();
  resetAxisPreview();
  resetRtaDensityCfg();
  S.setDbPerDiv(10);
  centerHz.confirm(1e9);
  spanHz.confirm(100e6);
  vi.mocked(send).mockClear();
});

afterEach(() => {
  vi.advanceTimersByTime(3000);
  resetAxisPreview();
  resetRtaDensityCfg();
  S.setDbPerDiv(10);
  resetFrequencyEditors();
  resetGraphMode();
  S.setRtaMode(false);
  S.setSdrMode(false);
  document.body.replaceChildren();
  vi.clearAllTimers();
  vi.useRealTimers();
});

function requestPreset(): string {
  presetAll();
  const command = vi.mocked(send).mock.calls.at(-1)![0] as { cmd: string; request_id: string };
  expect(command.cmd).toBe('SET_PRESET');
  return command.request_id;
}

function pendingFrequency() {
  const editor = document.createElement('div');
  editor.id = 'swp-freq-settings';
  document.body.append(editor);
  beginFrequencyCommit('swp-freq-settings', { center: 1.2e9, span: 80e6 });
  return editor;
}

describe('preset acknowledgement boundary', () => {
  it('retains acquisition intent on click, old replies and a matching error', () => {
    const editor = pendingFrequency();
    const a = requestPreset();
    const b = requestPreset();
    expect(a).not.toBe(b);
    expect(syncPresetStatus('SET_PRESET', a)).toBe(false);
    rejectPresetRequest('SET_PRESET', a);
    expect(centerHz.get()).toBe(1.2e9);
    expect(editor.dataset.pending).toBe('1');
    rejectPresetRequest('SET_PRESET', b);
    expect(syncPresetStatus('SET_PRESET', b)).toBe(false);
    expect(centerHz.get()).toBe(1.2e9);
    expect(editor.dataset.pending).toBe('1');
  });

  it('resets acquisition state only once for the matching success', () => {
    const editor = pendingFrequency();
    const requestId = requestPreset();
    expect(syncPresetStatus('SET_FREQ', requestId)).toBe(false);
    expect(syncPresetStatus('SET_PRESET', 42)).toBe(false);
    expect(syncPresetStatus('SET_PRESET', requestId)).toBe(true);
    expect(centerHz.get()).toBe(1e9);
    expect(spanHz.get()).toBe(100e6);
    expect(editor.dataset.pending).toBeUndefined();
    expect(syncPresetStatus('SET_PRESET', requestId)).toBe(false);
  });

  it('does not apply a preset success arriving after its request expired', () => {
    const requestId = requestPreset();
    vi.advanceTimersByTime(3000);
    const editor = pendingFrequency();
    expect(syncPresetStatus('SET_PRESET', requestId)).toBe(false);
    expect(editor.dataset.pending).toBe('1');
    expect(centerHz.get()).toBe(1.2e9);
  });

  it.each(['sdr', 'rta', 'std'])('resets every trace and the visible panel to %s factory defaults', mode => {
    document.body.innerHTML = '<input id="input-offset">' +
      '<button class="trace-btn"></button><button class="trace-btn"></button>' +
      '<select id="select-trace-mode"><option>CLEAR_WRITE</option><option>AVERAGE</option><option>MAX_HOLD</option><option>OFF</option></select>' +
      '<div id="trace-avg-row"><select id="select-trace-avg"><option>16</option><option>64</option><option>0</option></select></div>' +
      '<span id="trace-avg-status"></span><button id="btn-view-freeze"></button>';
    syncGraphModeStatus(mode);
    switchTraceTab(1);
    setTraceMode('MAX_HOLD');
    rememberTraceMode(mode === 'sdr');
    setTraceAverage(0);
    toggleFreeze();
    S.traces.forEach((trace, i) => {
      trace.avgTarget = 64;
      trace.avgTargetRta = 0;
      trace.avgCount = 9;
      trace.avgSum = new Float32Array([9]);
      S.rtaAvgN[i] = 9;
      S.rtaAvgSum[i] = new Float32Array([9]);
      S.rtaDone[i] = true;
      S.rtaDisplays[i] = new Float32Array([-65]);
    });
    const id = requestPreset();
    expect(S.traces[1].mode).toBe('VIEW');
    expect(S.rtaAvgN[1]).toBe(9);
    expect(syncPresetStatus('SET_PRESET', id)).toBe(true);
    const expectedMode = mode === 'sdr' ? 'AVERAGE' : 'CLEAR_WRITE';
    expect(S.activeTraceIdx).toBe(0);
    expect(S.traces.map(trace => trace.mode)).toEqual(S.traces.map((_, i) => i === 0 ? expectedMode : 'OFF'));
    for (const [i, trace] of S.traces.entries()) {
      expect(trace.avgTargetRta).toBe(SDR_DEFAULT_AVG);
      expect(trace.avgCount).toBe(0);
      expect(trace.avgSum).toBeNull();
      expect(trace.prevMode).toBeUndefined();
      expect(S.rtaAvgN[i]).toBe(0);
      expect(S.rtaAvgSum[i]).toBeNull();
      expect(S.rtaDone[i]).toBe(false);
      expect(S.rtaDisplays[i]).toBeNull();
    }
    expect((document.getElementById('select-trace-mode') as HTMLSelectElement).value).toBe(expectedMode);
    expect((document.getElementById('select-trace-avg') as HTMLSelectElement).value).toBe(String(SDR_DEFAULT_AVG));
    expect(document.getElementById('trace-avg-row')!.style.display).toBe(mode === 'sdr' ? '' : 'none');
    expect(document.getElementById('trace-avg-status')!.textContent).toBe(mode === 'sdr' ? String(SDR_DEFAULT_AVG) : '');
    expect(document.getElementById('btn-view-freeze')!.classList.contains('active')).toBe(false);
  });

  it('restores the display only after the matching preset acknowledgement', () => {
    setScale(7.8405096842395166);
    rtaHalflife.set(0);
    rtaDensLayers.set('density');
    rtaDensSource.set('live');
    rtaDensContrast.set(0.6);
    rtaDensWidth.set(9);
    rtaAmpBins.set(192);
    const rejected = requestPreset();
    rejectPresetRequest('SET_PRESET', rejected);
    expect(S.dbPerDiv).toBe(7.8405096842395166);
    expect(rtaDensLayers.get()).toBe('density');
    expect(rtaDensWidth.get()).toBe(9);
    const accepted = requestPreset();
    expect(syncPresetStatus('SET_PRESET', rejected)).toBe(false);
    expect(S.dbPerDiv).toBe(7.8405096842395166);
    expect(syncPresetStatus('SET_PRESET', accepted)).toBe(true);
    expect(S.dbPerDiv).toBe(10);
    expect([rtaHalflife.get(), rtaDensLayers.get(), rtaDensSource.get(), rtaDensContrast.get(), rtaDensWidth.get(), rtaAmpBins.get()])
      .toEqual([0.5, 'both', 'trace', 1, 5, 128]);
  });

  it('cancels wheel commits before and during a preset request', () => {
    const rect = { x: 10, y: 14, w: 800, h: 400 };
    axisWheel('y', -100, 825, 120, rect);
    const id = requestPreset();
    expect(previewLevelWindow()).toBeNull();
    vi.mocked(send).mockClear();
    vi.advanceTimersByTime(500);
    expect(send).not.toHaveBeenCalled();
    axisWheel('y', -100, 825, 120, rect);
    expect(syncPresetStatus('SET_PRESET', id)).toBe(true);
    vi.mocked(send).mockClear();
    vi.advanceTimersByTime(6000);
    expect(previewLevelWindow()).toBeNull();
    expect(S.dbPerDiv).toBe(10);
    expect(send).not.toHaveBeenCalled();
  });

  it('retires an in-flight reference when Preset succeeds', () => {
    refLevel.set(-140);
    const id = requestPreset();
    expect(refLevel.desiredValue()).toBe(-140);
    expect(syncPresetStatus('SET_PRESET', id)).toBe(true);
    expect(refLevel.desiredValue()).toBeNull();
  });
});

describe('acquisition mode gating', () => {
  it('keeps unsupported RTA controls disabled after the enclosing panel is enabled', () => {
    const ids = ['input-points', 'btn-points', 'select-window', 'select-detector',
      'select-framedet', 'select-tracealign', 'select-points-strategy', 'select-spur'];
    const panel = document.createElement('div');
    panel.id = 'sweep-panel';
    for (const id of ids) {
      const control = document.createElement(id.startsWith('input') ? 'input' : id.startsWith('btn') ? 'button' : 'select');
      control.id = id;
      panel.append(control);
    }
    const rbw = document.createElement('select');
    rbw.id = 'select-rbw-mode';
    for (const value of ['auto', 'manual', 'ratio10', 'ratio100']) {
      const option = document.createElement('option');
      option.value = value;
      rbw.append(option);
    }
    panel.append(rbw);
    document.body.append(panel);
    syncGraphModeStatus('rta');
    for (const id of ids) expect((document.getElementById(id) as HTMLInputElement).disabled, id).toBe(true);
    expect(Array.from(rbw.options).map(option => option.disabled)).toEqual([false, false, true, true]);
    syncGraphModeStatus('std');
    for (const id of ids) expect((document.getElementById(id) as HTMLInputElement).disabled, id).toBe(false);
    expect(Array.from(rbw.options).map(option => option.disabled)).toEqual([false, false, false, false]);
  });
});
