// Trigger hit detection from the frame path.
//
// While a level trigger is armed the device sends nothing until the threshold is crossed,
// so the FIRST packet that arrives is the capture itself. Detecting it here (instead of
// polling the status endpoint) makes the canvas status flip in the same tick as the
// captured spectrum, which is what the user sees.
import * as S from '../core/store';

type Listener = () => void;
const listeners = new Set<Listener>();

export function onTriggerHit(fn: Listener): void {
  listeners.add(fn);
}

/** Returns true when this packet was the trigger event (state was updated). */
export function noteFrameArrived(): boolean {
  if (!S.trigArmed || !S.trigWaiting) return false;
  S.setTrigWaiting(false);
  S.setTrigHit(true);
  listeners.forEach((fn) => fn());
  return true;
}
