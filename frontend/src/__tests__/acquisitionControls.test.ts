import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../core/wsSend', () => ({ send: vi.fn(), onCommandSent: vi.fn() }));

import * as S from '../core/store';
import { send } from '../core/wsSend';
import { presetAll, rejectPresetRequest, syncGraphModeStatus, syncPresetStatus } from '../ui/controls';
import { centerHz, spanHz } from '../ui/freqState';
import { resetGraphMode } from '../ui/graphMode';
import { beginFrequencyCommit, resetFrequencyEditors } from '../ui/panels/frequency';

beforeEach(() => {
  vi.useFakeTimers();
  resetGraphMode();
  S.setRtaMode(false);
  S.setSdrMode(false);
  resetFrequencyEditors();
  centerHz.confirm(1e9);
  spanHz.confirm(100e6);
  vi.mocked(send).mockClear();
});

afterEach(() => {
  vi.advanceTimersByTime(3000);
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
