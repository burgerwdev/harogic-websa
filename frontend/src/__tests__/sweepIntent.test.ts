import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import * as S from '../core/store';
import { setWS } from '../core/wsSend';
import { bindSweepEditor, rejectSweepRequest, resetSweepEditor, setSweepSpeed, syncSweepStatus } from '../ui/panels/waterfall';

const sent: Array<{ request_id: string }> = [];
const select = () => document.getElementById('select-sweep-mode') as HTMLSelectElement;
const input = () => document.getElementById('input-sweep-time') as HTMLInputElement;
function report(mode: number, time: number, extra = {}) {
  syncSweepStatus({ mode: 'std', sweep_time_mode: mode, sweep_time: time, ...extra });
}
function commit(mode: number, time: number) {
  select().value = String(mode);
  input().value = String(time);
  setSweepSpeed();
  return sent[sent.length - 1].request_id;
}
beforeEach(() => {
  document.body.innerHTML = `<select id="select-sweep-mode">${Array.from({ length: 9 }, (_, n) => `<option value="${n}">${n}</option>`).join('')}</select><input id="input-sweep-time"><button data-action="set-sweep"></button>`;
  S.setSdrMode(false);
  S.setRtaMode(false);
  resetSweepEditor();
  sent.length = 0;
  setWS({ readyState: WebSocket.OPEN, send: (raw: string) => sent.push(JSON.parse(raw)) } as unknown as WebSocket);
  bindSweepEditor();
  report(0, 0);
});
afterEach(() => { setWS(null); resetSweepEditor(); document.body.replaceChildren(); S.setSdrMode(false); });

describe('grouped sweep editor', () => {
  it('fills mode, time and visibility together from reload and mode profile reports', () => {
    report(7, 0.125);
    expect([select().value, input().value, input().style.display]).toEqual(['7', '0.125', '']);
    syncSweepStatus({ mode: 'rta', req: { rta: { sweep_time_mode: 2, sweep_time: 0 } } });
    expect([select().value, input().value, input().style.display]).toEqual(['2', '0', 'none']);
  });
  it('preserves both fields while focused and keeps an edited draft after blur', () => {
    report(7, 0.1);
    input().focus();
    report(2, 0);
    expect([select().value, input().value]).toEqual(['7', '0.1']);
    input().value = '0.3';
    input().dispatchEvent(new Event('input'));
    input().blur();
    report(6, 5);
    expect([select().value, input().value, input().style.display]).toEqual(['7', '0.3', '']);
  });
  it('ignores old ACKs, matching-value uncorrelated reports and old errors until latest refusal', () => {
    report(7, 0.1);
    const first = commit(6, 2);
    const latest = commit(8, 3);
    report(8, 3);
    report(6, 2, { response_to: 'SET_SWEEP', request_id: first });
    rejectSweepRequest(first);
    expect([select().value, input().value]).toEqual(['8', '3']);
    rejectSweepRequest(latest);
    expect([select().value, input().value]).toEqual(['6', '2']);
  });
  it('accepts corrected latest readback and ignores its delayed predecessor after completion', () => {
    const first = commit(6, 2);
    const latest = commit(7, 0.2);
    report(7, 0.25, { response_to: 'SET_SWEEP', request_id: latest });
    report(6, 2, { response_to: 'SET_SWEEP', request_id: first });
    expect([select().value, input().value]).toEqual(['7', '0.25']);
  });
  it('preserves a subsequent draft on refusal and clears it on preset reset', () => {
    const latest = commit(7, 0.2);
    input().value = '0.4';
    input().dispatchEvent(new Event('input'));
    rejectSweepRequest(latest);
    expect(input().value).toBe('0.4');
    resetSweepEditor();
    report(2, 0);
    expect([select().value, input().value, input().style.display]).toEqual(['2', '0', 'none']);
  });
  it('accepts external SET_SWEEP readback with no ID or another client ID when idle', () => {
    report(6, 4, { response_to: 'SET_SWEEP' });
    expect([select().value, input().value]).toEqual(['6', '4']);
    report(7, 0.5, { response_to: 'SET_SWEEP', request_id: 'other-client-1' });
    expect([select().value, input().value]).toEqual(['7', '0.5']);
  });
  it('cancels pending on a mode transition before ignoring an old own ACK', () => {
    const old = commit(7, 0.2);
    syncSweepStatus({ mode: 'rta', response_to: 'SET_SWEEP', request_id: old,
      sweep_time_mode: 2, sweep_time: 0 });
    syncSweepStatus({ mode: 'rta', sweep_time_mode: 6, sweep_time: 8 });
    expect([select().value, input().value]).toEqual(['6', '8']);
    rejectSweepRequest(old);
    expect([select().value, input().value]).toEqual(['6', '8']);
  });
  it('rolls a refusal back onto the superseded accepted pair, not the pre-set state', () => {
    report(2, 0);
    const a = commit(6, 2);
    const b = commit(7, 0.2);
    report(6, 2, { response_to: 'SET_SWEEP', request_id: a });
    expect([select().value, input().value]).toEqual(['7', '0.2']);
    rejectSweepRequest(b);
    expect([select().value, input().value, input().style.display]).toEqual(['6', '2', '']);
    report(6, 2, { response_to: 'SET_SWEEP', request_id: a });
    rejectSweepRequest(b);
    expect([select().value, input().value]).toEqual(['6', '2']);
  });
  it('stops superseded baseline updates after preset retires the sequence', () => {
    const a = commit(6, 2);
    resetSweepEditor();
    report(2, 0);
    report(7, 0.2, { response_to: 'SET_SWEEP', request_id: a });
    expect([select().value, input().value]).toEqual(['2', '0']);
  });
  it('does not send sweep configuration in SDR mode', () => {
    S.setSdrMode(true);
    setSweepSpeed();
    expect(sent).toEqual([]);
  });
});
