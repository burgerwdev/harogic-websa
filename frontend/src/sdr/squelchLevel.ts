import { sdrListenHz } from '../ui/sdrState';
import { setSdrSquelchLevel } from './iqStream';

/** Pre-AGC panadapter power at the tuned frequency, or no usable measurement. */
export function channelPowerDbm(freq: Float64Array, spec: Float32Array, listen: number): number | null {
  if (freq.length < 2 || freq.length !== spec.length || !Number.isFinite(listen)) return null;
  const bin = Math.abs(freq[1] - freq[0]);
  if (!(bin > 0)) return null;
  let peak: number | null = null;
  for (let i = 0; i < freq.length; i++) {
    const power = spec[i];
    if (Number.isFinite(power) && Math.abs(freq[i] - listen) <= bin) {
      peak = peak === null ? power : Math.max(peak, power);
    }
  }
  return peak;
}

let lastSentAt = -Infinity;
export function feedSquelchLevel(freq: Float64Array, spec: Float32Array): void {
  const power = channelPowerDbm(freq, spec, sdrListenHz.get());
  if (power === null) return;
  const now = performance.now();
  if (now - lastSentAt < 100) return;
  lastSentAt = now;
  setSdrSquelchLevel(power);
}
