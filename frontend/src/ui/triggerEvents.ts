// Leaf seam shared by UI actions, STATUS, and decoded RTA capture packets.
import * as S from '../core/store';

export interface TriggerMetadata {
  source?: string | null;
  generation?: number | null;
  frames?: number | null;
  capture_id?: number | null;
  packet_index?: number | null;
  packet_count?: number | null;
  waiting?: boolean | null;
  complete?: boolean | null;
  simulated?: boolean;
  supported?: boolean;
  valid?: boolean;
  trigger_missed?: boolean | null;
  last_sdk_status?: number | null;
}
type Listener = () => void;
const listeners = new Set<Listener>();
const resetListeners = new Set<Listener>();
let latest: TriggerMetadata = {};
let baselineGeneration: number | null = null;
let localArm = false;
let held = false;
let displayedGeneration: number | null = null;
let awaitingFree = false;
let missed = false;
let unsupported = false;
export function triggerUnsupported(): boolean { return unsupported; }
export function triggerMissed(): boolean { return missed; }

export function onTriggerHit(fn: Listener): void { listeners.add(fn); }
export function onTriggerReset(fn: Listener): void { resetListeners.add(fn); }
export function triggerHeld(): boolean { return held; }

export function resetTriggerRuntime(): void {
  localArm = false;
  baselineGeneration = null;
  held = false;
  displayedGeneration = null;
  awaitingFree = false;
  missed = false;
  S.setSwpArmed(false); S.setSwpHold(false); S.setSwpPrev(null);
  S.setTrigArmed(false); S.setTrigWaiting(false); S.setTrigHit(false);
  S.setTrigOverlay([]);
  resetListeners.forEach((fn) => fn());
}
export function resetTriggerConnection(): void {
  latest = {};
  unsupported = false;
  resetTriggerRuntime();
}

export function beginHardwareCapture(): boolean {
  if (typeof latest.generation !== 'number') return false;
  resetTriggerRuntime();
  localArm = true;
  baselineGeneration = typeof latest.generation === 'number' ? latest.generation : null;
  unsupported = latest.simulated === true && latest.supported === false;
  S.setTrigArmed(true); S.setTrigWaiting(true);
  return true;
}
export function releaseHardwareCapture(): void {
  resetTriggerRuntime();
  awaitingFree = true;
}

/** Ignore packets from before a local arm; clear accumulation only on its first confirmed packet. */
export function prepareTriggerFrame(metadata?: TriggerMetadata | null): 'ignore' | 'first' | 'continue' {
  if (metadata?.generation != null && latest.generation != null && metadata.generation < latest.generation) return 'ignore';
  if (metadata?.generation != null && displayedGeneration != null && metadata.generation < displayedGeneration) return 'ignore';
  if (awaitingFree) {
    if (metadata?.source !== 'bus' && metadata?.source !== 'freerun') return 'ignore';
    awaitingFree = false;
  }
  if (!localArm && metadata?.generation != null && (latest.generation == null || metadata.generation > latest.generation)) confirmTriggerMetadata(metadata);
  if (held) return 'ignore';
  if (!localArm && !S.trigArmed) return 'continue';
  if (!metadata || metadata.simulated || metadata.supported === false || metadata.valid === false) return 'ignore';
  if (metadata.source !== 'level' && metadata.source !== 'external' && metadata.source !== 'timer') return 'ignore';
  if (localArm && (baselineGeneration === null || metadata.generation == null || metadata.generation <= baselineGeneration)) return 'ignore';
  if (metadata.generation == null) return 'ignore';
  latest = metadata;
  if (displayedGeneration !== metadata.generation) {
    displayedGeneration = metadata.generation;
    return 'first';
  }
  return 'continue';
}
export function confirmTriggerMetadata(metadata: TriggerMetadata): void {
  if (latest.generation != null && metadata.generation != null && metadata.generation < latest.generation) return;
  if (held && latest.generation != null && metadata.generation != null && metadata.generation > latest.generation) resetTriggerRuntime();
  if (awaitingFree) {
    if (metadata.source !== 'bus' && metadata.source !== 'freerun') return;
    awaitingFree = false;
  }
  latest = metadata;
  unsupported = metadata.simulated === true && metadata.supported === false;
  if (metadata.source === 'bus' || metadata.source === 'freerun') {
    // An older STATUS can arrive before our arm command is dispatched.
    if (!localArm || (baselineGeneration !== null && metadata.generation != null && metadata.generation > baselineGeneration)) resetTriggerRuntime();
    return;
  }
  if (held || metadata.source == null || metadata.valid === false) return;
  if (localArm && (baselineGeneration === null || metadata.generation == null || metadata.generation <= baselineGeneration)) return;
  S.setTrigArmed(true);
  missed = metadata.trigger_missed === true;
  S.setTrigWaiting(true);
}

/** Only a complete, identified hardware capture can freeze the displayed packet. */
export function noteFrameArrived(metadata?: TriggerMetadata | null): boolean {
  if (!metadata || held || !S.trigArmed || metadata.simulated || metadata.supported === false || metadata.valid === false) return false;
  if (metadata.generation != null && latest.generation != null && metadata.generation < latest.generation) return false;
  if (metadata.generation != null && displayedGeneration != null && metadata.generation < displayedGeneration) return false;
  if (metadata.source == null || metadata.source === 'bus' || metadata.source === 'freerun') return false;
  if (localArm && (baselineGeneration === null || metadata.generation == null || metadata.generation <= baselineGeneration)) return false;
  missed = metadata.trigger_missed === true;
  if (metadata.complete !== true || metadata.generation == null || metadata.frames == null) return false;
  latest = metadata;
  localArm = false;
  held = true;
  S.setTrigArmed(false); S.setTrigWaiting(false); S.setTrigHit(true);
  listeners.forEach((fn) => fn());
  return true;
}
