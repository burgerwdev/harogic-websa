import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { setWS } from '../core/wsSend';
import { bindActions, syncGraphModeStatus } from '../ui/controls';
import { commitLevelOffset, commitRefLevel, rejectManualRef, syncRefUI } from '../ui/panels/refAmp';
import { DISPLAY_REF_TTL_MS, cancelDisplayRefRequest, getDisplayRef, noteDisplayRefReport, pendingDisplayRef, setDisplayRef } from '../ui/displayRef';
import { deviceMode, graphMode, resetGraphMode } from '../ui/graphMode';
import { confirmRefLevel, effectiveRefLevel, refLevel } from '../ui/refState';
import { displayOffset, displayUnit } from '../ui/displayState';

const sent: Array<Record<string, unknown>> = [];
const input = () => document.getElementById('input-ref') as HTMLInputElement;

beforeEach(() => {
  vi.useFakeTimers();
  resetAll();
  resetGraphMode();
  displayOffset.set(0);
  displayUnit.set('dBm');
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
    expect(input().value).toBe('-200');
    expect(pendingDisplayRef()).toBe(-200);
    expect(input().classList.contains('ref-pending')).toBe(true);
    input().value = '90';
    input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    expect(getDisplayRef()).toBe(90);
    document.getElementById('btn-ref-down')!.click();
    expect(getDisplayRef()).toBe(80);
    commitRefLevel(-170);
    expect(getDisplayRef()).toBe(-170);
    expect(sent.filter(message => message.cmd === 'SET_REF').map(message => message.ref)).toEqual([-160, 40, -160]);
  });

  it.each(['std', 'rta', 'sdr'] as const)('uses compensated editor and step bounds in %s', (mode) => {
    graphMode.confirm(mode);
    deviceMode.confirm(mode);
    commitLevelOffset(60);
    input().value = '170';
    document.getElementById('btn-ref-set')!.click();
    expect(input().value).toBe('160');
    expect(getDisplayRef()).toBe(160);
    expect(sent.at(-1)).toEqual({ cmd: 'SET_REF', mode: 'manual', ref: 40 });
    document.getElementById('btn-ref-up')!.click();
    document.getElementById('btn-ref-down')!.click();
    expect(input().value).toBe('150');
    expect(sent.filter(message => message.cmd === 'SET_REF')).toHaveLength(1);

    commitLevelOffset(-60);
    expect(getDisplayRef()).toBe(30);
    input().value = '-270';
    input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    expect(input().value).toBe('-260');
    expect(getDisplayRef()).toBe(-260);
    expect(sent.at(-1)).toEqual({ cmd: 'SET_REF', mode: 'manual', ref: -160 });
    document.getElementById('btn-ref-down')!.click();
    document.getElementById('btn-ref-up')!.click();
    expect(input().value).toBe('-250');
    expect(sent.filter(message => message.cmd === 'SET_REF')).toHaveLength(2);
    expect(pendingDisplayRef()).toBe(-250);
  });

  it.each(['std', 'rta', 'sdr'] as const)('retains a +100 viewport through raw ACKs and reversals in %s', mode => {
    graphMode.confirm(mode); deviceMode.confirm(mode);
    commitRefLevel(90);
    vi.advanceTimersByTime(1000);
    commitRefLevel(100);
    expect(sent.filter(message => message.cmd === 'SET_REF')).toEqual([
      { cmd: 'SET_REF', mode: 'manual', ref: 40 },
    ]);
    noteDisplayRefReport(-30);
    setDisplayRef('mode', -30);
    expect(pendingDisplayRef()).toBe(100);
    noteDisplayRefReport(40);
    setDisplayRef('mode', 40);
    expect(getDisplayRef()).toBe(100);
    expect(pendingDisplayRef()).toBeNull();
    commitRefLevel(110);
    expect(getDisplayRef()).toBe(100);
    document.getElementById('btn-ref-down')!.click();
    expect(getDisplayRef()).toBe(90);
    expect(sent.filter(message => message.cmd === 'SET_REF')).toHaveLength(1);
    commitRefLevel(30);
    noteDisplayRefReport(40);
    expect(pendingDisplayRef()).toBe(30);
    rejectManualRef();
    expect(getDisplayRef()).toBe(90);
    setDisplayRef('mode', 40);
    expect(getDisplayRef()).toBe(90);
  });

  it.each(['std', 'rta', 'sdr'] as const)('keeps the original deadline and rollback when a clamped request is reused in %s', mode => {
    graphMode.confirm(mode); deviceMode.confirm(mode);
    commitRefLevel(90);
    vi.advanceTimersByTime(DISPLAY_REF_TTL_MS - 1);
    commitRefLevel(100);
    commitLevelOffset(60);
    expect(pendingDisplayRef()).toBe(160);
    vi.advanceTimersByTime(1);
    expect(getDisplayRef()).toBe(30);
    expect(pendingDisplayRef()).toBeNull();
    expect(input().value).toBe('30');
  });

  it('shifts the offset editor display without replacing a pending hardware request', () => {
    commitRefLevel(-40);
    const offset = document.getElementById('input-offset') as HTMLInputElement;
    offset.value = '60';
    offset.dispatchEvent(new Event('change'));
    expect(getDisplayRef()).toBe(20);
    expect(input().value).toBe('20');
    expect(pendingDisplayRef()).toBe(20);
    expect(input().classList.contains('ref-pending')).toBe(true);
    expect(sent.filter(message => message.cmd === 'SET_REF')).toEqual([
      { cmd: 'SET_REF', mode: 'manual', ref: -40 },
    ]);
    noteDisplayRefReport(-40);
    syncRefUI();
    expect(pendingDisplayRef()).toBeNull();
    expect(input().value).toBe('20');
    expect(input().classList.contains('ref-pending')).toBe(false);
  });

  it('keeps a newer edit through stale reports and restores the last accepted display on rejection', () => {
    input().value = '-40';
    document.getElementById('btn-ref-set')!.click();
    noteDisplayRefReport(-40);
    syncRefUI();
    expect(pendingDisplayRef()).toBeNull();
    input().value = '-50';
    input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    noteDisplayRefReport(-30);
    syncRefUI();
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

  it('keeps manual Ref below the effective hardware floor and continues stepping down', () => {
    graphMode.confirm('rta'); deviceMode.confirm('rta');
    confirmRefLevel('rta', -60, undefined, -100);
    refLevel.set(-120);
    confirmRefLevel('rta', -60, -120, -120);
    setDisplayRef('mode', refLevel.get());
    expect(effectiveRefLevel.get()).toBe(-60);
    expect(getDisplayRef()).toBe(-120);
    document.getElementById('btn-ref-down')!.click();
    expect(sent.at(-1)).toEqual({ cmd: 'SET_REF', mode: 'manual', ref: -130 });
    confirmRefLevel('rta', -60, -120, -120);
    expect(refLevel.get()).toBe(-130);
    confirmRefLevel('rta', -60, -130, -130);
    expect(refLevel.get()).toBe(-130);
    expect(refLevel.pending()).toBe(false);
  });
});
