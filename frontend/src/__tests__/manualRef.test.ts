import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { setWS } from '../core/wsSend';
import { bindActions, syncGraphModeStatus } from '../ui/controls';
import { commitRefLevel, rejectManualRef, syncSdrRefUI } from '../ui/panels/refAmp';
import { DISPLAY_REF_TTL_MS, cancelDisplayRefRequest, getDisplayRef, noteDisplayRefReport, pendingDisplayRef, setDisplayRef } from '../ui/displayRef';
import { deviceMode, graphMode, resetGraphMode } from '../ui/graphMode';
import { confirmRefLevel, refLevel } from '../ui/refState';

const sent: Array<Record<string, unknown>> = [];
const input = () => document.getElementById('input-ref') as HTMLInputElement;

beforeEach(() => {
  vi.useFakeTimers();
  resetAll();
  resetGraphMode();
  deviceMode.confirm('sdr');
  graphMode.confirm('sdr');
  setDisplayRef('preset', -30);
  noteDisplayRefReport(-30);
  document.body.innerHTML = `<div id="level-panel"><input id="input-ref" type="number" value="-30">
    <button id="btn-ref-set" data-action="set-ref-level">Set</button>
    <button id="btn-ref-down" data-action="ref-down">Down</button>
    <button id="btn-ref-up" data-action="ref-up">Up</button>
    <button data-scale="5">5</button><input id="input-offset"><select id="select-level-unit"><option>dBm</option></select>
    </div><canvas id="spectrum"></canvas>`;
  sent.length = 0;
  // The test transport implements only the socket members consumed by wsSend.
  const socket = { readyState: WebSocket.OPEN, send: (value: string) => sent.push(JSON.parse(value)) } as unknown as WebSocket;
  setWS(socket);
  S.setDbPerDiv(10);
  bindActions();
});

afterEach(() => {
  setDisplayRef('preset', 0);
  setWS(null);
  resetGraphMode();
  document.body.replaceChildren();
  vi.useRealTimers();
});

describe('manual Ref controls', () => {
  it('shows real pending intent for Set, Enter, steps and the axis commit within the same bounds', () => {
    input().value = '-200';
    document.getElementById('btn-ref-set')!.click();
    expect(input().value).toBe('-160');
    expect(pendingDisplayRef()).toBe(-160);
    expect(input().classList.contains('ref-pending')).toBe(true);
    input().value = '90';
    input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    expect(getDisplayRef()).toBe(40);
    document.getElementById('btn-ref-down')!.click();
    expect(getDisplayRef()).toBe(30);
    commitRefLevel(-170);
    expect(getDisplayRef()).toBe(-160);
    expect(sent.filter(message => message.cmd === 'SET_REF').map(message => message.ref)).toEqual([-160, 40, 30, -160]);
  });

  it('keeps a newer edit through stale reports and restores the last accepted display on rejection', () => {
    input().value = '-40';
    document.getElementById('btn-ref-set')!.click();
    noteDisplayRefReport(-40);
    syncSdrRefUI();
    expect(pendingDisplayRef()).toBeNull();
    input().value = '-50';
    input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    noteDisplayRefReport(-30);
    syncSdrRefUI();
    expect(input().value).toBe('-50');
    expect(input().classList.contains('ref-pending')).toBe(true);
    rejectManualRef();
    expect(getDisplayRef()).toBe(-40);
    expect(input().value).toBe('-40');
    expect(input().classList.contains('ref-pending')).toBe(false);
  });

  it('rolls back timed-out manual edits and lets display-only fits supersede them without a hardware write', () => {
    setDisplayRef('auto', -75);
    input().value = '-50';
    document.getElementById('btn-ref-set')!.click();
    vi.advanceTimersByTime(DISPLAY_REF_TTL_MS);
    expect(getDisplayRef()).toBe(-75);
    expect(input().value).toBe('-75');
    expect(input().classList.contains('ref-pending')).toBe(false);
    input().value = '-45';
    document.getElementById('btn-ref-set')!.click();
    const writes = sent.filter(message => message.cmd === 'SET_REF').length;
    setDisplayRef('auto', -90);
    vi.advanceTimersByTime(DISPLAY_REF_TTL_MS);
    expect(getDisplayRef()).toBe(-90);
    expect(pendingDisplayRef()).toBeNull();
    expect(sent.filter(message => message.cmd === 'SET_REF')).toHaveLength(writes);
  });

  it('keeps the current display after Auto takes over an unconfirmed manual hardware request', () => {
    input().value = '-50';
    document.getElementById('btn-ref-set')!.click();
    cancelDisplayRefRequest();
    vi.advanceTimersByTime(DISPLAY_REF_TTL_MS);
    expect(getDisplayRef()).toBe(-50);
    expect(pendingDisplayRef()).toBeNull();
    setDisplayRef('auto', -80);
    expect(getDisplayRef()).toBe(-80);
  });

  it('disables actual Level form controls during exclusive measurements and restores local controls afterward', () => {
    syncGraphModeStatus('harmonic');
    for (const control of document.querySelectorAll<HTMLInputElement>('#level-panel input, #level-panel select, #level-panel button')) {
      expect(control.disabled).toBe(true);
    }
    const writes = sent.filter(message => message.cmd === 'SET_REF').length;
    expect(commitRefLevel(-10)).toBeNull();
    expect(sent.filter(message => message.cmd === 'SET_REF')).toHaveLength(writes);
    syncGraphModeStatus('std');
    expect(input().disabled).toBe(false);
    expect((document.getElementById('input-offset') as HTMLInputElement).disabled).toBe(false);
    expect((document.getElementById('select-level-unit') as HTMLSelectElement).disabled).toBe(false);
  });

  it('does not carry pending swept Ref across a confirmed mode handoff', () => {
    confirmRefLevel('std', -30);
    refLevel.set(-10);
    confirmRefLevel('rta', -60);
    expect(refLevel.get()).toBe(-60);
    expect(refLevel.pending()).toBe(false);
  });

  it('accepts a clamped Ref only for the newest acknowledged request', () => {
    confirmRefLevel('std', -30);
    refLevel.set(20);
    refLevel.set(30);
    confirmRefLevel('std', 17, 20);
    expect(refLevel.get()).toBe(30);
    expect(refLevel.pending()).toBe(true);
    confirmRefLevel('std', 27, 30);
    expect(refLevel.get()).toBe(27);
    expect(refLevel.pending()).toBe(false);
  });
});
