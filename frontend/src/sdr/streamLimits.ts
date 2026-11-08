import wire from '../../../contracts/stream-limits.json';

// Shared wire contract, compiled into both Rust and the browser workers.
export const MAX_DECODE_BYTES = wire.MAX_DECODE_EVENT_BYTES;
export const MAX_QUEUED_EVENTS = 2 * wire.DECODE_ACK_WINDOW;
export const MAX_QUEUED_BYTES = wire.DECODE_ACK_BYTES;

// Browser ingress/fallback buffers, per subscription. AudioWorklet latency
// targets are separate playback policy, not receiver/channel capacity.
export const AUDIO_QUEUE_SECONDS = 3;

// Subscription retries do not reconfigure DSP or restart recordings. These are
// independent of the backend reception supervisor's processor-recovery budget.
export const STREAM_RETRY = Object.freeze({
  networkInitialMs: 500,
  networkMaxMs: 5000,
  interruptionInitialMs: 1000,
  interruptionAttempts: 3,
  stableProgressMs: 30_000,
});
