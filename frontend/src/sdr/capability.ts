// Capability policy: may the browser run the DSP, or does the Python path serve audio?
//
// The browser DSP is the architecture's path, and the Python DSP is its fallback *and* its numeric
// reference. Which one is active must be an explicit, inspectable decision rather than an accident
// of whether a fetch succeeded, because the two produce audio through different sockets and a silent
// switch between them is indistinguishable from "the radio stopped working".
//
//   * `?wasm=0` (or `websa-dsp=off` in localStorage) forces the Python fallback — used by the e2e to
//     prove the fallback still plays, and by an operator who wants the reference implementation.
//   * a browser without WebAssembly cannot run the pipeline at all.
//   * anything else uses the browser DSP.
export const DSP_PREF_KEY = 'websa-dsp';

/** True when the browser can run the WASM pipeline at all. */
export function wasmSupported(): boolean {
  return typeof WebAssembly === 'object' && typeof WebAssembly.instantiate === 'function';
}

/**
 * True when the browser DSP should be used. `search` and `storage` are injectable so the policy can
 * be unit tested without a browser.
 */
export function wasmDspAllowed(
  search: string = location.search,
  storage: Pick<Storage, 'getItem'> | null = safeStorage(),
): boolean {
  if (!wasmSupported()) return false;
  const param = new URLSearchParams(search).get('wasm');
  if (param === '0' || param === 'off' || param === 'false') return false;
  if (param === '1' || param === 'on' || param === 'true') return true;
  try {
    return storage?.getItem(DSP_PREF_KEY) !== 'off';
  } catch {
    // A blocked localStorage (private mode) must not decide the DSP path.
    return true;
  }
}

/** Why the fallback is active, for the status readout (same inputs as `wasmDspAllowed`). */
export function wasmDspReason(
  search: string = location.search,
  storage: Pick<Storage, 'getItem'> | null = safeStorage(),
): string {
  if (!wasmSupported()) return 'no-webassembly';
  const param = new URLSearchParams(search).get('wasm');
  if (param === '0' || param === 'off' || param === 'false') return 'requested';
  if (wasmDspAllowed(search, storage)) return '';
  return 'stored-preference';
}

function safeStorage(): Pick<Storage, 'getItem'> | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}
