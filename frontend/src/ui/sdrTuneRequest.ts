import { send } from '../core/wsSend';
import * as S from '../core/store';
import { t } from '../core/i18n';
import { requestRender } from '../render/redraw';
import { renderSdrState, sdrListenHz } from './sdrState';

let serial = 0;
let inFlight: { id: string; hz: number; timer: number } | null = null;
let queued: number | null = null;
let version = -1;

/** Cancel pending work. Replies from the old request cannot start another command. */
export function cancelSdrTuneRequest(): void {
  if (inFlight) window.clearTimeout(inFlight.timer);
  inFlight = null;
  queued = null;
  sdrListenHz.reset();
}

function submit(hz: number): void {
  const id = `sdr-tune-${++serial}`;
  sdrListenHz.set(hz);
  sdrListenHz.touch(hz);
  inFlight = { id, hz, timer: window.setTimeout(() => {
    if (inFlight?.id !== id) return;
    cancelSdrTuneRequest();
    S.setStatusError(t('sdr_tune_timeout'));
    renderSdrState();
    requestRender();
  }, 3000) };
  send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: hz, request_id: id });
}

/** One request in flight; repeated input replaces the single unsent target. */
export function requestSdrTune(hz: number): void {
  if (!Number.isFinite(hz) || hz <= 0) return;
  if (inFlight) {
    queued = hz;
    sdrListenHz.set(hz);
    sdrListenHz.touch(hz);
  } else if (Math.abs(hz - (sdrListenHz.confirmedValue() ?? 0)) >= 0.5) submit(hz);
  renderSdrState();
  requestRender();
}

/** Drop unsent repeat input on blur, while allowing the current request to settle. */
export function stopSdrTuneRepeat(): void {
  queued = null;
  if (inFlight) {
    sdrListenHz.set(inFlight.hz);
    sdrListenHz.touch(inFlight.hz);
  }
}

/** Returns false for old request identities and older configuration snapshots. */
export function confirmSdrTuneReport(hz: number, responseTo: unknown, requestId: unknown,
  configVersion: unknown): boolean {
  const own = typeof requestId === 'string' && requestId.startsWith('sdr-tune-');
  const nextVersion = typeof configVersion === 'number' ? configVersion : -1;
  if ((own && requestId !== inFlight?.id) || (nextVersion >= 0 && nextVersion < version)) return false;
  if (nextVersion >= 0) version = nextVersion;
  if (!Number.isFinite(hz) || hz <= 0) return true;
  sdrListenHz.confirm(hz);
  if (inFlight && responseTo === 'SET_SDR_CHANNEL' && requestId === inFlight.id) {
    window.clearTimeout(inFlight.timer);
    inFlight = null;
    const next = queued;
    queued = null;
    sdrListenHz.reset();
    if (next !== null && Math.abs(next - hz) >= 0.5) submit(next);
  } else if (inFlight) {
    const target = queued ?? inFlight.hz;
    sdrListenHz.set(target);
    sdrListenHz.touch(target);
  }
  return true;
}

export function rejectSdrTuneRequest(responseTo?: string, requestId?: string | number): void {
  if (responseTo !== 'SET_SDR_CHANNEL' || requestId !== inFlight?.id) return;
  cancelSdrTuneRequest();
  renderSdrState();
  requestRender();
}

export function resetSdrTuneSession(): void {
  cancelSdrTuneRequest();
  version = -1;
}
