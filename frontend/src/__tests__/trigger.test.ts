import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { initTrigger } from '../ui/trigger';
import { resetTrigger, syncTriggerMode, confirmTriggerFromStatus, triggerFields, trigLevel, validTriggerValue } from '../ui/triggerState';
import { beginHardwareCapture, confirmTriggerMetadata, noteFrameArrived, prepareTriggerFrame, releaseHardwareCapture, resetTriggerConnection, resetTriggerRuntime, triggerHeld, triggerMissed } from '../ui/triggerEvents';
import { armSwpTrigger, evaluateSwpTrigger } from '../ui/swpTrigger';
import { displayOffset } from '../ui/displayState';
import { send } from '../core/wsSend';
vi.mock('../core/wsSend', () => ({ send: vi.fn(), onCommandSent: vi.fn() }));
vi.mock('../render/redraw', () => ({ requestRender: vi.fn() }));

const confirmed = { source: 'bus', edge: 'falling', level: -55.5, safetime: .1, delay: .2, pretime: .3, acqtime: .0005, retrigger: 2, retriggerperiod: 3, out: 'per_sweep', outpolarity: 'negative' };
function status(values = confirmed): void {
  confirmTriggerFromStatus({ ...Object.fromEntries(Object.entries(values).map(([key, value]) => [`trigger_${key}`, value])), trigger_actual: { ...values, valid: true, generation: 10, frames: 20 } });
}
function input(key: keyof typeof triggerFields): HTMLInputElement | HTMLSelectElement {
  return document.getElementById(triggerFields[key].id) as HTMLInputElement | HTMLSelectElement;
}
function change(key: keyof typeof triggerFields, value: string): void {
  input(key).value = value;
  input(key).dispatchEvent(new Event('change'));
}
function click(id: string): void { document.getElementById(id)!.click(); }

beforeAll(() => {
  vi.useFakeTimers();
  document.body.innerHTML = `<div id="trigger-panel">${Object.entries(triggerFields).map(([key, field]) => 'values' in field
    ? `<select id="${field.id}">${field.values.map((value) => `<option value="${value}">${value}</option>`).join('')}</select>`
    : `<input type="number" step="${key === 'retrigger' ? '1' : 'any'}" min="${field.min}" max="${field.max}" id="${field.id}">`).join('')}
    ${['capture', 'free', 'from-mkr', 'from-peak'].map((name) => `<button id="btn-trg-${name}"></button>`).join('')}</div>`;
  initTrigger();
});
beforeEach(() => {
  vi.useFakeTimers(); vi.mocked(send).mockClear();
  resetTriggerConnection();
  document.activeElement instanceof HTMLElement && document.activeElement.blur();
  syncTriggerMode('std'); resetTrigger();
  for (const trace of S.traces) { trace.raw = null; trace.powers = null; }
  S.setRtaMode(false); S.setRtaData(null); S.setActiveTraceIdx(0);
  S.markers.forEach((marker) => { marker.enabled = false; marker.idx = 0; });
  displayOffset.set(0); status();
});
afterEach(() => { resetTrigger(); vi.clearAllTimers(); vi.useRealTimers(); });

describe('trigger parameters and controls', () => {
  it('reflects all confirmed applied fields including external-client updates without sending commands', () => {
    syncTriggerMode('rta'); status();
    for (const [key, value] of Object.entries(confirmed)) expect(input(key as keyof typeof triggerFields).value).toBe(String(value));
    status({ ...confirmed, source: 'external', level: -61.25, edge: 'double', delay: 1.5 });
    expect(input('source').value).toBe('external'); expect(input('level').value).toBe('-61.25'); expect(input('delay').value).toBe('1.5');
    expect(send).not.toHaveBeenCalled();
  });
  it('keeps an uncommitted focused edit until blur, then reflects the latest confirmation', () => {
    input('level').focus(); input('level').value = '-12';
    status({ ...confirmed, level: -62 }); expect(input('level').value).toBe('-12');
    input('level').blur(); expect(input('level').value).toBe('-62');
  });
  it.each(['sdr', 'harmonic', 'pnm'])('disables every trigger action in %s and clears held state', (mode) => {
    armSwpTrigger(); S.setSwpHold(true);
    syncTriggerMode(mode);
    expect(S.swpHold).toBe(false); expect(S.trigArmed).toBe(false);
    for (const field of Object.values(triggerFields)) expect((document.getElementById(field.id) as HTMLInputElement).disabled).toBe(true);
    click('btn-trg-capture'); expect(send).not.toHaveBeenCalled(); expect(S.trigWaiting).toBe(false);
  });
  it('enables software Source, level and edge in standard mode', () => {
    for (const key of Object.keys(triggerFields) as (keyof typeof triggerFields)[]) expect(input(key).disabled).toBe(!['source', 'level', 'edge'].includes(key));
    change('delay', '1'); expect(send).not.toHaveBeenCalled(); expect(input('delay').value).toBe('0.2');
  });
  it('rejects invalid captures without clearing the picture or entering wait', () => {
    const powers = new Float32Array([-60, -50]); S.traces[0].raw = powers; S.traces[0].powers = powers;
    input('level').value = '100'; click('btn-trg-capture');
    expect(input('level').value).toBe('-55.5'); expect(S.traces[0].powers).toBe(powers);
    expect(S.trigWaiting).toBe(false); expect(send).not.toHaveBeenCalled();
  });
  it.each([['acqtime', '-1'], ['retrigger', '2.5'], ['retriggerperiod', '4000'], ['level', ''], ['delay', 'Infinity']] as const)('rejects %s=%s and restores its confirmation', (key, value) => {
    syncTriggerMode('rta'); status(); change(key, value);
    expect(input(key).value).toBe(String(confirmed[key])); expect(send).not.toHaveBeenCalled();
  });
  it('saves standard level and edge without changing the hardware source', () => {
    change('level', '-42'); change('edge', 'double');
    expect(trigLevel.get()).toBe(-42);
    expect(send).toHaveBeenCalledWith({ cmd: 'SET_TRIGGER', level: -42 });
    expect(send).toHaveBeenCalledWith({ cmd: 'SET_TRIGGER', edge: 'double' });
    status({ ...confirmed, level: -42, edge: 'double' });
    click('btn-trg-capture'); expect(S.trigWaiting).toBe(true); expect(trigLevel.get()).toBe(-42);
    expect(vi.mocked(send).mock.calls.some(([value]) => 'source' in value)).toBe(false);
  });
  it('arms and releases software capture through the SWP source selector without changing the device source', () => {
    change('source', 'level');
    expect(S.swpArmed).toBe(true);
    expect(input('source').value).toBe('level');
    status();
    expect(input('source').value).toBe('level');
    change('source', 'bus');
    expect(S.swpArmed).toBe(false);
    expect(input('source').value).toBe('bus');
    expect(vi.mocked(send).mock.calls.some(([value]) => 'source' in value)).toBe(false);
  });
  it('uses one capture transition for Source Level and Capture, and restores rejected edits', () => {
    syncTriggerMode('rta'); status();
    confirmTriggerMetadata({ source: 'bus', generation: 1, frames: 0 });
    change('source', 'level'); expect(S.trigWaiting).toBe(true);
    expect(vi.mocked(send).mock.calls.at(-1)?.[0]).toMatchObject({ cmd: 'SET_TRIGGER', source: 'level', level: -55.5 });
    releaseHardwareCapture();
    confirmTriggerMetadata({ source: 'level', generation: 3, valid: true }); expect(S.trigArmed).toBe(false);
    expect(prepareTriggerFrame({ source: 'level', generation: 3, complete: true, valid: true })).toBe('ignore');
    change('delay', '1.5'); expect(input('delay').value).toBe('1.5');
    resetTrigger(); expect(input('delay').value).toBe('0.2');
    confirmTriggerFromStatus({ trigger_delay: 9, trigger_actual: { ...confirmed, valid: true } });
    expect(input('delay').value).toBe('0.2');
  });
  it('does not present unknown real device settings as applied values', () => {
    syncTriggerMode('rta'); status();
    confirmTriggerFromStatus({ trigger_level: -20, trigger_actual: { valid: false, level: null, simulated: false } });
    expect(input('level').value).toBe('');
  });
  it('accepts schema boundary values without incidental HTML step restrictions', () => {
    expect(validTriggerValue('acqtime', '.0005')).toBe(.0005);
    expect(validTriggerValue('level', '-150')).toBe(-150);
    expect(validTriggerValue('retrigger', '65535')).toBe(65535);
  });
  it('uses the active marker and raw current-mode dBm for both threshold helpers', () => {
    S.traces[0].raw = new Float32Array([-90, -20, -35]); S.traces[0].powers = new Float32Array([10, 20, 30]);
    S.markers[0].enabled = true; S.markers[0].idx = 0; S.markers[1].enabled = true; S.markers[1].idx = 2; S.setActiveMkrId(2);
    displayOffset.set(80); click('btn-trg-from-mkr'); expect(trigLevel.get()).toBe(-35);
    click('btn-trg-from-peak'); expect(trigLevel.get()).toBe(-30);
    syncTriggerMode('rta'); status();
    S.setRtaData({ ver: 1, freq: new Float64Array([1, 2, 3]), spec: new Float32Array([-80, -40, -45]), wfRow: new Uint16Array(1), maxDensity: 1, startHz: 1, stopHz: 3 });
    click('btn-trg-from-mkr'); expect(trigLevel.get()).toBe(-45);
    click('btn-trg-from-peak'); expect(trigLevel.get()).toBe(-50);
  });
  it('resets hold and private phase on Free Run, Escape and preset reset', () => {
    click('btn-trg-capture'); expect(S.trigWaiting).toBe(true);
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })); expect(S.trigWaiting).toBe(false);
    click('btn-trg-capture'); resetTrigger(); expect(S.swpArmed).toBe(false); expect(S.swpHold).toBe(false);
    click('btn-trg-capture'); expect(S.swpArmed).toBe(true); click('btn-trg-free'); expect(S.swpArmed).toBe(false);
    expect(vi.mocked(send).mock.calls.some(([value]) => 'source' in value && value.source === 'bus')).toBe(false);
  });
});

describe('capture boundaries', () => {
  it('keeps the completed generation held and accepts a newer capture before STATUS', () => {
    syncTriggerMode('rta'); confirmTriggerMetadata({ source: 'bus', generation: 300, valid: true });
    beginHardwareCapture();
    const capture = { source: 'level', generation: 301, frames: 5, complete: true, valid: true };
    expect(prepareTriggerFrame(capture)).toBe('first'); expect(noteFrameArrived(capture)).toBe(true);
    confirmTriggerMetadata({ ...capture, frames: 9 }); expect(triggerHeld()).toBe(true);
    const next = { ...capture, generation: 302, frames: 10 };
    expect(prepareTriggerFrame(next)).toBe('first');
    expect(noteFrameArrived(next)).toBe(true);
  });
  it('ignores pre-Free packets and allows a new connection with a lower generation', () => {
    syncTriggerMode('rta'); confirmTriggerMetadata({ source: 'level', generation: 50, valid: true });
    releaseHardwareCapture();
    confirmTriggerMetadata({ source: 'level', generation: 50, valid: true }); expect(S.trigArmed).toBe(false);
    expect(prepareTriggerFrame({ source: 'level', generation: 50 })).toBe('ignore');
    confirmTriggerMetadata({ source: 'bus', generation: 51 }); expect(S.trigArmed).toBe(false);
    resetTriggerConnection(); confirmTriggerMetadata({ source: 'level', generation: 1, valid: true });
    expect(S.trigArmed).toBe(true);
  });
  it('does not treat a plain frame or elapsed time as a software hit', () => {
    armSwpTrigger(); vi.advanceTimersByTime(10000);
    expect(noteFrameArrived()).toBe(false); expect(S.trigHit).toBe(false); expect(S.trigWaiting).toBe(true);
  });
  it('drops crossing history when the same-size frequency grid changes', () => {
    trigLevel.confirm(-55); S.setFreqArray(new Float64Array([1, 2])); armSwpTrigger();
    S.traces[0].powers = new Float32Array([10, 10]);
    evaluateSwpTrigger(new Float32Array([-80, -80]));
    S.setFreqArray(new Float64Array([3, 4])); evaluateSwpTrigger(new Float32Array([-30, -30]));
    expect(S.swpHold).toBe(false);
    evaluateSwpTrigger(new Float32Array([-80, -80])); expect(S.swpHold).toBe(true);
  });
  it('captures native dBm crossings despite normalized or held display powers', () => {
    trigLevel.confirm(-55); S.setFreqArray(new Float64Array([1, 2])); armSwpTrigger();
    S.traces[0].powers = new Float32Array([0, 0]);
    evaluateSwpTrigger(new Float32Array([-40, -40]));
    evaluateSwpTrigger(new Float32Array([-80, -40]));
    expect(S.swpHold).toBe(true);
    expect(S.trigHit).toBe(true);
  });
  it('holds only after the complete newer hardware capture and ignores stale or partial packets', () => {
    syncTriggerMode('rta'); confirmTriggerMetadata({ source: 'bus', generation: 100, frames: 10 }); beginHardwareCapture();
    const capture = { source: 'level', generation: 101, frames: 11, complete: false, supported: true, valid: true };
    expect(prepareTriggerFrame({ ...capture, generation: 100 })).toBe('ignore');
    expect(prepareTriggerFrame(capture)).toBe('first'); expect(noteFrameArrived(capture)).toBe(false);
    expect(prepareTriggerFrame({ ...capture, frames: 12 })).toBe('continue');
    expect(noteFrameArrived({ ...capture, frames: 12, complete: false, trigger_missed: true })).toBe(false); expect(triggerMissed()).toBe(true);
    expect(noteFrameArrived({ ...capture, frames: 13, complete: true, trigger_missed: true, last_sdk_status: -303 })).toBe(true);
    expect(triggerHeld()).toBe(true); expect(triggerMissed()).toBe(true);
    expect(S.trigOverlay.some((line) => line.includes('MISSED') || line.includes('遗漏'))).toBe(true);
    expect(prepareTriggerFrame({ ...capture, frames: 14 })).toBe('ignore');
    resetTriggerRuntime(); expect(triggerHeld()).toBe(false);
  });
  it('rejects old complete packets after STATUS or an accepted partial packet advances generation', () => {
    syncTriggerMode('rta'); confirmTriggerMetadata({ source: 'bus', generation: 400, frames: 1 }); beginHardwareCapture();
    const packet = { source: 'level', generation: 401, frames: 2, complete: false, valid: true };
    expect(prepareTriggerFrame(packet)).toBe('first');
    const newer = { ...packet, generation: 403, frames: 3 };
    expect(prepareTriggerFrame(newer)).toBe('first');
    expect(prepareTriggerFrame({ ...packet, complete: true })).toBe('ignore');
    expect(noteFrameArrived({ ...packet, complete: true })).toBe(false);
    confirmTriggerMetadata({ ...newer, generation: 404 });
    expect(prepareTriggerFrame({ ...newer, complete: true })).toBe('ignore');
    expect(noteFrameArrived({ ...newer, complete: true })).toBe(false);
    expect(noteFrameArrived({ ...newer, generation: 404, frames: 4, complete: true })).toBe(true);
  });
  it('never fabricates captures from fake or unknown device metadata', () => {
    syncTriggerMode('rta'); confirmTriggerMetadata({ source: 'bus', generation: 200 }); beginHardwareCapture();
    expect(noteFrameArrived({ source: 'level', generation: 201, frames: 1, complete: null })).toBe(false);
    expect(noteFrameArrived({ source: 'level', generation: 201, frames: 2, complete: true, simulated: true })).toBe(false);
    expect(S.trigHit).toBe(false);
  });
});
