import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { setWS } from '../core/wsSend';
import { unitMap } from '../core/units';
import { updateFreqUIInputs } from '../ui/freqInputs';
import { confirmGraphMode, deviceMode, requestGraphMode, resetGraphMode } from '../ui/graphMode';
import { currentRBW, currentVBW, rbwMode, vbwMode } from '../ui/swpState';
import {
  applyRBW, applyVBW, confirmResolutionStatus, markResolutionDirty,
  rejectResolutionCommit, resetResolutionEditors, syncResolutionAvailability,
} from '../ui/panels/resolution';

const sent: Array<Record<string, any>> = [];
const input = (field: 'rbw' | 'vbw') => document.getElementById(`input-${field}`) as HTMLInputElement;
const select = (field: 'rbw' | 'vbw') => document.getElementById(`select-${field}-mode`) as HTMLSelectElement;
const report = (values: Record<string, any> = {}) => confirmResolutionStatus({
  mode: 'std', rbw: 300000, rbw_mode: 'auto', vbw: 300000, vbw_mode: 'bypass', ...values,
});
function edit(field: 'rbw' | 'vbw', value: string): void {
  input(field).value = value;
  markResolutionDirty(field);
}
function submit(field: 'rbw' | 'vbw', value: string): string {
  edit(field, value);
  select(field).value = 'manual';
  (field === 'rbw' ? applyRBW : applyVBW)();
  return sent[sent.length - 1].request_id;
}

beforeEach(() => {
  vi.useFakeTimers();
  resetGraphMode();
  confirmGraphMode('std');
  deviceMode.confirm('std');
  unitMap.reset();
  document.body.innerHTML = `
    <input id="input-rbw" type="number"><input id="input-vbw" type="number">
    <select id="select-rbw-mode"><option>auto</option><option>manual</option><option>one_thousandth_span</option><option>one_percent_span</option></select>
    <select id="select-vbw-mode"><option>bypass</option><option>equal</option><option>tenth</option><option>manual</option></select>
    <button data-action="apply-rbw"></button><button data-action="apply-vbw"></button>`;
  resetResolutionEditors();
  report();
  sent.length = 0;
  setWS({ readyState: WebSocket.OPEN, send: (text: string) => sent.push(JSON.parse(text)) } as unknown as WebSocket);
});

afterEach(() => {
  resetResolutionEditors();
  resetGraphMode();
  deviceMode.confirm('std');
  setWS(null);
  vi.useRealTimers();
  document.body.innerHTML = '';
});

describe('resolution editor transactions', () => {
  it.each(['rbw', 'vbw'] as const)('preserves a blurred %s draft through ordinary STATUS refresh', field => {
    input(field).focus();
    edit(field, '123.45');
    input(field).blur();
    report({ rbw: 450000, vbw: 600000 });
    updateFreqUIInputs(true);
    expect(input(field).value).toBe('123.45');
    expect(input(field).dataset.edited).toBe('1');
    expect(sent).toEqual([]);
  });

  it('keeps numeric desire and selected mode pending until the same correlated ACK', () => {
    const requestId = submit('rbw', '100');
    expect(currentRBW.get()).toBe(100000);
    expect(rbwMode.get()).toBe('manual');
    report({ rbw: 100000, rbw_mode: 'auto' });
    expect(currentRBW.pending()).toBe(true);
    expect(rbwMode.pending()).toBe(true);
    report({ response_to: 'SET_RBW', request_id: requestId, rbw: 96000, rbw_mode: 'manual' });
    expect(currentRBW.pending()).toBe(false);
    expect(rbwMode.pending()).toBe(false);
    expect(currentRBW.get()).toBe(96000);
    expect(input('rbw').value).toBe('96.00');
  });

  it('ignores old ACKs and ERRORs before and after the latest request settles', () => {
    const first = submit('rbw', '100');
    const second = submit('rbw', '200');
    report({ response_to: 'SET_RBW', request_id: first, rbw: 100000, rbw_mode: 'manual' });
    rejectResolutionCommit('SET_RBW', first);
    expect(currentRBW.get()).toBe(200000);
    expect(input('rbw').dataset.pending).toBe('1');
    report({ response_to: 'SET_RBW', request_id: second, rbw: 192000, rbw_mode: 'manual' });
    report({ response_to: 'SET_RBW', request_id: first, rbw: 100000, rbw_mode: 'manual' });
    rejectResolutionCommit('SET_RBW', first);
    expect(currentRBW.get()).toBe(192000);
    expect(input('rbw').value).toBe('192.00');
  });

  it('requires request identity even when a stale readback equals the newest desire', () => {
    submit('vbw', '100');
    const latest = submit('vbw', '200');
    report({ response_to: 'SET_VBW', request_id: 123, vbw: 200000, vbw_mode: 'manual' });
    report({ response_to: 'SET_VBW', vbw: 200000, vbw_mode: 'manual' });
    expect(currentVBW.pending()).toBe(true);
    expect(vbwMode.pending()).toBe(true);
    report({ response_to: 'SET_VBW', request_id: latest, vbw: 180000, vbw_mode: 'manual' });
    expect(currentVBW.get()).toBe(180000);
    expect(currentVBW.pending()).toBe(false);
  });

  it.each(['rbw', 'vbw'] as const)('keeps %s edits typed after submission when its ACK arrives', field => {
    const requestId = submit(field, '100');
    edit(field, '250');
    report({ response_to: `SET_${field.toUpperCase()}`, request_id: requestId,
      [field]: 96000, [`${field}_mode`]: 'manual' });
    updateFreqUIInputs(true);
    expect(input(field).value).toBe('250');
    expect(input(field).dataset.edited).toBe('1');
    expect(input(field).dataset.pending).toBeUndefined();
  });

  it('rolls B back to accepted A without letting A clear an equal B desire', () => {
    const first = submit('rbw', '100');
    const second = submit('rbw', '100');
    report({ response_to: 'SET_RBW', request_id: first, rbw: 96000, rbw_mode: 'manual' });
    expect(currentRBW.get()).toBe(100000);
    expect(currentRBW.pending()).toBe(true);
    expect(rbwMode.pending()).toBe(true);
    rejectResolutionCommit('SET_RBW', second);
    expect(currentRBW.get()).toBe(96000);
    expect(rbwMode.get()).toBe('manual');
    expect(input('rbw').value).toBe('96.00');
  });

  it('keeps the newest accepted predecessor as rollback when older replies arrive out of order', () => {
    const first = submit('vbw', '100');
    const second = submit('vbw', '200');
    const third = submit('vbw', '300');
    report({ response_to: 'SET_VBW', request_id: second, vbw: 180000, vbw_mode: 'manual' });
    report({ response_to: 'SET_VBW', request_id: first, vbw: 90000, vbw_mode: 'manual' });
    rejectResolutionCommit('SET_VBW', third);
    expect(currentVBW.get()).toBe(180000);
    expect(input('vbw').value).toBe('180.00');
  });

  it('rolls back only the rejected transaction and preserves its newer draft', () => {
    const rbwId = submit('rbw', '100');
    submit('vbw', '50');
    edit('rbw', '200');
    rejectResolutionCommit('SET_RBW', rbwId);
    expect(currentRBW.get()).toBe(300000);
    expect(rbwMode.get()).toBe('auto');
    expect(input('rbw').value).toBe('200');
    expect(input('rbw').dataset.edited).toBe('1');
    expect(currentVBW.get()).toBe(50000);
    expect(currentVBW.pending()).toBe(true);
  });

  it('rolls back a timed out numeric and mode intent together without deleting a newer draft', () => {
    submit('rbw', '100');
    edit('rbw', '240');
    vi.advanceTimersByTime(3000);
    expect(currentRBW.get()).toBe(300000);
    expect(rbwMode.get()).toBe('auto');
    expect(currentRBW.pending()).toBe(false);
    expect(rbwMode.pending()).toBe(false);
    expect(input('rbw').value).toBe('240');
    expect(input('rbw').dataset.pending).toBeUndefined();
  });

  it('switches from manual to auto without retaining the old numeric desire', () => {
    submit('rbw', '100');
    select('rbw').value = 'auto';
    applyRBW();
    expect(currentRBW.desiredValue()).toBeNull();
    expect(rbwMode.get()).toBe('auto');
    const latest = sent[sent.length - 1];
    expect(latest.mode).toBe('auto');
    expect(latest.rbw).toBeUndefined();
    report({ response_to: 'SET_RBW', request_id: latest.request_id, rbw: 420000 });
    expect(input('rbw').value).toBe('420.00');
  });

  it('resets pending editors and rejects their delayed readback after Preset', () => {
    const requestId = submit('rbw', '100');
    resetResolutionEditors();
    report({ rbw: 500000 });
    report({ response_to: 'SET_RBW', request_id: requestId, rbw: 100000, rbw_mode: 'manual' });
    expect(currentRBW.get()).toBe(500000);
    expect(input('rbw').value).toBe('500.00');
    expect(input('rbw').dataset.edited).toBeUndefined();
  });

  it('uses RTA readback and refuses span ratio modes instead of silently sending auto', () => {
    confirmGraphMode('rta');
    deviceMode.confirm('rta');
    report({ mode: 'rta', rbw: 24000, rbw_mode: 'auto', vbw: 12000, vbw_mode: 'equal' });
    expect(currentRBW.get()).toBe(24000);
    expect(vbwMode.get()).toBe('equal');
    expect(Array.from(select('rbw').options).find(option => option.value === 'one_percent_span')?.disabled).toBe(true);
    select('rbw').value = 'one_percent_span';
    applyRBW();
    expect(sent).toEqual([]);
    expect(select('rbw').validity.customError).toBe(true);
    select('rbw').value = 'manual';
    edit('rbw', '10');
    applyRBW();
    expect(sent[0]).toMatchObject({ cmd: 'SET_RBW', mode: 'manual', rbw: 10000 });
  });

  it('gates acquisition actions during SDR, a measurement, or a pending mode switch', () => {
    confirmGraphMode('sdr');
    deviceMode.confirm('sdr');
    syncResolutionAvailability('sdr');
    applyRBW(); applyVBW();
    expect(input('rbw').disabled).toBe(true);
    expect(select('vbw').disabled).toBe(true);
    confirmGraphMode('std');
    deviceMode.confirm('harmonic');
    applyRBW(); applyVBW();
    deviceMode.confirm('std');
    requestGraphMode('rta');
    applyRBW(); applyVBW();
    expect(sent).toEqual([]);
  });
});
