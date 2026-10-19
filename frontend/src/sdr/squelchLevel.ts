/// Squelch gate level feed: per RTAF frame, measure the channel power and the
/// off-channel noise floor on the panadapter, and forward the MODE-SELECTED metric
/// to the DSP worker's squelch gate.
///
/// Both quantities are PRE-AGC (the panadapter is computed from the raw capture):
/// the audio PCM is AGC-normalized to a constant RMS, which is what reduced the
/// squelch to an on/off switch when it referenced the audio level.
///
/// Modes:
///   power — gate level = channel power (dBm at the listen frequency). The classic
///           carrier/power squelch.
///   snr   — gate level = channel − noise floor (dB). Tracks a noisy band better;
///           needs the panadapter noise estimate (25th percentile off-channel).
import { sdrListenHz, sdrSquelchMode, type SquelchMode } from '../ui/sdrState';
import { setSdrSquelchLevel } from './iqStream';

const MIN_INTERVAL_MS = 100;
const EXCLUDE_BINS = 3;      // keep signal + modulation sidebands out of the noise estimate
const NOISE_PERCENTILE = 25; // robust vs a residual in-band carrier

let lastSentAt = 0;
let lastChannelDbm: number | null = null;
let lastNoiseDbm: number | null = null;

/** Test seam: clears the throttle so consecutive calls can be asserted independently. */
export function resetSquelchLevelFeed(): void {
  lastSentAt = 0;
  lastChannelDbm = null;
  lastNoiseDbm = null;
}

/** Diagnostics: the last measured pair (dBm, dB). */
export function lastSquelchLevels(): { channel: number | null; noise: number | null } {
  return { channel: lastChannelDbm, noise: lastNoiseDbm };
}

function metricFor(mode: SquelchMode, channel: number, noise: number): number {
  return mode === 'snr' ? channel - noise : channel;
}

/// Extract the mode's gate level from one RTAF frame and forward it (throttled).
/// No-op when the listen frequency is outside the displayed window.
export function feedSquelchLevel(freqs: Float64Array, spec: Float32Array): void {
  const listen = sdrListenHz.get();
  if (!(listen > 0) || freqs.length < 4) return;
  const step = Math.abs(freqs[1] - freqs[0]);
  if (!(step > 0)) return;

  const exclude = EXCLUDE_BINS * step;
  let channel: number | null = null;
  const noiseBins: number[] = [];
  for (let i = 0; i < freqs.length; i++) {
    const f = freqs[i];
    const v = spec[i];
    if (!Number.isFinite(v)) continue;
    if (Math.abs(f - listen) <= exclude) {
      if (channel === null || v > channel) channel = v;
    } else {
      noiseBins.push(v);
    }
  }
  if (channel === null || noiseBins.length < 4) return;
  noiseBins.sort((a, b) => a - b);
  const noise = noiseBins[Math.floor((noiseBins.length - 1) * NOISE_PERCENTILE / 100)];
  lastChannelDbm = channel;
  lastNoiseDbm = noise;

  const now = performance.now();
  if (now - lastSentAt < MIN_INTERVAL_MS) return;
  lastSentAt = now;
  setSdrSquelchLevel(metricFor(sdrSquelchMode.get(), channel, noise));
}
