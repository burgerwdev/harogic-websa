import { afterEach, beforeEach, expect, it, vi } from 'vitest';

let dispose: (() => void) | undefined;
beforeEach(() => { vi.resetModules(); localStorage.clear(); document.body.innerHTML = '<button id="btn-sdr-eq">EQ</button>'; });
afterEach(() => { dispose?.(); dispose = undefined; document.body.replaceChildren(); localStorage.clear(); vi.restoreAllMocks(); });

it('edits the shared speaker EQ, preserves it on close, and resets all values to flat', async () => {
  const eq = await import('../audio/equalizer');
  const ui = await import('../ui/sdrEqualizer');
  dispose = ui.initSdrEqualizer();
  const send = vi.spyOn(window, 'fetch');
  document.querySelector<HTMLButtonElement>('#btn-sdr-eq')!.click();
  const panel = document.querySelector<HTMLElement>('.sdr-eq-window')!;
  const slider = panel.querySelector<HTMLInputElement>('[data-eq-band="2"]')!;
  slider.value = '5.5'; slider.dispatchEvent(new Event('input', { bubbles: true }));
  expect(eq.getEqualizer().gains).toEqual([0, 0, 5.5, 0, 0]);
  expect(slider.getAttribute('aria-valuetext')).toBe('+5.5 dB');
  panel.querySelector<HTMLButtonElement>('[data-eq-bypass]')!.click(); expect(eq.equalizerActive()).toBe(false);
  panel.querySelector<HTMLButtonElement>('[data-close]')!.click(); expect(panel.hidden).toBe(true);
  document.querySelector<HTMLButtonElement>('#btn-sdr-eq')!.click(); expect(panel.hidden).toBe(false);
  expect(eq.getEqualizer().gains[2]).toBe(5.5);
  const profile = panel.querySelector<HTMLSelectElement>('[data-eq-preset]')!;
  profile.value = 'voice'; profile.dispatchEvent(new Event('change', { bubbles: true }));
  expect(eq.getEqualizer()).toEqual({ enabled: true, gains: [-6, -2, 2, 3, -4], trim: -3 });
  eq.resetEqualizer(); expect(eq.getEqualizer()).toEqual(eq.equalizerDefaults()); expect(slider.value).toBe('0');
  expect(localStorage.getItem('websa-speaker-eq-v1')).toBeNull();
  expect(send).not.toHaveBeenCalled();
});

it('restores valid preferences and rejects corrupt or out-of-range gains', async () => {
  localStorage.setItem('websa-speaker-eq-v1', JSON.stringify({ enabled: false, gains: [1, 2, 3, 4, 5], trim: -4 }));
  const eq = await import('../audio/equalizer');
  expect(eq.getEqualizer()).toEqual({ enabled: false, gains: [1, 2, 3, 4, 5], trim: -4 });
  eq.setEqualizer({ gains: [0, 0, 30, 0, 0] }); expect(eq.getEqualizer().gains[2]).toBe(3);
  eq.setEqualizer({ trim: Number.NaN }); expect(eq.getEqualizer().trim).toBe(-4);
  eq.resetEqualizer(); expect(eq.getEqualizer()).toEqual(eq.equalizerDefaults());
});
