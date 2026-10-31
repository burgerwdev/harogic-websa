import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { receiverPlugin } from './pluginFixtures';

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
	const { setPluginManifest } = await import('../sdr/registry');
	setPluginManifest(['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm', 'ft8', 'drm', 'drmplus', 'sstv', 'radiofax'].map((id) => receiverPlugin(id)));
	const { deviceMode } = await import('../ui/graphMode');
	deviceMode.confirm('sdr');
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

describe('SDR zoom follows confirmed tuning', () => {
	const CENTER = 98e6;
	const SPAN = 3.125e6;
	const SELECTION = { lo: 96.8e6, hi: 97.2e6 };
	const captureAt = (center: number) => ({ lo: center - SPAN / 2, hi: center + SPAN / 2 });
	function report(center: number, listen: number, version: number, responseTo = 'STATUS', requestId?: string) {
		const snapshot = status('sdr', 'am', { freq: listen });
		const capture = captureAt(center);
		return { ...snapshot, config_version: version, response_to: responseTo, request_id: requestId,
			sdr: { ...snapshot.sdr, center, listen, actual: { start: capture.lo, stop: capture.hi } } };
	}
	async function setupZoom() {
		const loaded = await load();
		const viewport = await import('../ui/spectrumViewport');
		const store = await import('../core/store');
		const { graphMode } = await import('../ui/graphMode');
		graphMode.confirm('sdr');
		store.setSdrMode(true);
		loaded.controls.syncSdrPanel(report(CENTER, CENTER, 1));
		viewport.__testSeedCapture(captureAt(CENTER));
		store.setFreqArray(Float64Array.from({ length: 101 }, (_, i) => captureAt(CENTER).lo + i * SPAN / 100));
		viewport.zoomEnabled.set(true);
		viewport.setView(SELECTION, 1);
		vi.mocked(loaded.send).mockClear();
		return { ...loaded, viewport };
	}

	it('centers the zoom selection on To Center even when capture and listen frequencies already match', async () => {
		const { controls, state, send, viewport } = await setupZoom();
		document.body.insertAdjacentHTML('beforeend', '<button data-action="sdr-to-center">To Center</button>');
		controls.bindActions();
		(document.querySelector('[data-action="sdr-to-center"]') as HTMLButtonElement).click();
		expect(viewport.getView()).toEqual({ lo: CENTER - 200_000, hi: CENTER + 200_000 });
		expect(state.sdrListenHz.get()).toBe(CENTER);
		expect(send).toHaveBeenCalledWith({ cmd: 'SET_SDR', center: CENTER, decimate: 16 });
		const moved = { lo: CENTER + 800_000, hi: CENTER + 1_200_000 };
		viewport.setView(moved, 1);
		controls.syncSdrPanel(report(CENTER, CENTER, 2, 'SET_SDR'));
		expect(viewport.getView()).toEqual(moved);
		controls.sdrToCenter();
		expect(viewport.getView()).toEqual({ lo: CENTER - 200_000, hi: CENTER + 200_000 });
	});

	it('lets To Center replace an unsettled axis preview', async () => {
		const { controls, viewport } = await setupZoom();
		const store = await import('../core/store');
		const axis = await import('../ui/axisDrag');
		const capture = captureAt(CENTER);
		store.setRtaData({ freq: new Float64Array([capture.lo, capture.hi]), startHz: capture.lo, stopHz: capture.hi });
		document.body.insertAdjacentHTML('beforeend', '<select id="select-sdr-decimate"><option value="16">16</option></select>');
		axis.axisDragStart('x', 400, 424, { x: 10, y: 14, w: 800, h: 400 });
		axis.axisDragMove(480, 424);
		axis.axisDragEnd();
		expect(axis.previewFreqWindow()).not.toBeNull();
		controls.sdrToCenter();
		expect(axis.previewFreqWindow()).toBeNull();
		expect(viewport.getView()).toEqual({ lo: CENTER - 200_000, hi: CENTER + 200_000 });
	});

	it('slides the zoom selection to the receiver when To Center is pressed', async () => {
		const { controls, viewport } = await setupZoom();
		vi.mocked(window.matchMedia).mockReturnValue({ matches: false } as MediaQueryList);
		vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
		vi.spyOn(window, 'requestAnimationFrame').mockImplementation(callback =>
			window.setTimeout(() => callback(performance.now()), 16));
		vi.spyOn(window, 'cancelAnimationFrame').mockImplementation(id => window.clearTimeout(id));
		controls.sdrToCenter();
		expect(viewport.getView()).toEqual(SELECTION);
		vi.advanceTimersByTime(80);
		expect(viewport.getView()!.lo).toBeGreaterThan(SELECTION.lo);
		expect(viewport.getView()!.lo).toBeLessThan(CENTER - 200_000);
		vi.advanceTimersByTime(viewport.VIEW_SLIDE_MS);
		expect(viewport.getView()).toEqual({ lo: CENTER - 200_000, hi: CENTER + 200_000 });
	});

	it.each(['before', 'after'] as const)('centers To Center after a changed capture is acknowledged %s its frame', async order => {
		const { controls, state, viewport } = await setupZoom();
		const listen = CENTER + 400_000;
		controls.syncSdrPanel(report(CENTER, listen, 2));
		viewport.setView(SELECTION, 1);
		controls.sdrToCenter();
		expect(viewport.getView()).toEqual(SELECTION);
		if (order === 'after') viewport.setCapture(captureAt(listen));
		controls.syncSdrPanel(report(listen, listen, 3, 'SET_SDR'));
		if (order === 'before') {
			expect(viewport.getView()).toEqual(SELECTION);
			viewport.setCapture(captureAt(listen));
		}
		expect(viewport.getView()).toEqual({ lo: listen - 200_000, hi: listen + 200_000 });
		expect(state.sdrListenHz.get()).toBe(listen);
	});

	it.each(['before', 'after'].flatMap(order => [false, true].map(identity => ({ order, identity }))))(
		'centers on the first To Center click when the $order-frame ACK retains old actual bounds (identity=$identity)', async ({ order, identity }) => {
		const { controls, state, send, viewport } = await setupZoom();
		const listen = CENTER + 400_000;
		controls.syncSdrPanel(report(CENTER, listen, 2));
		viewport.setView(SELECTION, 1);
		vi.mocked(send).mockClear();
		controls.sdrToCenter();
		const snapshot = report(listen, listen, 3, 'SET_SDR');
		const old = captureAt(CENTER);
		const stale = { ...snapshot, sdr: { ...snapshot.sdr,
			actual: { start: old.lo, stop: old.hi, ...(identity ? { center: CENTER, decimate: 16 } : {}) } } };
		if (order === 'after') viewport.setCapture(captureAt(listen));
		controls.syncSdrPanel(stale);
		if (order === 'before') {
			expect(viewport.getView()).toEqual(SELECTION);
			viewport.setCapture(captureAt(listen));
		}
		controls.syncSdrPanel({ ...stale, response_to: 'SET_SDR_CHANNEL' });
		controls.syncSdrPanel({ ...stale, response_to: 'STATUS' });
		viewport.setCapture(captureAt(listen));
		expect(viewport.getView()).toEqual({ lo: listen - 200_000, hi: listen + 200_000 });
		expect(state.sdrListenHz.get()).toBe(listen);
		expect(send).toHaveBeenCalledTimes(2);
	});

	it.each([9_810_000, 14_200_000, 125_000_000])('keeps the selected window when entering a listen frequency near %s Hz', async center => {
		const { controls, state, send, viewport } = await setupZoom();
		controls.syncSdrPanel(report(center, center, 2));
		viewport.setCapture(captureAt(center));
		const selection = { lo: center - 300_000, hi: center + 300_000 };
		viewport.setView(selection, 1);
		document.body.insertAdjacentHTML('beforeend', `<input id="input-sdr-listen" value="${(center - 200_000) / 1e6}">`);
		controls.bindActions();
		vi.mocked(send).mockClear();
		document.getElementById('input-sdr-listen')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncSdrPanel(report(center, center - 200_000, 3, 'SET_SDR_CHANNEL', request.request_id));
		expect(state.sdrListenHz.get()).toBe(center - 200_000);
		expect(state.sdrCenterHz.get()).toBe(center);
		expect(viewport.getView()).toEqual(selection);
		controls.syncSdrPanel(report(center, center - 200_000, 3));
		expect(viewport.getView()).toEqual(selection);
	});

	it.each(['before', 'after', 'status-first'] as const)('returns a zoomed axis pan to its original window with %s ACK delivery', async order => {
		const { controls, state, send, viewport } = await setupZoom();
		const store = await import('../core/store');
		const axis = await import('../ui/axisDrag');
		const center = 9_810_000;
		const selection = { lo: center - 250_000, hi: center + 150_000 };
		const rect = { x: 10, y: 14, w: 800, h: 400 };
		const draw = (c: number) => {
			const capture = captureAt(c);
			viewport.setCapture(capture);
			store.setRtaData({ freq: new Float64Array([capture.lo, capture.hi]), startHz: capture.lo, stopHz: capture.hi });
		};
		controls.syncSdrPanel(report(center, center, 2));
		draw(center);
		viewport.setView(selection, 1);
		document.body.insertAdjacentHTML('beforeend', '<select id="select-sdr-decimate"><option value="16">16</option></select>');
		vi.mocked(send).mockClear();
		for (const [index, delta] of [-80, 80].entries()) {
			const nextCenter = index === 0 ? center + 40_000 : center;
			axis.axisDragStart('x', 400, 424, rect);
			axis.axisDragMove(400 + delta, 424);
			const preview = axis.previewFreqWindow();
			axis.axisDragEnd();
			const request = vi.mocked(send).mock.calls.at(-1)![0] as { center: number; request_id: string };
			expect(request.center).toBe(nextCenter);
			if (order === 'after') draw(nextCenter);
			if (order === 'status-first') controls.syncSdrPanel(report(nextCenter, center, 3 + index));
			controls.syncSdrPanel(report(nextCenter, center, 3 + index, 'SET_SDR', request.request_id));
			if (order !== 'after') draw(nextCenter);
			expect(axis.previewFreqWindow()).toBeNull();
			expect(viewport.getView()).toEqual(preview);
			expect(state.sdrListenHz.get()).toBe(center);
		}
		expect(viewport.getView()).toEqual(selection);
	});

	it.each([-1, 1])('slides only enough to reveal a partly hidden passband on side %s', async direction => {
		const { controls, send, viewport } = await setupZoom();
		viewport.setView({ lo: CENTER - 200_000, hi: CENTER + 200_000 }, 1);
		const listen = CENTER + direction * 199_000;
		document.body.insertAdjacentHTML('beforeend', `<input id="input-sdr-listen" value="${listen / 1e6}">`);
		controls.applySdrTune();
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncSdrPanel(report(CENTER, listen, 2, 'SET_SDR_CHANNEL', request.request_id));
		expect(viewport.getView()).toEqual({ lo: CENTER - 200_000 + direction * 5_000,
			hi: CENTER + 200_000 + direction * 5_000 });
	});

	it('keeps a fast zoomed pan reversal through both delayed capture confirmations', async () => {
		const { controls, state, send, viewport } = await setupZoom();
		const store = await import('../core/store');
		const axis = await import('../ui/axisDrag');
		const draw = (center: number) => {
			const capture = captureAt(center);
			viewport.setCapture(capture);
			store.setRtaData({ freq: new Float64Array([capture.lo, capture.hi]), startHz: capture.lo, stopHz: capture.hi });
		};
		draw(CENTER);
		document.body.insertAdjacentHTML('beforeend', '<select id="select-sdr-decimate"><option value="16">16</option></select>');
		for (const delta of [-80, 80]) {
			axis.axisDragStart('x', 400, 424, { x: 10, y: 14, w: 800, h: 400 });
			axis.axisDragMove(400 + delta, 424);
			axis.axisDragEnd();
		}
		expect(send).toHaveBeenCalledTimes(1);
		expect(axis.previewFreqWindow()).toEqual(SELECTION);
		const first = vi.mocked(send).mock.calls[0][0] as { center: number; request_id: string };
		controls.syncSdrPanel(report(first.center, CENTER, 2, 'SET_SDR', first.request_id));
		draw(first.center);
		expect(axis.previewFreqWindow()).toEqual(SELECTION);
		const last = vi.mocked(send).mock.calls[1][0] as { center: number; request_id: string };
		expect(last.center).toBe(CENTER);
		controls.syncSdrPanel(report(CENTER, CENTER, 3, 'SET_SDR', last.request_id));
		draw(CENTER);
		expect(axis.previewFreqWindow()).toBeNull();
		expect(viewport.getView()).toEqual(SELECTION);
		expect(state.sdrListenHz.get()).toBe(CENTER);
	});

	it('lets a new axis drag cancel receiver focus waiting for a capture frame', async () => {
		const { controls, viewport } = await setupZoom();
		const store = await import('../core/store');
		const axis = await import('../ui/axisDrag');
		const capture = captureAt(CENTER);
		store.setRtaData({ freq: new Float64Array([capture.lo, capture.hi]), startHz: capture.lo, stopHz: capture.hi });
		const nextCenter = CENTER + 5e6;
		controls.syncSdrPanel(report(nextCenter, nextCenter, 2, 'SET_SDR_CHANNEL'));
		axis.axisDragStart('x', 400, 424, { x: 10, y: 14, w: 800, h: 400 });
		viewport.setCapture(captureAt(nextCenter));
		expect(viewport.getView()).toEqual({ lo: SELECTION.lo + 5e6, hi: SELECTION.hi + 5e6 });
	});

	it('lets a new tune cancel receiver focus waiting for an older capture frame', async () => {
		const { controls, send, viewport } = await setupZoom();
		const first = CENTER + 5e6, latest = CENTER + 10e6;
		controls.syncSdrPanel(report(first, first, 2, 'SET_SDR_CHANNEL'));
		controls.listenAtFreq(latest);
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		viewport.setCapture(captureAt(first));
		expect(viewport.getView()).toEqual({ lo: SELECTION.lo + 5e6, hi: SELECTION.hi + 5e6 });
		controls.syncSdrPanel(report(latest, latest, 3, 'SET_SDR_CHANNEL', request.request_id));
		viewport.setCapture(captureAt(latest));
		expect(viewport.getView()).toEqual({ lo: latest - 200_000, hi: latest + 200_000 });
	});

	it('keeps a zoomed pan in place when the backend fits its listen frequency at a capture edge', async () => {
		const { controls, state, send, viewport } = await setupZoom();
		const store = await import('../core/store');
		const axis = await import('../ui/axisDrag');
		const capture = captureAt(CENTER);
		store.setRtaData({ freq: new Float64Array([capture.lo, capture.hi]), startHz: capture.lo, stopHz: capture.hi });
		document.body.insertAdjacentHTML('beforeend', '<select id="select-sdr-decimate"><option value="16">16</option></select>');
		axis.axisDragStart('x', 400, 424, { x: 10, y: 14, w: 800, h: 400 });
		axis.axisDragMove(400 - 8_000, 424); // Pan beyond the old receiver's capture coverage.
		const preview = axis.previewFreqWindow();
		axis.axisDragEnd();
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { center: number; request_id: string };
		const listen = request.center - SPAN / 2 + 6_000;
		controls.syncSdrPanel(report(request.center, listen, 2, 'SET_SDR', request.request_id));
		viewport.setCapture(captureAt(request.center));
		expect(viewport.getView()).toEqual(preview);
		expect(state.sdrListenHz.get()).toBe(listen);
	});

	it.each(['center', 'listen'])('focuses 125 MHz with the %s input rule after moving the overview', async field => {
		const { controls, send, viewport } = await setupZoom();
		const center = 125e6;
		controls.syncSdrPanel(report(center, center, 2));
		viewport.setCapture(captureAt(center));
		viewport.setView({ lo: center - 1.2e6, hi: center - 0.8e6 }, 1);
		document.body.insertAdjacentHTML('beforeend', '<input id="input-sdr-center" value="125">' +
			'<input id="input-sdr-listen" value="125"><select id="select-sdr-decimate"><option value="16">16</option></select>');
		controls.bindActions();
		vi.mocked(send).mockClear();
		const { renderRequestCount } = await import('../render/redraw');
		const renders = renderRequestCount();
		const input = document.getElementById(`input-sdr-${field}`) as HTMLInputElement;
		input.focus();
		input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
		expect(viewport.getView()).toEqual(field === 'center'
			? { lo: center - 200_000, hi: center + 200_000 }
			: { lo: center - 394_000, hi: center + 6_000 });
		expect(renderRequestCount()).toBeGreaterThan(renders);
		if (field === 'listen') expect(send).not.toHaveBeenCalled();
		// A delayed echo must not repeat the focus after a subsequent overview drag.
		const moved = { lo: center + 0.8e6, hi: center + 1.2e6 };
		viewport.setView(moved, 1);
		controls.syncSdrPanel(report(center, center, 3, field === 'center' ? 'SET_SDR' : 'STATUS'));
		expect(viewport.getView()).toEqual(moved);
		// Further Enter presses remain explicit focus actions, including before an ACK.
		for (let i = 0; i < 2; i++) {
			input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
			expect(viewport.getView()).toEqual(field === 'center'
				? { lo: center - 200_000, hi: center + 200_000 }
				: { lo: center - 6_000, hi: center + 394_000 });
			viewport.setView(moved, 1);
		}
	});

	it.each(['lo', 'hi'] as const)('refocuses a resubmitted listen frequency at the %s capture edge', async edge => {
		const { controls, send, viewport } = await setupZoom();
		const capture = captureAt(CENTER);
		controls.syncSdrPanel(report(CENTER, capture[edge], 2));
		viewport.setView(SELECTION, 1);
		document.body.insertAdjacentHTML('beforeend', `<input id="input-sdr-listen" value="${capture[edge] / 1e6}">`);
		controls.applySdrTune();
		expect(viewport.getView()).toEqual(edge === 'lo'
			? { lo: capture.lo, hi: capture.lo + 400_000 }
			: { lo: capture.hi - 400_000, hi: capture.hi });
		expect(send).not.toHaveBeenCalled();
	});

	it('defers a repeated-frequency focus until the acknowledged capture arrives', async () => {
		const { controls, viewport } = await setupZoom();
		const center = CENTER + SPAN / 4;
		controls.syncSdrPanel(report(center, center, 2));
		viewport.setView(SELECTION, 1); // Cancels the first automatic focus.
		controls.syncSdrPanel(report(CENTER, CENTER, 1)); // Stale bounds cannot replace the acknowledged capture.
		document.body.insertAdjacentHTML('beforeend', `<input id="input-sdr-listen" value="${center / 1e6}">`);
		controls.applySdrTune();
		expect(viewport.getView()).toEqual(SELECTION);
		viewport.setCapture(captureAt(center));
		expect(viewport.getView()).toEqual({ lo: center - 394_000, hi: center + 6_000 });
	});

	it.each(['center', 'listen'])('ignores an empty or invalid %s submission', async field => {
		const { controls, send, viewport } = await setupZoom();
		document.body.insertAdjacentHTML('beforeend', `<input id="input-sdr-${field}">`);
		const input = document.getElementById(`input-sdr-${field}`) as HTMLInputElement;
		for (const value of ['', 'invalid', 'Infinity', '0', '-1']) {
			input.value = value;
			if (field === 'center') controls.applySdr();
			else controls.applySdrTune();
			expect(viewport.getView()).toEqual(SELECTION);
		}
		expect(send).not.toHaveBeenCalled();
	});

	it('keeps a queued return to the confirmed frequency pending until its tune settles', async () => {
		const { controls, send, viewport } = await setupZoom();
		const listen = CENTER + 400_000;
		controls.listenAtFreq(listen);
		const first = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		document.body.insertAdjacentHTML('beforeend', `<input id="input-sdr-listen" value="${CENTER / 1e6}">`);
		controls.applySdrTune();
		expect(viewport.getView()).toEqual(SELECTION);
		expect(send).toHaveBeenCalledTimes(1);
		controls.syncSdrPanel(report(CENTER, listen, 2, 'SET_SDR_CHANNEL', first.request_id));
		const last = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncSdrPanel(report(CENTER, CENTER, 3, 'SET_SDR_CHANNEL', last.request_id));
		expect(viewport.getView()).toEqual({ lo: CENTER - 394_000, hi: CENTER + 6_000 });
	});

	it('does not scroll to a superseded listen target during a fast input reversal', async () => {
		const { controls, send, viewport } = await setupZoom();
		const selection = { lo: CENTER - 200_000, hi: CENTER + 200_000 };
		viewport.setView(selection, 1);
		document.body.insertAdjacentHTML('beforeend', '<input id="input-sdr-listen">');
		const input = document.getElementById('input-sdr-listen') as HTMLInputElement;
		input.value = String((CENTER + 400_000) / 1e6);
		controls.applySdrTune();
		const first = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		input.value = String(CENTER / 1e6);
		controls.applySdrTune();
		controls.syncSdrPanel(report(CENTER, CENTER + 400_000, 2, 'SET_SDR_CHANNEL', first.request_id));
		expect(viewport.getView()).toEqual(selection);
		const last = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncSdrPanel(report(CENTER, CENTER, 3, 'SET_SDR_CHANNEL', last.request_id));
		expect(viewport.getView()).toEqual(selection);
	});

	it.each(['SET_SDR_CHANNEL', 'STATUS'])('reveals an off-screen passband confirmed by %s with the smallest pan', async responseTo => {
		const { controls, send, viewport } = await setupZoom();
		const listen = CENTER + 400_000;
		controls.listenAtFreq(listen);
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		const { renderRequestCount } = await import('../render/redraw');
		const renders = renderRequestCount();
		// Unconfirmed input must not strand the view at a rejected frequency.
		expect(viewport.getView()).toEqual(SELECTION);
		controls.syncSdrPanel(report(CENTER, listen, 2, responseTo, request.request_id));
		expect(viewport.getView()).toEqual({ lo: listen - 394_000, hi: listen + 6_000 });
		expect(renderRequestCount()).toBeGreaterThan(renders);
		expect(send).toHaveBeenCalledTimes(1);
	});

	it.each(['SET_SDR', 'SET_SDR_CHANNEL', 'STATUS'].flatMap(responseTo =>
		['before', 'after'].map(order => ({ responseTo, order }))))(
		'follows a center change with $responseTo arriving $order the frame', async ({ responseTo, order }) => {
		const { controls, viewport } = await setupZoom();
		const center = CENTER + 5e6;
		if (order === 'after') viewport.setCapture(captureAt(center));
		controls.syncSdrPanel(report(center, center, 2, responseTo));
		if (order === 'before') {
			expect(viewport.getView()).toEqual(SELECTION);
			viewport.setCapture(captureAt(center));
		}
		expect(viewport.getView()).toEqual({ lo: center - 200_000, hi: center + 200_000 });
	});

	it('waits for the new capture when a center report omits actual bounds', async () => {
		const { controls, viewport } = await setupZoom();
		const center = CENTER + 5e6;
		const snapshot = report(center, center, 2, 'SET_SDR');
		controls.syncSdrPanel({ ...snapshot, sdr: { ...snapshot.sdr, actual: undefined } });
		expect(viewport.getView()).toEqual(SELECTION);
		viewport.setCapture(captureAt(center));
		expect(viewport.getView()).toEqual({ lo: center - 200_000, hi: center + 200_000 });
	});

	it.each(['lo', 'hi'] as const)('clamps the view at the %s capture edge without shrinking', async edge => {
		const { controls, viewport } = await setupZoom();
		const capture = captureAt(CENTER);
		controls.syncSdrPanel(report(CENTER, capture[edge], 2, 'SET_SDR_CHANNEL'));
		expect(viewport.getView()).toEqual(edge === 'lo'
			? { lo: capture.lo, hi: capture.lo + 400_000 }
			: { lo: capture.hi - 400_000, hi: capture.hi });
	});

	it('keeps the latest receiver target through intermediate capture frames', async () => {
		const { controls, viewport } = await setupZoom();
		const first = CENTER + 5e6, latest = CENTER + 10e6;
		controls.syncSdrPanel(report(first, first, 2, 'SET_SDR_CHANNEL'));
		controls.syncSdrPanel(report(latest, latest + 400_000, 3, 'SET_SDR_CHANNEL'));
		viewport.setCapture(captureAt(first));
		viewport.setCapture(captureAt(latest));
		expect(viewport.getView()).toEqual({ lo: latest + 200_000, hi: latest + 600_000 });
	});

	it('ignores stale reports and repeated snapshots after the user moves the overview box', async () => {
		const { controls, viewport } = await setupZoom();
		const listen = CENTER + 400_000;
		controls.syncSdrPanel(report(CENTER, listen, 3, 'SET_SDR_CHANNEL'));
		viewport.setView(SELECTION, 1);
		controls.syncSdrPanel(report(CENTER, CENTER, 2, 'SET_SDR_CHANNEL'));
		controls.syncSdrPanel(report(CENTER, listen, 3));
		expect(viewport.getView()).toEqual(SELECTION);
	});

	it('leaves the full capture visible when tuning without a selection', async () => {
		const { controls, viewport } = await setupZoom();
		viewport.setView(null);
		controls.syncSdrPanel(report(CENTER, CENTER + 400_000, 2, 'SET_SDR_CHANNEL'));
		expect(viewport.isZoomed()).toBe(false);
		expect(viewport.getView()).toEqual(captureAt(CENTER));
	});

	it('keeps the selection on rejected input, invalid reports and sub-Hz jitter', async () => {
		const { controls, send, viewport } = await setupZoom();
		const { rejectSdrTuneRequest } = await import('../ui/sdrTuneRequest');
		controls.listenAtFreq(CENTER + 400_000);
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		rejectSdrTuneRequest('SET_SDR_CHANNEL', request.request_id);
		controls.syncSdrPanel(report(CENTER, NaN, 2, 'SET_SDR_CHANNEL'));
		controls.syncSdrPanel(report(CENTER, CENTER + 0.1, 3));
		expect(viewport.getView()).toEqual(SELECTION);
	});

	it('returns to the full capture when a bandwidth change supersedes deferred tuning', async () => {
		const { controls, viewport } = await setupZoom();
		const center = CENTER + 5e6;
		controls.syncSdrPanel(report(center, center, 2, 'SET_SDR_CHANNEL'));
		const narrower = { lo: center - SPAN / 4, hi: center + SPAN / 4 };
		viewport.setCapture(narrower);
		viewport.setCapture(captureAt(center));
		expect(viewport.isZoomed()).toBe(false);
		expect(viewport.getView()).toEqual(captureAt(center));
	});
});

describe('SDR control intent and STATUS', () => {
	it('cycles only available modes and refuses selection outside receiver control', async () => {
		const { controls, state, send } = await load();
		// These instances must follow resetModules so they share the controls state.
		const { setPluginManifest } = await import('../sdr/registry');
		const { graphMode, deviceMode } = await import('../ui/graphMode');
		setPluginManifest([receiverPlugin('am'), receiverPlugin('ism', { available: false }), receiverPlugin('cw')]);
		graphMode.confirm('sdr');
		controls.bindCanvas();
		key('M');
		expect(state.sdrDemod.get()).toBe('cw');
		vi.mocked(send).mockClear();
		controls.selectSdrDemod('ism');
		expect(state.sdrDemod.get()).toBe('cw');
		deviceMode.confirm('server');
		controls.selectSdrDemod('am');
		expect(state.sdrDemod.get()).toBe('cw');
		expect(send).not.toHaveBeenCalled();
	});
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
		const { graphMode, deviceMode } = await import('../ui/graphMode');
		const { setPluginManifest } = await import('../sdr/registry');
		setPluginManifest(['am', mode].map((id) => receiverPlugin(id)));
		deviceMode.confirm('sdr');
		graphMode.confirm('sdr');
		state.sdrIfbw.confirm(mode === 'wfm' ? 6000 : 180000);
		controls.bindCanvas();
		if (via === 'keyboard') key('M');
		else controls.selectSdrDemod(mode);
		const expected = mode === 'wfm' ? 180000 : mode === 'cw' ? 500 : 3000;
		expect(state.sdrDemod.get()).toBe(mode);
		expect(state.sdrIfbw.get()).toBe(expected);
		expect(send).toHaveBeenCalledWith(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', mode, ifbw: expected,
			request_id: expect.stringMatching(/^sdr-filter-/) }));
	});

	it('restores each mode’s last confirmed filter instead of a global manual value', async () => {
		const { controls, state, send } = await load();
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 6000 }));
		const ack = (mode: string, ifbw: number) => {
			const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
			controls.syncSdrPanel({ ...status('sdr', mode, { if_bw: ifbw }),
				response_to: 'SET_SDR_CHANNEL', request_id: request.request_id });
		};
		controls.selectSdrDemod('cw');
		expect(state.sdrIfbw.get()).toBe(500);
		ack('cw', 500);
		controls.selectSdrIfbw(900);
		ack('cw', 900);
		controls.selectSdrDemod('am');
		expect(state.sdrDemod.get()).toBe('am');
		expect(state.sdrIfbw.get()).toBe(6000);
		ack('am', 6000);
		controls.selectSdrDemod('cw');
		expect(state.sdrDemod.get()).toBe('cw');
		expect(state.sdrIfbw.get()).toBe(900);
	});

	it('keeps only the newest quick filter queued and settles requests by their own ACK id', async () => {
		const { controls, state, send } = await load();
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 6000 }));
		controls.selectSdrIfbw(3000);
		const first = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.selectSdrIfbw(4500);
		controls.selectSdrIfbw(12000);
		expect(send).toHaveBeenCalledTimes(1);
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 3000 }),
			response_to: 'SET_SDR_CHANNEL', request_id: first.request_id });
		expect(send).toHaveBeenCalledTimes(2);
		const second = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string; mode: string; ifbw: number };
		expect(second).toEqual(expect.objectContaining({ mode: 'am', ifbw: 12000 }));
		expect(second.request_id).not.toBe(first.request_id);
		expect(state.sdrIfbw.get()).toBe(12000);

		// A non-ACK report updates the confirmed baseline, but cannot replace the newest intent.
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 4500 }));
		expect(state.sdrIfbw.confirmedValue()).toBe(4500);
		expect(state.sdrIfbw.get()).toBe(12000);
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 3000 }),
			response_to: 'SET_SDR_CHANNEL', request_id: first.request_id });
		controls.rejectSdrFilterRequest('SET_SDR_CHANNEL', first.request_id);
		controls.rejectSdrFilterRequest('SET_PRESET', second.request_id);
		expect(state.sdrIfbw.confirmedValue()).toBe(4500);
		expect(state.sdrIfbw.pending()).toBe(true);
		expect(send).toHaveBeenCalledTimes(2);
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 12000 }),
			response_to: 'SET_SDR_CHANNEL', request_id: second.request_id });
		expect(state.sdrIfbw.confirmedValue()).toBe(12000);
		expect(state.sdrIfbw.pending()).toBe(false);
	});

	it.each([false, true])('settles a stale filter ACK without overwriting newer state (queued: %s)', async (queued) => {
		const { controls, state, send } = await load();
		// resetModules above gives each test fresh request ownership and store state.
		const store = await import('../core/store');
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 6000 }), config_version: 5 });
		controls.selectSdrIfbw(3000);
		const first = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		if (queued) controls.selectSdrIfbw(12000);
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 3000 }), config_version: 3,
			response_to: 'SET_SDR_CHANNEL', request_id: first.request_id });
		expect(state.sdrIfbw.confirmedValue()).toBe(6000);
		if (queued) {
			expect(send).toHaveBeenCalledTimes(2);
			const second = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
			controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 12000 }), config_version: 6,
				response_to: 'SET_SDR_CHANNEL', request_id: second.request_id });
		}
		vi.advanceTimersByTime(3000);
		expect(state.sdrIfbw.get()).toBe(queued ? 12000 : 6000);
		expect(state.sdrIfbw.pending()).toBe(false);
		expect(store.statusError).toBeNull();
	});

	it('admits newer state carrying a completed filter identity without settling the live edit', async () => {
		const { controls, state, send } = await load();
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 6000 }), config_version: 1 });
		controls.selectSdrIfbw(3000);
		const first = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 3000 }), config_version: 2,
			response_to: 'SET_SDR_CHANNEL', request_id: first.request_id });
		controls.selectSdrIfbw(12000);
		const second = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 4500 }), config_version: 3,
			response_to: 'STATUS', request_id: first.request_id });
		expect(state.sdrIfbw.confirmedValue()).toBe(4500);
		expect(state.sdrIfbw.get()).toBe(12000);
		expect(state.sdrIfbw.pending()).toBe(true);
		expect(send).toHaveBeenCalledTimes(2);
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 12000 }), config_version: 4,
			response_to: 'SET_SDR_CHANNEL', request_id: second.request_id });
		expect(state.sdrIfbw.pending()).toBe(false);
	});

	it('refreshes queued full receiver edits with the newest AM detector confirmation', async () => {
		const { controls, state, send } = await load();
		const { initDecodeWindow } = await import('../ui/decodeWindow');
		controls.syncGraphModeStatus('sdr');
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 6000, volume: 0.8, am_sync: false }));
		controls.bindActions();
		const hooks = vi.mocked(initDecodeWindow).mock.calls.at(-1)![0];
		vi.mocked(send).mockClear();
		controls.selectSdrIfbw(3000);
		const first = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		state.sdrVolume.set(1.2);
		controls.applySdrDemod();
		expect(send).toHaveBeenCalledTimes(1);
		hooks.onAmSync!(true);
		expect(send).toHaveBeenCalledTimes(2);
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 6000, volume: 0.8, am_sync: true }));
		expect(state.sdrAmSync.confirmedValue()).toBe(true);
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 3000, volume: 0.8, am_sync: true }),
			response_to: 'SET_SDR_CHANNEL', request_id: first.request_id });
		expect(send).toHaveBeenCalledTimes(3);
		const queued = vi.mocked(send).mock.calls.at(-1)![0] as {
			request_id: string; ifbw: number; volume: number; am_sync: boolean;
		};
		expect(queued.request_id).toMatch(/^sdr-filter-/);
		expect(queued.request_id).not.toBe(first.request_id);
		expect(queued).toEqual(expect.objectContaining({ ifbw: 3000, volume: 1.2, am_sync: true }));
		expect(state.sdrAmSync.get()).toBe(true);
		expect(state.sdrVolume.get()).toBe(1.2);
	});

	it('does not remember an unconfirmed target or let its late ACK change a newer mode request', async () => {
		const { controls, state, send } = await load();
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 6000 }));
		controls.selectSdrDemod('cw');
		const initialCw = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncSdrPanel({ ...status('sdr', 'cw', { if_bw: 500 }),
			response_to: 'SET_SDR_CHANNEL', request_id: initialCw.request_id });
		controls.selectSdrIfbw(900);
		const unconfirmedCw = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.selectSdrDemod('am');
		controls.rejectSdrFilterRequest('SET_SDR_CHANNEL', unconfirmedCw.request_id);
		const amRequest = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string; mode: string; ifbw: number };
		expect(amRequest).toEqual(expect.objectContaining({ mode: 'am', ifbw: 6000 }));
		controls.syncSdrPanel({ ...status('sdr', 'cw', { if_bw: 900 }),
			response_to: 'SET_SDR_CHANNEL', request_id: unconfirmedCw.request_id });
		expect(state.sdrDemod.get()).toBe('am');
		expect(state.sdrIfbw.get()).toBe(6000);
		expect(state.sdrDemod.pending()).toBe(true);
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 6000 }),
			response_to: 'SET_SDR_CHANNEL', request_id: amRequest.request_id });
		controls.selectSdrDemod('cw');
		expect(state.sdrIfbw.get()).toBe(500);
	});

	it('restores the confirmed filter after timeout and ignores the timed-out ACK', async () => {
		const { controls, state, send } = await load();
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 6000 }));
		controls.selectSdrIfbw(3000);
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		vi.advanceTimersByTime(3000);
		expect(state.sdrIfbw.get()).toBe(6000);
		expect(state.sdrIfbw.desiredValue()).toBeNull();
		expect(send).toHaveBeenCalledTimes(1);
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 3000 }),
			response_to: 'SET_SDR_CHANNEL', request_id: request.request_id });
		expect(state.sdrIfbw.confirmedValue()).toBe(6000);
		expect(state.sdrIfbw.get()).toBe(6000);
	});
	it('keeps external receiver mode and filter pending beyond three seconds until its matching ACK', async () => {
		const { controls, state, send } = await load();
		// Resolve the fresh registry instance after this test's resetModules boundary.
		const { pluginManifest, setPluginManifest } = await import('../sdr/registry');
		setPluginManifest([...(pluginManifest() ?? []), receiverPlugin('gps', {
			kind: 'digital', provider: 'gnss-sdr', startupTimeoutMs: 25000,
			defaultBandwidthHz: 2_000_000, minBandwidthHz: 2_000_000, maxBandwidthHz: 2_000_000,
			bandwidthPresetsHz: [2_000_000], bandwidthEditable: false, audioCompanion: null, outputs: ['table'],
		})]);
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 6000 }));
		controls.selectSdrDemod('gps');
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string; ifbw: number };
		vi.advanceTimersByTime(20000);
		expect(state.sdrDemod.pending()).toBe(true);
		expect(state.sdrDemod.get()).toBe('gps');
		expect(state.sdrIfbw.pending()).toBe(true);
		expect(state.sdrIfbw.get()).toBe(request.ifbw);
		controls.syncSdrPanel({ ...status('sdr', 'gps', { if_bw: request.ifbw }),
			response_to: 'SET_SDR_CHANNEL', request_id: request.request_id });
		expect(state.sdrDemod.pending()).toBe(false);
		expect(state.sdrDemod.confirmedValue()).toBe('gps');
		vi.advanceTimersByTime(6000);
		expect(state.sdrDemod.get()).toBe('gps');
		expect(state.sdrIfbw.get()).toBe(request.ifbw);
	});
	it('uses the selected receiver startup deadline when entering SDR', async () => {
		const { controls, state } = await load();
		// Static imports would retain registry/graph state from before resetModules.
		const { pluginManifest, setPluginManifest } = await import('../sdr/registry');
		setPluginManifest([...(pluginManifest() ?? []), receiverPlugin('gps', { kind: 'digital', provider: 'gnss-sdr', startupTimeoutMs: 25000 })]);
		const graph = await import('../ui/graphMode');
		graph.deviceMode.confirm('std');
		graph.graphMode.confirm('std');
		state.sdrDemod.confirm('gps');
		controls.setGraphMode('sdr');
		vi.advanceTimersByTime(20000);
		expect(graph.pendingGraphMode()).toBe('sdr');
		graph.confirmGraphMode('sdr');
		vi.advanceTimersByTime(6000);
		expect(graph.currentGraphMode()).toBe('sdr');
		expect(graph.pendingGraphMode()).toBeNull();
	});

	it('learns mode memory from external STATUS rather than only local edits', async () => {
		const { controls, state } = await load();
		controls.syncSdrPanel(status('sdr', 'cw', { if_bw: 900 }));
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 4500 }));
		controls.selectSdrDemod('cw');
		expect(state.sdrIfbw.get()).toBe(900);
	});

	it('retries rejected saved mode memory at its recommendation, then restores the confirmed mode', async () => {
		localStorage.setItem('web-sa-sdr-filter-by-mode', '{"cw":900000}');
		const { controls, state, send } = await load();
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 6000 }));
		controls.selectSdrDemod('cw');
		const restored = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string; ifbw: number };
		expect(restored.ifbw).toBe(900000);
		controls.rejectSdrFilterRequest('SET_SDR_CHANNEL', restored.request_id);
		const fallback = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string; mode: string; ifbw: number };
		expect(fallback).toEqual(expect.objectContaining({ mode: 'cw', ifbw: 500 }));
		expect(fallback.request_id).not.toBe(restored.request_id);
		controls.rejectSdrFilterRequest('SET_SDR_CHANNEL', fallback.request_id);
		expect(state.sdrDemod.get()).toBe('am');
		expect(state.sdrDemod.desiredValue()).toBeNull();
		expect(state.sdrIfbw.get()).toBe(6000);
		expect(state.sdrIfbw.desiredValue()).toBeNull();
		expect(send).toHaveBeenCalledTimes(2);
	});

	it('Preset clears filter preferences and invalidates an outstanding filter ACK', async () => {
		const { controls, state, send } = await load();
		controls.syncSdrPanel(status('sdr', 'am', { if_bw: 6000 }));
		controls.selectSdrIfbw(3000);
		const filter = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.presetAll();
		const preset = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncPresetStatus('SET_PRESET', preset.request_id);
		expect(localStorage.getItem('web-sa-sdr-ifbw')).toBeNull();
		controls.syncSdrPanel({ ...status('sdr', 'am', { if_bw: 3000 }),
			response_to: 'SET_SDR_CHANNEL', request_id: filter.request_id });
		expect(state.sdrIfbw.confirmedValue()).toBe(6000);
		expect(state.sdrIfbw.desiredValue()).toBeNull();
		controls.selectSdrDemod('cw');
		expect(state.sdrIfbw.get()).toBe(500);
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
		expect(send).toHaveBeenCalledWith(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 9_810_100 }));
		const space = new KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true });
		button.dispatchEvent(space); expect(space.defaultPrevented).toBe(false);
		(document.getElementById('input-sdr-listen') as HTMLInputElement).focus();
		const arrow = new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true });
		document.activeElement!.dispatchEvent(arrow); expect(arrow.defaultPrevented).toBe(false);
		expect(state.sdrListenHz.get()).toBe(9_810_000);
	});

	it.each(['minimum', 'maximum'] as const)('stops keyboard tuning at the RF %s inside a crossing IQ window', async (edge) => {
		const { controls, state, send } = await load();
		const store = await import('../core/store');
		const { graphMode } = await import('../ui/graphMode');
		graphMode.confirm('sdr');
		const boundary = edge === 'minimum' ? store.FREQ_MIN : store.FREQ_MAX;
		const direction = edge === 'minimum' ? 1 : -1;
		state.sdrCenterHz.confirm(boundary);
		state.sdrSpanHz.confirm(390_625);
		state.sdrListenHz.confirm(boundary + direction);
		controls.bindCanvas();
		key(edge === 'minimum' ? 'ArrowLeft' : 'ArrowRight');
		expect(state.sdrListenHz.get()).toBe(boundary);
		expect(send).toHaveBeenCalledWith(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', freq: boundary }));
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

	it('keeps AGC independent of squelch and settles only its latest command reply', async () => {
		document.body.insertAdjacentHTML('beforeend', '<button id="btn-sdr-agc" data-action="toggle-sdr-agc"></button>');
		const { controls, state, send } = await load();
		controls.bindActions();
		controls.syncSdrPanel(status('sdr', 'am', { agc: true, squelch: -112.3 }));
		const button = document.getElementById('btn-sdr-agc') as HTMLButtonElement;
		button.click();
		const first = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		expect(first).toEqual({ cmd: 'SET_SDR_CHANNEL', channel: 1, agc: false, request_id: expect.stringMatching(/^sdr-agc-/) });
		button.click();
		const second = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		controls.syncSdrPanel({ ...status('sdr', 'am', { agc: false, squelch: -112.3 }),
			response_to: 'SET_SDR_CHANNEL', request_id: first.request_id });
		controls.rejectSdrAgcRequest('SET_SDR_CHANNEL', first.request_id);
		expect(state.sdrAgc.get()).toBe(true);
		expect(state.sdrAgc.pending()).toBe(true);
		expect(button.getAttribute('aria-pressed')).toBe('true');
		controls.syncSdrPanel({ ...status('sdr', 'am', { agc: true, squelch: -112.3 }),
			response_to: 'SET_SDR_CHANNEL', request_id: second.request_id });
		expect(state.sdrAgc.pending()).toBe(false);
		expect(state.sdrSquelch.get()).toBe(-112.3);
		expect(localStorage.getItem('web-sa-sdr-agc')).toBe('1');
	});

	it.each(['error', 'timeout', 'disconnect', 'leave'])(
		'clears unconfirmed AGC intent after %s and ignores its late reply', async (ending) => {
		document.body.insertAdjacentHTML('beforeend', '<button id="btn-sdr-agc" data-action="toggle-sdr-agc"></button>');
		const { controls, state, send } = await load();
		controls.bindActions();
		controls.syncSdrPanel(status('sdr', 'am', { agc: true, squelch: -97.2 }));
		const button = document.getElementById('btn-sdr-agc') as HTMLButtonElement;
		button.click();
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		expect(state.sdrAgc.get()).toBe(false);
		if (ending === 'error') controls.rejectSdrAgcRequest('SET_SDR_CHANNEL', request.request_id);
		else if (ending === 'timeout') vi.advanceTimersByTime(3000);
		else if (ending === 'disconnect') controls.disconnectSdrSession();
		else controls.syncSdrPanel(status('std'));
		expect(state.sdrAgc.desiredValue()).toBeNull();
		expect(state.sdrAgc.get()).toBe(true);
		expect(button.getAttribute('aria-pressed')).toBe('true');
		controls.syncSdrPanel({ ...status('sdr', 'am', { agc: false }),
			response_to: 'SET_SDR_CHANNEL', request_id: request.request_id });
		expect(state.sdrAgc.get()).toBe(true);
		expect(state.sdrSquelch.get()).toBe(-97.2);
		controls.syncSdrPanel(status('sdr', 'am', { agc: false }));
		expect(state.sdrAgc.get()).toBe(false);
		expect(button.getAttribute('aria-pressed')).toBe('false');
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

	it.each(['std', 'rta'])('keeps a held Y gesture across recurring %s STATUS', async (mode) => {
		const { controls, send } = await load();
		const { graphMode } = await import('../ui/graphMode');
		const { axisDragStart, axisDragMove, axisDragEnd, axisDragging, previewLevelWindow, resetAxisPreview } = await import('../ui/axisDrag');
		const { setDisplayRef } = await import('../ui/displayRef');
		graphMode.confirm(mode as 'std' | 'rta');
		setDisplayRef('preset', -30);
		const rect = { x: 10, y: 14, w: 800, h: 400 };
		try {
			axisDragStart('y', 830, 200, rect);
			axisDragMove(830, 240);
			controls.syncSdrPanel(status(mode));
			controls.syncSdrPanel(status(mode));
			expect(axisDragging()).toBe(true);
			expect(previewLevelWindow()?.ref).toBe(-20);
			axisDragMove(830, 160);
			axisDragEnd();
			expect(send).toHaveBeenCalledWith({ cmd: 'SET_REF', mode: 'manual', ref: -40 });
		} finally {
			resetAxisPreview();
		}
	});
});
