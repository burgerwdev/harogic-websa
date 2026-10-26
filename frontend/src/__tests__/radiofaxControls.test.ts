import { afterEach, expect, it, vi } from 'vitest';
import { initDecodeWindow, setDecodeActive, setDecodeWindowAvailable } from '../ui/decodeWindow';
import { resetSdrState, sdrRadiofaxAcquisition, sdrRadiofaxIoc, sdrRadiofaxLpm } from '../ui/sdrState';

afterEach(() => { document.body.innerHTML = ''; vi.restoreAllMocks(); });

it('keeps manual, automatic and 180 LPM transitions valid', () => {
	resetSdrState();
	document.body.innerHTML = `<div id="decode-window"></div>
		<button id="radiofax-auto"></button><button id="radiofax-manual"></button>
		<select id="radiofax-ioc"><option>auto</option><option>288</option><option>576</option></select>
		<select id="radiofax-lpm"><option>auto</option><option>120</option><option>180</option></select>`;
	const send = vi.fn();
	initDecodeWindow({ onTune: () => {}, onSstvMode: () => {}, onRadiofaxConfig: send });
	setDecodeActive(true);
	setDecodeWindowAvailable('radiofax');
	document.getElementById('radiofax-manual')!.click();
	expect(send).toHaveBeenLastCalledWith({ radiofax_acquisition: 'manual', radiofax_ioc: '576', radiofax_lpm: '120' });
	const lpm = document.getElementById('radiofax-lpm') as HTMLSelectElement;
	lpm.value = '180';
	lpm.dispatchEvent(new Event('change'));
	expect(send).toHaveBeenLastCalledWith({ radiofax_acquisition: 'manual', radiofax_ioc: '576', radiofax_lpm: '180' });
	sdrRadiofaxAcquisition.set('manual');
	sdrRadiofaxIoc.set('576');
	sdrRadiofaxLpm.set('180');
	sdrRadiofaxAcquisition.confirm('manual');
	sdrRadiofaxIoc.confirm('576');
	sdrRadiofaxLpm.confirm('180');
	document.getElementById('radiofax-auto')!.click();
	expect(send).toHaveBeenLastCalledWith({ radiofax_acquisition: 'auto', radiofax_lpm: 'auto' });
	const ioc = document.getElementById('radiofax-ioc') as HTMLSelectElement;
	ioc.value = 'auto';
	ioc.dispatchEvent(new Event('change'));
	expect(send).toHaveBeenLastCalledWith({ radiofax_ioc: 'auto', radiofax_acquisition: 'auto', radiofax_lpm: 'auto' });
});
