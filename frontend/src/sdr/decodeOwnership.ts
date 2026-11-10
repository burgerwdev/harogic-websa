import type { DecodeEnvelope } from './decodeProtocol';

let owned = false;
const ownership = new Set<(native: boolean) => void>();
const consumers = new Set<(envelopes: readonly DecodeEnvelope[]) => void>();

export const nativeDecodeOwned = (): boolean => owned;
export function onDecodeOwnership(fn: (native: boolean) => void): () => void { ownership.add(fn); return () => ownership.delete(fn); }
export function onNativeDecode(fn: (envelopes: readonly DecodeEnvelope[]) => void): () => void { consumers.add(fn); return () => consumers.delete(fn); }
export function publishNativeDecode(envelopes: readonly DecodeEnvelope[]): void { if (owned) for (const fn of [...consumers]) fn(envelopes); }
/** Native UID workers also feed the main panel; never subscribe to every source twice. */
export function claimNativeDecode(): () => void {
  if (owned) throw new Error('Native decoded output already has an owner');
  owned = true; for (const fn of [...ownership]) fn(true);
  let released = false;
  return () => { if (released) return; released = true; owned = false; for (const fn of [...ownership]) fn(false); };
}
