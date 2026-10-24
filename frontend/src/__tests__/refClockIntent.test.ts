import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { prepareSdrAudioTransition } from '../audio/sdrAudio';
import { setWS } from '../core/wsSend';
import { deviceMode, graphMode, resetGraphMode } from '../ui/graphMode';
import {
  rejectRefClockRequest, resetRefClockIntent, setRefClock,
  syncRefClockStatus, toggleRefClkOut,
} from '../ui/panels/refAmp';

vi.mock('../audio/sdrAudio', () => ({ prepareSdrAudioTransition: vi.fn() }));

const sent: Array<Record<string, unknown>> = [];
const source = () => document.getElementById('select-refclk') as HTMLSelectElement;
const output = () => document.getElementById('btn-refclk-out') as HTMLButtonElement;
const requestId = (index: number) => sent[index].request_id as string;
const sourcePending = () => source().getAttribute('aria-busy') === 'true';
const outputPending = () => output().getAttribute('aria-busy') === 'true';

beforeEach(() => {
  document.body.innerHTML = `<select id="select-refclk">
    <option value="internal">Internal</option><option value="external">External</option>
    <option value="external_forced">Forced</option><option value="premium">Premium</option>
    </select><span id="refclk-label"></span><button id="btn-refclk-out"></button>`;
  resetGraphMode();
  deviceMode.confirm('std');
  graphMode.confirm('std');
  resetRefClockIntent();
  syncRefClockStatus({ ref_clock: 'internal', refclk_out: false });
  sent.length = 0;
  vi.mocked(prepareSdrAudioTransition).mockClear();
  setWS({
    readyState: WebSocket.OPEN,
    send: (message: string) => sent.push(JSON.parse(message)),
  } as unknown as WebSocket);
});

afterEach(() => {
  setWS(null);
  resetRefClockIntent();
  resetGraphMode();
  document.body.replaceChildren();
});

describe('correlated reference clock intent', () => {
  it('keeps the newest source pending through an equal older ACK and unrelated readback', () => {
    setRefClock('external');
    setRefClock('external_forced');
    setRefClock('external');
    expect(source().value).toBe('external');
    expect(sourcePending()).toBe(true);
    expect(new Set(sent.map(message => message.request_id)).size).toBe(3);

    syncRefClockStatus({ ref_clock: 'external', response_to: 'SET_REFCK', request_id: requestId(0) });
    expect(sourcePending()).toBe(true);
    syncRefClockStatus({ ref_clock: 'internal' });
    rejectRefClockRequest('SET_REFCK', requestId(1));
    expect(source().value).toBe('external');
    expect(sourcePending()).toBe(true);

    syncRefClockStatus({ ref_clock: 'external', response_to: 'SET_REFCK', request_id: requestId(2) });
    expect(sourcePending()).toBe(false);
    syncRefClockStatus({ ref_clock: 'external_forced', response_to: 'SET_REFCK', request_id: requestId(1) });
    expect(source().value).toBe('external');
  });

  it('accumulates rapid output clicks from intent even when the DOM is stale', () => {
    toggleRefClkOut();
    output().classList.remove('on');
    toggleRefClkOut();
    toggleRefClkOut();
    expect(sent.map(message => message.on)).toEqual([true, false, true]);
    expect(output().classList.contains('on')).toBe(true);
    expect(document.getElementById('refclk-label')!.classList.contains('refclk-label-on')).toBe(true);

    syncRefClockStatus({ refclk_out: true, response_to: 'SET_REFCKOUT', request_id: requestId(0) });
    expect(outputPending()).toBe(true);
    syncRefClockStatus({ refclk_out: false });
    rejectRefClockRequest('SET_REFCKOUT', requestId(1));
    expect(output().classList.contains('on')).toBe(true);
    expect(outputPending()).toBe(true);
    rejectRefClockRequest('SET_REFCKOUT', requestId(2));
    expect(output().classList.contains('on')).toBe(false);
    expect(outputPending()).toBe(false);
    expect(document.getElementById('refclk-label')!.classList.contains('refclk-label-on')).toBe(false);
  });

  it('settles source and output independently and rolls back only a matching ERROR', () => {
    setRefClock('premium');
    toggleRefClkOut();
    syncRefClockStatus({
      ref_clock: 'premium', refclk_out: true,
      response_to: 'SET_REFCK', request_id: requestId(0),
    });
    expect(sourcePending()).toBe(false);
    expect(outputPending()).toBe(true);
    rejectRefClockRequest('SET_REFCKOUT', requestId(0));
    rejectRefClockRequest('SET_REFCKOUT');
    rejectRefClockRequest('SET_REFCK', requestId(1));
    expect(outputPending()).toBe(true);
    syncRefClockStatus({ refclk_out: false });
    rejectRefClockRequest('SET_REFCKOUT', requestId(1));
    expect(output().classList.contains('on')).toBe(false);
    expect(outputPending()).toBe(false);
    expect(source().value).toBe('premium');

    setRefClock('external');
    syncRefClockStatus({ ref_clock: 'premium' });
    rejectRefClockRequest('SET_REFCK', requestId(2));
    expect(source().value).toBe('premium');
    expect(sourcePending()).toBe(false);
  });

  it('accepts authoritative readback from the matching ACK even when it differs from intent', () => {
    setRefClock('external');
    syncRefClockStatus({ ref_clock: 'internal', response_to: 'SET_REFCK', request_id: requestId(0) });
    expect(source().value).toBe('internal');
    expect(sourcePending()).toBe(false);
    toggleRefClkOut();
    syncRefClockStatus({ refclk_out: false, response_to: 'SET_REFCKOUT', request_id: requestId(1) });
    expect(output().classList.contains('on')).toBe(false);
    expect(outputPending()).toBe(false);
  });

  it('keeps intent through missing or foreign correlation even when the reported value agrees', () => {
    setRefClock('external');
    toggleRefClkOut();
    syncRefClockStatus({ ref_clock: 'external', response_to: 'SET_REFCK' });
    syncRefClockStatus({ refclk_out: true, response_to: 'SET_REFCKOUT', request_id: 1 });
    rejectRefClockRequest('SET_REFCK', 1);
    expect(sourcePending()).toBe(true);
    expect(outputPending()).toBe(true);
    syncRefClockStatus({ ref_clock: 'internal', refclk_out: false });
    expect(source().value).toBe('external');
    expect(output().classList.contains('on')).toBe(true);
  });

  it('drops pending intents on reset and ignores their delayed ACKs and errors', () => {
    setRefClock('external');
    toggleRefClkOut();
    resetRefClockIntent();
    syncRefClockStatus({ ref_clock: 'internal', refclk_out: false });
    syncRefClockStatus({
      ref_clock: 'external', refclk_out: true,
      response_to: 'SET_REFCK', request_id: requestId(0),
    });
    syncRefClockStatus({ refclk_out: true, response_to: 'SET_REFCKOUT', request_id: requestId(1) });
    expect(source().value).toBe('internal');
    expect(output().classList.contains('on')).toBe(false);
    expect(sourcePending()).toBe(false);
    expect(outputPending()).toBe(false);
    setRefClock('premium');
    toggleRefClkOut();
    rejectRefClockRequest('SET_REFCK', requestId(0));
    rejectRefClockRequest('SET_REFCKOUT', requestId(1));
    expect(source().value).toBe('premium');
    expect(output().classList.contains('on')).toBe(true);
    expect(sourcePending()).toBe(true);
    expect(outputPending()).toBe(true);
  });

  it('guards measurement ownership and mutes clock changes only in SDR', () => {
    for (const mode of ['harmonic', 'pnm']) {
      deviceMode.confirm(mode);
      setRefClock('external');
      toggleRefClkOut();
    }
    expect(sent).toEqual([]);
    expect(source().value).toBe('internal');
    expect(outputPending()).toBe(false);
    deviceMode.confirm('rta');
    graphMode.confirm('rta');
    setRefClock('external');
    toggleRefClkOut();
    expect(prepareSdrAudioTransition).not.toHaveBeenCalled();
    deviceMode.confirm('sdr');
    graphMode.confirm('sdr');
    setRefClock('internal');
    toggleRefClkOut();
    expect(sent.map(message => message.cmd)).toEqual(['SET_REFCK', 'SET_REFCKOUT', 'SET_REFCK', 'SET_REFCKOUT']);
    expect(prepareSdrAudioTransition).toHaveBeenCalledTimes(2);
  });
});
