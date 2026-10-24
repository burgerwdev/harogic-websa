import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../core/wsSend', () => ({ send: vi.fn(), onCommandSent: vi.fn() }));
vi.mock('../audio/sdrAudio', () => ({
	prepareSdrAudioTransition: vi.fn(), setSdrAudioEnabled: vi.fn(), setSdrAudioMode: vi.fn(),
}));
vi.mock('../sdr/iqStream', () => ({
	resetSdrIq: vi.fn(), setSdrIqEnabled: vi.fn(), setSstvModePin: vi.fn(),
}));
vi.mock('../ui/decodeWindow', () => ({
	canToggleAmSync: vi.fn(() => true), confirmAmSyncGeneration: vi.fn(), setAmInfo: vi.fn(),
	decodeChannel: () => 1, initDecodeWindow: vi.fn(), refreshDecodeWindow: vi.fn(),
	setDecodeActive: vi.fn(), setDecodeChannelMode: vi.fn(), toggleDecodeWindow: vi.fn(),
}));
vi.mock('../ui/refAutoScale', () => ({
	clearSdrEntryState: vi.fn(), postRefNotice: vi.fn(), requestSdrEntryFit: vi.fn(), resetAutoScaleState: vi.fn(),
	autoScaleBusy: () => false, autoScaleRequest: vi.fn(), clearAutoScaleHint: vi.fn(),
}));

const listeners: Array<() => void> = [];

beforeEach(() => {
	vi.resetModules();
	vi.clearAllMocks();
	vi.useFakeTimers();
	localStorage.clear();
	document.getElementById('spectrum')!.addEventListener = vi.fn();
	document.getElementById('spectrum')!.removeEventListener = vi.fn();
	const add = EventTarget.prototype.addEventListener;
	vi.spyOn(EventTarget.prototype, 'addEventListener').mockImplementation(function (this: EventTarget, type, listener, options) {
		add.call(this, type, listener, options);
		if (this === document || this === window) {
			const target = this;
			listeners.push(() => target.removeEventListener(type, listener, options));
		}
	});
});
afterEach(() => {
	listeners.splice(0).forEach((remove) => remove());
	vi.restoreAllMocks();
	vi.clearAllTimers();
	vi.useRealTimers();
	localStorage.clear();
	document.body.innerHTML = '';
});

// Slots and entry snapshots must load after each test seeds storage; static imports retain
// the previous test's preferences instead of exercising the reload boundary.
async function load() {
	const controls = await import('../ui/controls');
	const state = await import('../ui/sdrState');
	const { send } = await import('../core/wsSend');
	const store = await import('../core/store');
	store.setDeviceConnected(true);
	return { controls, state, send };
}

function status(mode = 'sdr', demod = 'am', receiver: Record<string, unknown> = {}) {
	return { mode, connected: true, sdr: { center: 98e6, decimate: 16, listen: 98e6,
		demod, if_bw: 12000, channels: [{ channel: 1, enabled: true, freq: 98e6,
			mode: demod, if_bw: 12000, am_sync_generation: 0, ...receiver }] } };
}

function key(value: string) {
	const event = new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true });
	document.dispatchEvent(event);
	expect(event.defaultPrevented).toBe(true);
}

describe('SDR control intent and STATUS', () => {
	it.each([null, 145.5e6])('starts fresh at the swept centre or marker (%s), ignoring non-SDR receiver defaults', async (marker) => {
		const { controls, state } = await load();
		const store = await import('../core/store');
		const { centerHz, swpCenterHz } = await import('../ui/freqState');
		centerHz.confirm(999e6);
		swpCenterHz.confirm(144e6);
		store.markers[0].enabled = marker !== null;
		store.markers[0].freq = marker;
		controls.syncSdrPanel(status('std'));
		expect(state.hasStoredSdrPrefs()).toBe(false);
		expect(state.sdrCenterHz.confirmedValue()).toBeNull();
		controls.setGraphMode('sdr');
		expect(state.sdrCenterHz.get()).toBe(marker ?? 144e6);
	});

	it('returns to saved SDR tuning and listening preferences after swept STATUS', async () => {
		localStorage.setItem('web-sa-sdr-center', '7100000');
		localStorage.setItem('web-sa-sdr-listen', '7074000');
		localStorage.setItem('web-sa-sdr-demod', 'ft8');
		localStorage.setItem('web-sa-sdr-ifbw', '3000');
		const { controls, state } = await load();
		controls.syncSdrPanel(status('std'));
		controls.syncSdrPanel(status('rta', 'wfm'));
		controls.setGraphMode('sdr');
		expect(state.sdrCenterHz.get()).toBe(7100000);
		expect(state.sdrListenHz.get()).toBe(7074000);
		expect(state.sdrDemod.get()).toBe('ft8');
		expect(state.sdrIfbw.get()).toBe(3000);
		expect(localStorage.getItem('web-sa-sdr-center')).toBe('7100000');
	});

	it.each(['wfm', 'cw', 'ft8'].flatMap((mode) => ['keyboard', 'panel'].map((via) => ({ mode, via }))))(
		'$via selection gives $mode its usable IF bandwidth', async ({ mode, via }) => {
		const { controls, state, send } = await load();
		const { graphMode } = await import('../ui/graphMode');
		const { setPluginManifest } = await import('../sdr/registry');
		setPluginManifest(['am', mode].map((id) => ({ id, kind: id === 'ft8' ? 'digital' as const : 'analog' as const,
			implemented: true, audioEnhancement: false })));
		graphMode.confirm('sdr');
		state.sdrIfbw.confirm(mode === 'wfm' ? 6000 : 180000);
		controls.bindCanvas();
		if (via === 'keyboard') key('M');
		else controls.selectSdrDemod(mode);
		const expected = mode === 'wfm' ? 180000 : 3000;
		expect(state.sdrDemod.get()).toBe(mode);
		expect(state.sdrIfbw.get()).toBe(expected);
		expect(send).toHaveBeenCalledWith(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', mode, ifbw: expected }));
	});

	it.each(['PageUp', 'PageDown'])('preserves the manual %s bandwidth across subsequent mode selections', async (page) => {
		const { controls, state } = await load();
		const { graphMode } = await import('../ui/graphMode');
		graphMode.confirm('sdr');
		state.sdrIfbw.confirm(6000);
		controls.bindCanvas();
		key(page);
		const manual = page === 'PageUp' ? 12000 : 3000;
		expect(state.sdrIfbw.get()).toBe(manual);
		for (const mode of ['wfm', 'cw', 'ft8']) {
			controls.selectSdrDemod(mode);
			expect(state.sdrIfbw.get()).toBe(manual);
		}
	});
	it('tunes immediately after quick-step selection while preserving button activation and field editing', async () => {
		document.body.insertAdjacentHTML('beforeend', '<div id="sdr-step-quick"></div><input id="input-sdr-listen">');
		const { controls, state, send } = await load();
		const { graphMode } = await import('../ui/graphMode');
		graphMode.confirm('sdr');
		state.sdrCenterHz.confirm(9_810_000); state.sdrSpanHz.confirm(100_000); state.sdrListenHz.confirm(9_810_000);
		controls.bindActions(); controls.bindCanvas();
		const button = document.querySelector<HTMLButtonElement>('[data-step-hz="100"]')!;
		button.focus(); button.click();
		key('ArrowRight');
		expect(state.sdrListenHz.get()).toBe(9_810_100);
		key('ArrowLeft');
		expect(state.sdrListenHz.get()).toBe(9_810_000);
		expect(send).toHaveBeenCalledWith({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 9_810_100 });
		const space = new KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true });
		button.dispatchEvent(space); expect(space.defaultPrevented).toBe(false);
		(document.getElementById('input-sdr-listen') as HTMLInputElement).focus();
		const arrow = new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true });
		document.activeElement!.dispatchEvent(arrow); expect(arrow.defaultPrevented).toBe(false);
		expect(state.sdrListenHz.get()).toBe(9_810_000);
	});

	it('keeps tenths of a dB distinct and reflects the threshold through STATUS', async () => {
		document.body.insertAdjacentHTML('beforeend', '<input type="range" id="input-sdr-squelch" min="-160" max="0" step="0.1"><span id="cur-sdr-squelch"></span>');
		const { controls, state, send } = await load();
		controls.bindActions(); controls.syncSdrPanel(status('sdr', 'am', { squelch: -110 }));
		const slider = document.getElementById('input-sdr-squelch') as HTMLInputElement;
		for (const threshold of [-110.1, -110.2]) {
			slider.value = String(threshold); slider.dispatchEvent(new Event('input')); slider.dispatchEvent(new Event('change'));
			expect(state.sdrSquelch.get()).toBe(threshold);
			expect(send).toHaveBeenLastCalledWith(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', squelch: threshold }));
			controls.syncSdrPanel(status('sdr', 'am', { squelch: threshold }));
			expect(slider.value).toBe(String(threshold));
			expect(document.getElementById('cur-sdr-squelch')!.textContent).toBe(`${threshold.toFixed(1)} dBm`);
		}
		controls.syncSdrPanel(status('sdr', 'am', { squelch: -160 }));
		expect(document.getElementById('cur-sdr-squelch')!.textContent).toBe('Off');
	});

	it.each(['drm', 'drmplus'])('disables unsupported %s controls and restores them on analog STATUS', async (mode) => {
		document.body.insertAdjacentHTML('beforeend', `
			<button id="btn-sdr-agc"></button><button id="btn-sdr-nr"></button>
			<input id="input-sdr-squelch"><select id="select-sdr-nr-algo"></select>
			<select id="select-sdr-nr-strength"></select><input id="input-sdr-nr-atten">
			<button data-sdr-deemph="50"></button><input id="input-sdr-volume">
			<button id="btn-sdr-audio"></button>`);
		const { controls } = await load();
		const unsupported = ['#btn-sdr-agc', '#btn-sdr-nr', '#input-sdr-squelch', '#select-sdr-nr-algo',
			'#select-sdr-nr-strength', '#input-sdr-nr-atten', '[data-sdr-deemph]'];
		controls.syncSdrPanel(status('sdr', mode, { nr_algorithm: 'wiener', volume: 0.35 }));
		for (const selector of unsupported) expect((document.querySelector(selector) as HTMLInputElement).disabled).toBe(true);
		expect((document.getElementById('input-sdr-volume') as HTMLInputElement).disabled).toBe(false);
		expect((document.getElementById('input-sdr-volume') as HTMLInputElement).value).toBe('0.35');
		expect((document.getElementById('btn-sdr-audio') as HTMLButtonElement).disabled).toBe(false);
		controls.toggleSdrAudio();
		const { setSdrAudioEnabled } = await import('../audio/sdrAudio');
		expect(setSdrAudioEnabled).toHaveBeenLastCalledWith(true);
		controls.syncSdrPanel(status('sdr', 'am', { nr_algorithm: 'wiener' }));
		for (const selector of unsupported) expect((document.querySelector(selector) as HTMLInputElement).disabled).toBe(false);
	});

	it('moves the CW receive overlay when primary STATUS reports a different pitch', async () => {
		const { controls, state } = await load();
		controls.syncSdrPanel(status('sdr', 'cw', { if_bw: 500, pitch: 950 }));
		expect(state.demodBandHz('cw', state.sdrIfbw.get())).toEqual([700, 1200]);
		controls.syncSdrPanel(status('sdr', 'cw', { if_bw: 500, pitch: 500 }));
		expect(state.demodBandHz('cw', state.sdrIfbw.get())).toEqual([250, 750]);
	});

	it.each([-1, 7])('replays stored SSTV pin %s only on confirmed SDR entry', async (pin) => {
		localStorage.setItem('web-sa-sdr-sstv-mode', String(pin));
		const { controls } = await load();
		const { setSstvModePin } = await import('../sdr/iqStream');
		expect(setSstvModePin).not.toHaveBeenCalled();
		controls.syncGraphModeStatus('std');
		expect(setSstvModePin).not.toHaveBeenCalled();
		controls.setGraphMode('sdr');
		expect(setSstvModePin).not.toHaveBeenCalled();
		controls.syncGraphModeStatus('sdr');
		expect(setSstvModePin).toHaveBeenCalledExactlyOnceWith(pin < 0 ? null : pin, 1);
		controls.syncGraphModeStatus('sdr');
		expect(setSstvModePin).toHaveBeenCalledTimes(1);
	});

	it('sends an AM detector toggle alone and waits through old STATUS for confirmation', async () => {
		const { controls, state, send } = await load();
		const { initDecodeWindow, refreshDecodeWindow, setAmInfo } = await import('../ui/decodeWindow');
		controls.syncGraphModeStatus('sdr');
		controls.syncSdrPanel(status('sdr', 'am', { am_sync: false }));
		controls.bindActions();
		const hooks = vi.mocked(initDecodeWindow).mock.calls.at(-1)![0];
		vi.mocked(send).mockClear();
		hooks.onAmSync!(true);
		expect(send).toHaveBeenCalledExactlyOnceWith({ cmd: 'SET_SDR_CHANNEL', channel: 1,
			am_sync: true, request_id: expect.any(String) });
		expect(setAmInfo).toHaveBeenLastCalledWith(null, 1);
		expect(state.sdrAmSync.pending()).toBe(true);
		expect(state.sdrAmSync.confirmedValue()).toBe(false);
		expect(localStorage.getItem('web-sa-sdr-am-sync')).toBe('0');
		controls.syncSdrPanel(status('sdr', 'am', { am_sync: false }));
		expect(state.sdrAmSync.desiredValue()).toBe(true);
		expect(state.sdrAmSync.pending()).toBe(true);
		controls.syncSdrPanel(status('sdr', 'am', { am_sync: true }));
		expect(state.sdrAmSync.confirmedValue()).toBe(true);
		expect(state.sdrAmSync.pending()).toBe(false);
		expect(localStorage.getItem('web-sa-sdr-am-sync')).toBe('1');
		vi.mocked(refreshDecodeWindow).mockClear();
		vi.advanceTimersByTime(3001);
		expect(refreshDecodeWindow).not.toHaveBeenCalled();
		controls.applySdrDemod();
		expect(send).toHaveBeenLastCalledWith(expect.objectContaining({ am_sync: true, mode: 'am', ifbw: 12000 }));
	});

	it.each(['rejection', 'timeout'])('restores the confirmed detector after %s', async (outcome) => {
		const { controls, state, send } = await load();
		const { initDecodeWindow, refreshDecodeWindow } = await import('../ui/decodeWindow');
		controls.syncGraphModeStatus('sdr');
		controls.syncSdrPanel(status('sdr', 'am', { am_sync: false }));
		controls.bindActions();
		vi.mocked(initDecodeWindow).mock.calls.at(-1)![0].onAmSync!(true);
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		vi.mocked(refreshDecodeWindow).mockClear();
		if (outcome === 'rejection') {
			controls.rejectSdrAmSyncRequest('SET_SDR_CHANNEL', 'unrelated-mode-edit');
			controls.rejectSdrAmSyncRequest('SET_SDR', request.request_id);
			expect(state.sdrAmSync.pending()).toBe(true);
			controls.rejectSdrAmSyncRequest('SET_SDR_CHANNEL', request.request_id);
		} else vi.advanceTimersByTime(3000);
		expect(state.sdrAmSync.pending()).toBe(false);
		expect(state.sdrAmSync.desiredValue()).toBeNull();
		expect(state.sdrAmSync.get()).toBe(false);
		expect(state.sdrAmSync.confirmedValue()).toBe(false);
		expect(localStorage.getItem('web-sa-sdr-am-sync')).toBe('0');
		expect(refreshDecodeWindow).toHaveBeenCalledOnce();
	});

	it('refuses detector requests outside the active primary AM receiver or during measurement ownership', async () => {
		const { controls, send } = await load();
		const { initDecodeWindow, canToggleAmSync } = await import('../ui/decodeWindow');
		const store = await import('../core/store');
		controls.bindActions();
		const hooks = vi.mocked(initDecodeWindow).mock.calls.at(-1)![0];
		hooks.onAmSync!(true);
		expect(send).not.toHaveBeenCalled();
		controls.syncGraphModeStatus('sdr');
		vi.mocked(send).mockClear();
		controls.syncSdrPanel(status('sdr', 'wfm', { am_sync: false }));
		hooks.onAmSync!(true);
		expect(send).not.toHaveBeenCalled();
		controls.syncSdrPanel(status('sdr', 'am', { am_sync: false }));
		vi.mocked(send).mockClear();
		vi.mocked(canToggleAmSync).mockReturnValue(false);
		hooks.onAmSync!(true);
		vi.mocked(canToggleAmSync).mockReturnValue(true);
		store.setMeasOn(true);
		store.setViewMode('harm');
		expect(hooks.canAmSync!()).toBe(false);
		hooks.onAmSync!(true);
		store.setMeasOn(false);
		controls.syncGraphModeStatus('pnm');
		expect(hooks.canAmSync!()).toBe(false);
		hooks.onAmSync!(true);
		expect(send).not.toHaveBeenCalled();
	});

	it.each([false, undefined])('requires connected true and drops pending detector intent for disconnected STATUS (%s)', async (connected) => {
		const { controls, state, send } = await load();
		const store = await import('../core/store');
		const { initDecodeWindow, setDecodeActive, confirmAmSyncGeneration, refreshDecodeWindow } = await import('../ui/decodeWindow');
		const { setSdrIqEnabled, resetSdrIq } = await import('../sdr/iqStream');
		controls.syncGraphModeStatus('sdr');
		controls.syncSdrPanel(status('sdr', 'am', { am_sync: false }));
		controls.bindActions();
		const hooks = vi.mocked(initDecodeWindow).mock.calls.at(-1)![0];
		hooks.onAmSync!(true);
		expect(state.sdrAmSync.pending()).toBe(true);
		store.setDeviceConnected(false);
		controls.syncSdrPanel({ ...status('sdr', 'am', { am_sync: true }), connected });
		expect(state.sdrAmSync.confirmedValue()).toBe(false);
		expect(state.sdrAmSync.desiredValue()).toBeNull();
		expect(state.sdrAmSync.pending()).toBe(false);
		expect(setDecodeActive).toHaveBeenLastCalledWith(false);
		expect(confirmAmSyncGeneration).toHaveBeenLastCalledWith(null);
		expect(setSdrIqEnabled).toHaveBeenLastCalledWith(false);
		expect(resetSdrIq).toHaveBeenCalled();
		expect(hooks.canAmSync!()).toBe(false);
		vi.mocked(send).mockClear();
		vi.mocked(refreshDecodeWindow).mockClear();
		hooks.onAmSync!(true);
		controls.syncGraphModeStatus('sdr');
		expect(setDecodeActive).toHaveBeenLastCalledWith(false);
		vi.advanceTimersByTime(3001);
		expect(send).not.toHaveBeenCalled();
		expect(refreshDecodeWindow).not.toHaveBeenCalled();
		store.setDeviceConnected(true);
		controls.syncSdrPanel(status('sdr', 'am', { am_sync: false }));
		expect(setDecodeActive).toHaveBeenLastCalledWith(true);
		expect(setSdrIqEnabled).toHaveBeenLastCalledWith(true);
		expect(confirmAmSyncGeneration).toHaveBeenLastCalledWith(0);
		expect(state.sdrAmSync.desiredValue()).toBeNull();
	});

	it.each([undefined, null, -1, 0x100000000, 1.5, '0'])('invalidates the AM telemetry generation for invalid STATUS generation %s', async (generation) => {
		const { controls } = await load();
		const { confirmAmSyncGeneration } = await import('../ui/decodeWindow');
		controls.syncSdrPanel(status('sdr', 'am', { am_sync: true, am_sync_generation: 0 }));
		expect(confirmAmSyncGeneration).toHaveBeenLastCalledWith(0);
		controls.syncSdrPanel(status('sdr', 'am', { am_sync: true, am_sync_generation: generation }));
		expect(confirmAmSyncGeneration).toHaveBeenLastCalledWith(null);
	});

	it('Preset clears a saved initial detector preference before the first SDR entry', async () => {
		localStorage.setItem('web-sa-sdr-am-sync', '1');
		const { controls, state, send } = await load();
		controls.syncGraphModeStatus('std');
		controls.presetAll();
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncPresetStatus('SET_PRESET', request.request_id);
		expect(localStorage.getItem('web-sa-sdr-am-sync')).toBeNull();
		expect(state.sdrAmSync.get()).toBe(false);
		vi.mocked(send).mockClear();
		controls.syncGraphModeStatus('sdr');
		expect(vi.mocked(send).mock.calls.some(([command]) => 'am_sync' in command && command.am_sync === true)).toBe(false);
	});
});
