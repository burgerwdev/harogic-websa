import { send } from '../core/wsSend';
import { currentSdrChannel } from './sdrChannels';
import * as S from '../core/store';
import { t } from '../core/i18n';
import { requestRender } from '../render/redraw';
import { renderSdrState, sdrListenHz, sdrPrimaryEnabled } from './sdrState';
import { cancelReceiverFocus } from './spectrumViewport';

let serial = 0;
let inFlight: { id: string; hz: number; timer: number; followViewport: boolean } | null = null;
let queued: { hz: number; followViewport: boolean } | null = null;
let version = -1;
let timeoutError: { id: string; text: string; version: number } | null = null;

function clearTimeoutError(): void {
  if (timeoutError && S.statusError === timeoutError.text) S.setStatusError(null);
  timeoutError = null;
}

/** Cancel pending work. Replies from the old request cannot start another command. */
export function cancelSdrTuneRequest(): void {
  if (inFlight) window.clearTimeout(inFlight.timer);
  inFlight = null;
  queued = null;
  clearTimeoutError();
  sdrListenHz.reset();
}

function submit(hz: number, followViewport: boolean): void {
  cancelReceiverFocus();
  const id = `sdr-tune-${++serial}`;
  sdrListenHz.set(hz);
  sdrListenHz.touch(hz);
  inFlight = { id, hz, followViewport, timer: window.setTimeout(() => {
    if (inFlight?.id !== id) return;
    cancelSdrTuneRequest();
    const text = t('sdr_tune_timeout');
    timeoutError = { id, text, version };
    S.setStatusError(text);
    renderSdrState();
    requestRender();
  }, 3000) };
  send({ cmd: 'SET_SDR_CHANNEL', channel: currentSdrChannel(), freq: hz, enabled: true, request_id: id });
}

/** One request in flight; repeated input replaces the single unsent target. */
export function requestSdrTune(hz: number, options: { followViewport?: boolean; force?: boolean } = {}): void {
  if (!Number.isFinite(hz) || hz <= 0) return;
  clearTimeoutError();
  const followViewport = options.followViewport !== false;
  if (inFlight) {
    cancelReceiverFocus();
    queued = { hz, followViewport };
    sdrListenHz.set(hz);
    sdrListenHz.touch(hz);
  } else if (options.force || !sdrPrimaryEnabled.get() || Math.abs(hz - (sdrListenHz.confirmedValue() ?? 0)) >= 0.5) submit(hz, followViewport);
  renderSdrState();
  requestRender();
}

/** Read before confirming a report: plot tuning keeps its frequency axis under the pointer. */
export function sdrTuneFollowsViewport(hz: number, requestId?: unknown): boolean {
  // Older confirmations must not scroll toward a target already superseded by input.
  if (queued) return Math.abs(queued.hz - hz) < 0.5 && queued.followViewport;
  return !inFlight || inFlight.followViewport
    || (requestId !== inFlight.id && Math.abs(inFlight.hz - hz) >= 0.5);
}

/** Drop unsent repeat input on blur, while allowing the current request to settle. */
export function stopSdrTuneRepeat(): void {
  queued = null;
  if (inFlight) {
    sdrListenHz.set(inFlight.hz);
    sdrListenHz.touch(inFlight.hz);
  }
}

export function confirmSdrTuneReport(hz: number, responseTo: unknown, requestId: unknown,
  configVersion: unknown, options: { acceptSnapshot?: boolean; contextChanged?: boolean; receiverEnabled?: boolean } = {}): boolean {
  const own = typeof requestId === 'string' && requestId.startsWith('sdr-tune-');
  const nextVersion = typeof configVersion === 'number' && Number.isFinite(configVersion) ? configVersion : -1;
  const matching = inFlight !== null && responseTo === 'SET_SDR_CHANNEL' && requestId === inFlight.id;
  const accepted = options.acceptSnapshot !== false && !(nextVersion >= 0 && nextVersion < version)
    && (!own || requestId === inFlight?.id || nextVersion > version);
  if (responseTo === 'SET_SDR_CHANNEL' && requestId === timeoutError?.id) clearTimeoutError();
  if (accepted) {
    if (typeof options.receiverEnabled === 'boolean') sdrPrimaryEnabled.confirm(options.receiverEnabled);
    if (options.contextChanged && !matching) cancelSdrTuneRequest();
    if (nextVersion >= 0) version = nextVersion;
    if (Number.isFinite(hz) && hz > 0) {
      sdrListenHz.confirm(hz);
      if (timeoutError && nextVersion > timeoutError.version) clearTimeoutError();
    }
  }
  if (matching && inFlight) {
    window.clearTimeout(inFlight.timer);
    inFlight = null;
    const next = queued;
    queued = null;
    sdrListenHz.reset();
    if (next !== null && Math.abs(next.hz - (sdrListenHz.confirmedValue() ?? 0)) >= 0.5) {
      submit(next.hz, next.followViewport);
    }
    if (!accepted) {
      renderSdrState();
      requestRender();
    }
  } else if (accepted && inFlight) {
    const target = queued?.hz ?? inFlight.hz;
    sdrListenHz.set(target);
    sdrListenHz.touch(target);
  }
  return accepted;
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
  sdrPrimaryEnabled.confirm(true);
}
