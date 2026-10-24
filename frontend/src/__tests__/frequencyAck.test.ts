import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../core/wsSend', () => ({ send: vi.fn() }));

import * as S from '../core/store';
import { send } from '../core/wsSend';
import { centerHz, spanHz } from '../ui/freqState';
import { updateFreqUIInputs } from '../ui/freqInputs';
import { spanStepHz } from '../ui/swpState';
import { graphMode, resetGraphMode } from '../ui/graphMode';
import {
  beginFrequencyCommit, confirmFrequencyStatus, markFrequencyDirty,
  rejectFrequencyCommit, resetFrequencyEditors, stepSwpSpan, syncFrequencyEditorStatus,
} from '../ui/panels/frequency';

function editor(id: 'swp-freq-settings' | 'rta-freq-settings' = 'swp-freq-settings') {
  const el = document.createElement('div');
  el.id = id;
  const input = document.createElement('input');
  input.id = id === 'swp-freq-settings' ? 'input-center' : 'input-rta-center';
  el.append(input);
  document.body.append(el);
  return { el, input };
}

beforeEach(() => {
  vi.useFakeTimers();
  resetGraphMode();
  graphMode.confirm('std');
  S.setRtaMode(false);
  S.setSdrMode(false);
  resetFrequencyEditors();
  centerHz.confirm(1e9);
  spanHz.confirm(100e6);
  vi.mocked(send).mockClear();
});

afterEach(() => {
  resetFrequencyEditors();
  resetGraphMode();
  spanStepHz.reset();
  document.getElementById('swp-freq-settings')?.remove();
  document.getElementById('rta-freq-settings')?.remove();
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe('frequency request ownership', () => {
  it.each(['ack', 'error'] as const)('ignores an old %s while the newer submission is pending', kind => {
    const { el } = editor();
    markFrequencyDirty('swp-freq-settings');
    const a = beginFrequencyCommit('swp-freq-settings', { center: 1.1e9, span: 90e6 });
    markFrequencyDirty('swp-freq-settings');
    const b = beginFrequencyCommit('swp-freq-settings', { center: 1.2e9, span: 80e6 });
    if (kind === 'ack') {
      confirmFrequencyStatus({ response_to: 'SET_FREQ', request_id: a, mode: 'std', center: 1.1e9, span: 90e6, req: {} });
    } else rejectFrequencyCommit('SET_FREQ', a);
    expect(el.dataset.requestId).toBe(b);
    expect(el.dataset.pending).toBe('1');
    expect(centerHz.get()).toBe(1.2e9);
    expect(spanHz.get()).toBe(80e6);
    syncFrequencyEditorStatus('SET_FREQ', b);
    expect(el.dataset.pending).toBeUndefined();
  });

  it('rolls back a rejected newer request to the accepted predecessor window', () => {
    const { el } = editor();
    const a = beginFrequencyCommit('swp-freq-settings', { center: 1.1e9, span: 90e6 });
    const b = beginFrequencyCommit('swp-freq-settings', { center: 1.2e9, span: 80e6 });
    confirmFrequencyStatus({ response_to: 'SET_FREQ', request_id: a, mode: 'std', center: 1.09e9, span: 89e6, req: {} });
    expect(centerHz.get()).toBe(1.2e9);
    expect(spanHz.get()).toBe(80e6);
    expect(el.dataset.requestId).toBe(b);
    rejectFrequencyCommit('SET_FREQ', b);
    expect(centerHz.get()).toBe(1.09e9);
    expect(spanHz.get()).toBe(89e6);
    expect(el.dataset.pending).toBeUndefined();
    confirmFrequencyStatus({ response_to: 'SET_FREQ', request_id: a, mode: 'std', center: 1.1e9, span: 90e6, req: {} });
    expect(centerHz.get()).toBe(1.09e9);
    expect(spanHz.get()).toBe(89e6);
  });

  it('preserves a newer unsubmitted draft through acknowledgement and input refresh', () => {
    const { el, input } = editor();
    markFrequencyDirty('swp-freq-settings');
    const requestId = beginFrequencyCommit('swp-freq-settings');
    markFrequencyDirty('swp-freq-settings');
    input.dataset.edited = '1';
    input.value = '1234.5678';
    syncFrequencyEditorStatus('SET_FREQ', requestId);
    updateFreqUIInputs(true);
    expect(el.dataset.dirty).toBe('1');
    expect(input.dataset.edited).toBe('1');
    expect(input.value).toBe('1234.5678');
    expect(el.dataset.pending).toBeUndefined();
  });

  it('requires a matching request id before clearing submitted edits', () => {
    const { el, input } = editor();
    markFrequencyDirty('swp-freq-settings');
    input.dataset.edited = '1';
    const requestId = beginFrequencyCommit('swp-freq-settings');
    syncFrequencyEditorStatus('SET_FREQ');
    syncFrequencyEditorStatus('SET_FREQ', 42);
    syncFrequencyEditorStatus('SET_RTA', requestId);
    expect(el.dataset.pending).toBe('1');
    syncFrequencyEditorStatus('SET_FREQ', requestId);
    expect(el.dataset.dirty).toBeUndefined();
    expect(input.dataset.edited).toBeUndefined();
    expect(el.dataset.pending).toBeUndefined();
  });

  it('expires each editor independently after 3000 ms without a status refresh', () => {
    const swp = editor();
    const rta = editor('rta-freq-settings');
    beginFrequencyCommit('swp-freq-settings', { center: 1.1e9, span: 90e6 });
    vi.advanceTimersByTime(1000);
    beginFrequencyCommit('rta-freq-settings');
    vi.advanceTimersByTime(1999);
    expect(swp.el.dataset.pending).toBe('1');
    vi.advanceTimersByTime(1);
    expect(swp.el.dataset.pending).toBeUndefined();
    expect(centerHz.get()).toBe(1e9);
    expect(spanHz.get()).toBe(100e6);
    expect(rta.el.dataset.pending).toBe('1');
    vi.advanceTimersByTime(1000);
    expect(rta.el.dataset.pending).toBeUndefined();
  });

  it('gives a superseding submission its own full timeout', () => {
    const { el } = editor();
    beginFrequencyCommit('swp-freq-settings');
    vi.advanceTimersByTime(2000);
    const b = beginFrequencyCommit('swp-freq-settings');
    vi.advanceTimersByTime(2999);
    expect(el.dataset.requestId).toBe(b);
    vi.advanceTimersByTime(1);
    expect(el.dataset.pending).toBeUndefined();
  });

  it('accumulates rapid span steps from the newest desired center and span', () => {
    const { el } = editor();
    spanStepHz.set(10e6);
    beginFrequencyCommit('swp-freq-settings', { center: 1.2e9, span: 100e6 });
    stepSwpSpan(1);
    stepSwpSpan(1);
    expect(vi.mocked(send).mock.calls.map(([command]) => command)).toEqual([
      { cmd: 'SET_FREQ', center: 1.2e9, span: 110e6, request_id: expect.any(String) },
      { cmd: 'SET_FREQ', center: 1.2e9, span: 120e6, request_id: expect.any(String) },
    ]);
    const [a, b] = vi.mocked(send).mock.calls.map(([command]) => command as { request_id: string });
    expect(a.request_id).not.toBe(b.request_id);
    expect(el.dataset.requestId).toBe(b.request_id);
    expect(spanHz.get()).toBe(120e6);
  });

  it('uses the actual device-clamped window from the matching acknowledgement', () => {
    const { el } = editor();
    const requestId = beginFrequencyCommit('swp-freq-settings', { center: 1.2e9, span: 120e6 });
    confirmFrequencyStatus({ response_to: 'SET_FREQ', request_id: requestId, mode: 'std', center: 1.19e9, span: 119e6, req: {} });
    expect(centerHz.get()).toBe(1.19e9);
    expect(spanHz.get()).toBe(119e6);
    expect(el.dataset.pending).toBeUndefined();
  });

  it('does not restore an old window when a reply arrives after reset', () => {
    const { el } = editor();
    const requestId = beginFrequencyCommit('swp-freq-settings', { center: 1.2e9, span: 120e6 });
    resetFrequencyEditors();
    confirmFrequencyStatus({ response_to: 'SET_FREQ', request_id: requestId, mode: 'std', center: 1.2e9, span: 120e6, req: {} });
    expect(centerHz.get()).toBe(1e9);
    expect(spanHz.get()).toBe(100e6);
    expect(el.dataset.pending).toBeUndefined();
  });
});
