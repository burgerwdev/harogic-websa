import { createParam } from '../core/params';

/** Display preference shared by all channel and hover overlays on both plots. */
export const sdrPassbandVisible = createParam<boolean>('sdr.passbandVisible', {
  fallback: true, scope: 'sdr-display', authoritative: true,
  persistKey: 'websa-sdr-passband-visible', persist: 'desired',
  parse: value => value === 'true' ? true : value === 'false' ? false : null,
  serialize: String,
});
