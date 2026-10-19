/// Squelch gate level feed: the panadapter's channel power (dBm) at the listen
/// frequency, sampled from the RTAF frames the display socket already receives.
///
/// The gate must reference a PRE-AGC quantity: the audio PCM is AGC-normalized
/// (constant RMS regardless of signal strength), which is what reduced the squelch
/// to an on/off switch. The panadapter is computed from the raw capture before any
/// AGC and measured accurate to +0.21 dB against a known source (FINDINGS #19).
import { sdrListenHz } from '../ui/sdrState';
import { setSdrSquelchLevel } from './iqStream';

const MIN_INTERVAL_MS = 100;

let lastSentAt = 0;

/** Test seam: clears the throttle so consecutive calls can be asserted independently. */
export function resetSquelchLevelFeed(): void {
  lastSentAt = 0;
}

/// Extract the power bin at the listen frequency and forward it to the DSP worker
/// (throttled; the gate's hysteresis/hold does the rest). No-op when the listen
/// frequency is outside the displayed window.
export function feedSquelchLevel(freqs: Float64Array, spec: Float32Array): void {
  const listen = sdrListenHz.get();
  if (!(listen > 0) || freqs.length < 2) return;
  const step = Math.abs(freqs[1] - freqs[0]);
  if (!(step > 0)) return;
  const lo = listen - step;
  const hi = listen + step;
  let best: number | null = null;
  for (let i = 0; i < freqs.length; i++) {
    const f = freqs[i];
    if (f >= lo && f <= hi) {
      const v = spec[i];
      if (Number.isFinite(v) && (best === null || v > best)) best = v;
    }
  }
  if (best === null) return;
  const now = performance.now();
  if (now - lastSentAt < MIN_INTERVAL_MS) return;
  lastSentAt = now;
  setSdrSquelchLevel(best);
}
