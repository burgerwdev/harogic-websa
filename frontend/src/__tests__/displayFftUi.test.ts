import { beforeEach, expect, it, vi } from 'vitest';
import { initWfRange, syncDisplayFftUi } from '../ui/wfRange';
import { send } from '../core/wsSend';
vi.mock('../core/wsSend', () => ({ send: vi.fn() }));
beforeEach(() => {
  localStorage.clear(); vi.mocked(send).mockClear();
  document.body.innerHTML = '<select id="select-sdr-fft"><option value="0">Default</option><option value="32768">32768</option><option value="65536">65536</option></select><output id="sdr-fft-actual"></output>';
});
it('only displays confirmed sizes through request, warming, active and fallback states', () => {
  initWfRange(); syncDisplayFftUi(true, { available: true, size: 0 });
  const select = document.querySelector('select')!;
  select.value = '65536'; select.dispatchEvent(new Event('change'));
  expect(send).toHaveBeenCalledWith({ cmd: 'SET_SDR_FFT', size: 65536 });
  expect(select.value).toBe('0');
  syncDisplayFftUi(true, { available: true, size: 0, requested_size: 65536, state: 'warming' });
  expect(select.value).toBe('0');
  syncDisplayFftUi(true, { available: true, size: 65536, state: 'active', observation_ms: 16.78, bin_spacing_hz: 59.6, enbw_hz: 224.7 });
  expect(select.value).toBe('65536');
  expect(document.querySelector('output')!.textContent).toContain('16.8 ms');
  syncDisplayFftUi(true, { available: true, size: 0, error: 'SDK FFT failed' });
  expect(select.value).toBe('0');
  expect(document.querySelector('output')!.title).toBe('SDK FFT failed');
});
it('disables unsupported sources and never sends a tier from disabled controls', () => {
  initWfRange(); syncDisplayFftUi(true, { available: false, size: 0 });
  const select = document.querySelector('select')!;
  expect(select.disabled).toBe(true);
  select.value = '32768'; select.dispatchEvent(new Event('change'));
  expect(send).not.toHaveBeenCalled();
  expect(select.value).toBe('0');
  syncDisplayFftUi(false, { available: true, size: 65536 });
  expect(select.disabled).toBe(true);
});

it('leaves the native popup selection untouched by repeated status refreshes', () => {
  initWfRange(); syncDisplayFftUi(true, { available: true, size: 32768 });
  const select = document.querySelector('select')!;
  select.focus();
  const writeValue = vi.spyOn(select, 'value', 'set');
  for (let i = 0; i < 20; i++) syncDisplayFftUi(true, { available: true, size: 32768 });
  expect(writeValue).not.toHaveBeenCalled();
  syncDisplayFftUi(true, { available: true, size: 65536 });
  expect(select.value).toBe('65536');
  writeValue.mockRestore();
});

it('shows FFT only in SDR, including its disabled state while capture is unavailable', () => {
  const row = document.createElement('div'); row.id = 'sdr-fft-settings';
  document.body.append(row); row.append(document.querySelector('select')!);
  syncDisplayFftUi(false, undefined, 'std'); expect(row.hidden).toBe(true);
  syncDisplayFftUi(false, undefined, 'sdr'); expect(row.hidden).toBe(false);
  expect(document.querySelector('select')!.disabled).toBe(true);
  syncDisplayFftUi(true, { available: true, size: 0 }, 'sdr');
  expect(document.querySelector('select')!.disabled).toBe(false);
});
