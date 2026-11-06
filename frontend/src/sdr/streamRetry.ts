import { STREAM_RETRY } from './streamLimits';

/** Retry subscriptions, never receiver configuration or recording tasks. */
export class StreamRetry {
  private networkAttempts = 0;
  private interruptions = 0;
  private progressingSince: number | null = null;

  progress(): void {
    const now = Date.now();
    this.progressingSince ??= now;
    // A socket opening (or replaying a start marker) alone is not recovery.
    if (now - this.progressingSince >= STREAM_RETRY.stableProgressMs) this.interruptions = 0;
    this.networkAttempts = 0;
  }

  delay(code: number, reason: string): number | null {
    this.progressingSince = null;
    if ([1000, 1001, 1006, 1012].includes(code)) {
      return Math.min(STREAM_RETRY.networkInitialMs * 2 ** Math.min(this.networkAttempts++,
        Math.ceil(Math.log2(STREAM_RETRY.networkMaxMs / STREAM_RETRY.networkInitialMs))), STREAM_RETRY.networkMaxMs);
    }
    if (code === 1013 || code === 1011 && ['receiver failed', 'subscriber source stream invalid'].includes(reason)) {
      if (this.interruptions >= STREAM_RETRY.interruptionAttempts) return null;
      return STREAM_RETRY.interruptionInitialMs * 2 ** this.interruptions++;
    }
    return null;
  }
}
