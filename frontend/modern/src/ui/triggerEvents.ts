// Trigger hit detection from the frame path.
//
// While a level trigger is armed the device sends nothing until the threshold is crossed,
// so the FIRST packet that arrives is the capture itself. Detecting it here (instead of
// polling the status endpoint) makes the canvas status flip in the same tick as the
// captured spectrum, which is what the user sees.
import * as S from '../core/store';

type Listener = () => void;
const listeners = new Set<Listener>();
// Only trust packets once the device itself reports that it is waiting for the trigger:
// right after arming, packets from the still free-running device are still in flight and
// would otherwise be mistaken for the capture.
let backendWaiting = false;
let armedAt = 0;

export function setBackendWaiting(v: boolean): void {
  backendWaiting = v;
}

/** Timestamp of the arm command, used to tell in-flight frames from a real capture. */
export function setArmedAt(ms: number): void {
  armedAt = ms;
}

const SETTLE_MS = 500;   // device reconfigures in ~0.35 s; in-flight frames arrive well before this

export function onTriggerHit(fn: Listener): void {
  listeners.add(fn);
}

/** Returns true when this packet was the trigger event (state was updated). */
export function noteFrameArrived(): boolean {
  if (!S.trigArmed || !S.trigWaiting) return false;
  // Frames still in flight when the arm command was sent must not count as a capture, but
  // the device reconfigures in ~0.35 s: anything arriving after the settle window really is
  // the trigger, even if the status poll has not confirmed "waiting" yet (a source already
  // above the threshold captures immediately, which used to be missed).
  if (!backendWaiting && performance.now() - armedAt < SETTLE_MS) return false;
  S.setTrigWaiting(false);
  S.setTrigHit(true);
  listeners.forEach((fn) => fn());
  return true;
}
