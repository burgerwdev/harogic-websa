import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../core/wsSend', () => ({ send: vi.fn(), onCommandSent: vi.fn() }));
vi.mock('../audio/sdrAudio', () => ({
	prepareSdrAudioTransition: vi.fn(), setSdrAudioEnabled: vi.fn(), setSdrAudioMode: vi.fn(),
}));
vi.mock('../sdr/iqStream', () => ({ resetSdrIq: vi.fn(), setSdrIqEnabled: vi.fn(), setSstvModePin: vi.fn() }));
vi.mock('../ui/decodeWindow', () => ({
	canToggleAmSync: () => true, confirmAmSyncGeneration: vi.fn(), setAmInfo: vi.fn(),
	decodeChannel: () => 1, initDecodeWindow: vi.fn(), refreshDecodeWindow: vi.fn(),
	setDecodeActive: vi.fn(), setDecodeChannelMode: vi.fn(), toggleDecodeWindow: vi.fn(),
}));
vi.mock('../ui/refAutoScale', () => ({
	clearSdrEntryState: vi.fn(), postRefNotice: vi.fn(), requestSdrEntryFit: vi.fn(), resetAutoScaleState: vi.fn(),
	autoScaleBusy: () => false, autoScaleRequest: vi.fn(), clearAutoScaleHint: vi.fn(),
}));

const listeners: Array<() => void> = [];
let canvas: HTMLCanvasElement;
let screenScale = 1;

beforeEach(() => {
	vi.resetModules();
	vi.clearAllMocks();
	vi.useFakeTimers();
	localStorage.clear();
	screenScale = 1;
	const fake = document.getElementById('spectrum') as HTMLCanvasElement;
	const get = document.getElementById.bind(document);
	canvas = document.createElement('canvas');
	canvas.id = 'spectrum';
	canvas.width = 860;
	canvas.height = 480;
	canvas.style.border = '0px solid transparent';
	Object.defineProperties(canvas, { clientWidth: { value: 860 }, clientHeight: { value: 480 } });
	document.body.append(canvas);
	vi.spyOn(document, 'getElementById').mockImplementation(id => id === 'spectrum' ? canvas : get(id));
	vi.spyOn(canvas, 'getContext').mockImplementation(() => fake.getContext('2d'));
	vi.spyOn(canvas, 'getBoundingClientRect').mockImplementation(() => ({
		left: 0, top: 0, width: 860 * screenScale, height: 480 * screenScale,
		x: 0, y: 0, right: 860 * screenScale, bottom: 480 * screenScale, toJSON: () => ({}),
	}));
	let captured: number | null = null;
	canvas.setPointerCapture = vi.fn(id => { captured = id; });
	canvas.hasPointerCapture = vi.fn(id => captured === id);
	canvas.releasePointerCapture = vi.fn(id => { if (captured === id) captured = null; });
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
	window.dispatchEvent(new Event('blur'));
	listeners.splice(0).forEach(remove => remove());
	vi.restoreAllMocks();
	vi.clearAllTimers();
	vi.useRealTimers();
	localStorage.clear();
	document.body.innerHTML = '';
});

// Each case reloads module-owned gesture state and listeners; static imports would retain them.
async function load(zoom = false) {
	const controls = await import('../ui/controls');
	const S = await import('../core/store');
	const state = await import('../ui/sdrState');
	const viewport = await import('../ui/spectrumViewport');
	const graph = await import('../ui/graphMode');
	const filter = await import('../ui/sdrFilterGesture');
	const locate = await import('../ui/sdrLocate');
	const plot = await import('../render/plot');
	const axis = await import('../ui/axisDrag');
	const { send } = await import('../core/wsSend');
	const { setPluginManifest } = await import('../sdr/registry');
	const { smoothBins } = await import('../ui/displayState');
	const { refLevel } = await import('../ui/refState');
	S.initStore();
	S.setDeviceConnected(true);
	S.setSdrMode(true);
	S.setRtaMode(false);
	graph.graphMode.confirm('sdr');
	graph.deviceMode.confirm('sdr');
	setPluginManifest([{ id: 'am', kind: 'analog', implemented: true, audioEnhancement: false }]);
	state.sdrDemod.confirm('am');
	state.sdrListenHz.confirm(10_000_000);
	state.sdrCenterHz.confirm(10_000_000);
	state.sdrIfbw.confirm(40_000);
	state.sdrLocateMode.set('signal');
	state.sdrSpanHz.confirm(400_000);
	viewport.__testSeedCapture({ lo: 9_800_000, hi: 10_200_000 });
	viewport.zoomEnabled.set(zoom);
	S.setFreqArray(Float64Array.from({ length: 101 }, (_, i) => 9_800_000 + i * 4000));
	S.setRtaData({ freq: S.freqArray, startHz: 9_800_000, stopHz: 10_200_000 });
	S.traces[0].powers = new Float32Array(101).fill(-50);
	S.traces[0].powers[30] = -30;
	smoothBins.set(1);
	refLevel.confirm(0);
	controls.bindCanvas();
	return { controls, S, state, viewport, filter, locate, plot, axis, send };
}

function pointer(type: string, x: number, y = 240, ctrlKey = false, id = 1) {
	const event = new MouseEvent(type, { clientX: x * screenScale, clientY: y * screenScale,
		button: 0, ctrlKey, bubbles: true, cancelable: true });
	Object.defineProperty(event, 'pointerId', { value: id });
	(type === 'pointerdown' ? canvas : window).dispatchEvent(event);
}

function drag(x0: number, x1: number, y = 240, ctrl = false) {
	pointer('pointerdown', x0, y, ctrl);
	pointer('pointermove', x1, y, ctrl);
	pointer('pointerup', x1, y, ctrl);
}

describe('canvas pointer ownership', () => {
  it.each([0.02, 0.98])('tunes near plot edge %s without moving the capture window', async fraction => {
    const { state, plot, send } = await load();
    const p = plot.plotRect();
    const x = p.x + p.w * fraction;
    pointer('pointerdown', p.x + p.w * 0.5);
    vi.advanceTimersByTime(500);
    pointer('pointermove', x);
    vi.advanceTimersByTime(500);
    pointer('pointermove', x);
    expect(state.sdrCenterHz.get()).toBe(10_000_000);
    expect(send).not.toHaveBeenCalled();
    pointer('pointerup', x);
    expect(send).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 9_800_000 + 400_000 * fraction,
    }));
    expect(state.sdrCenterHz.get()).toBe(10_000_000);
  });

  it.each([false, true])('drags the measured top badge with Zoom %s and releases outside', async zoom => {
    const { state, viewport, send } = await load(zoom);
    const badge = await import('../ui/sdrBadgeGesture');
    const g = badge.sdrBadgeGeometry()!;
    const x = g.left + 2, y = g.top + g.height / 2;
    canvas.dispatchEvent(new MouseEvent('mousemove', { clientX: x, clientY: y }));
    expect(canvas.style.cursor).toBe('grab');
    pointer('pointerdown', x, y);
    expect(canvas.style.cursor).toBe('grabbing');
    expect(badge.sdrBadgeDragFrequency()).toBe(10_000_000);
    pointer('pointermove', x + 80, -40);
    expect(badge.sdrBadgeDragFrequency()).toBe(10_040_000);
    expect(state.sdrListenHz.get()).toBe(10_000_000);
    expect(send).not.toHaveBeenCalled();
    pointer('pointerup', x + 80, -40);
    expect(send).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', freq: 10_040_000 }));
    expect(viewport.isZoomed()).toBe(false);
    expect(badge.sdrBadgeDragFrequency()).toBeNull();
    expect(canvas.releasePointerCapture).toHaveBeenCalledWith(1);
  });

  it.each(['left', 'right'])('keeps the initial frequency offset on an edge-clamped %s badge', async edge => {
    const { state, send } = await load(true);
    const badge = await import('../ui/sdrBadgeGesture');
    const initial = edge === 'left' ? 9_800_000 : 10_200_000;
    state.sdrListenHz.confirm(initial);
    const g = badge.sdrBadgeGeometry()!;
    const x = edge === 'left' ? g.left + g.width - 1 : g.left + 1;
    const delta = edge === 'left' ? 20 : -20;
    pointer('pointerdown', x, 7);
    pointer('pointermove', x, 7);
    expect(badge.sdrBadgeDragFrequency()).toBe(initial);
    pointer('pointermove', x + delta, 7);
    expect(badge.sdrBadgeDragFrequency()).toBe(initial + delta * 500);
    pointer('pointerup', x + delta, 7);
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ freq: initial + delta * 500 }));
  });

  it('bounds badge tuning to both the zoomed view and the device RF range', async () => {
    const { S, viewport, send } = await load(true);
    const badge = await import('../ui/sdrBadgeGesture');
    viewport.setView({ lo: 9_900_000, hi: 10_100_000 });
    S.setFrequencyLimits(9_950_000, 10_050_000);
    const g = badge.sdrBadgeGeometry()!;
    pointer('pointerdown', g.left + 1, 7);
    pointer('pointermove', -1000, 7);
    expect(badge.sdrBadgeDragFrequency()).toBe(9_950_000);
    pointer('pointermove', 2000, 7);
    expect(badge.sdrBadgeDragFrequency()).toBe(10_050_000);
    pointer('pointerup', 2000, 7);
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ freq: 10_050_000 }));
    S.setFrequencyLimits(9000, 9e9);
    const second = badge.sdrBadgeGeometry()!;
    pointer('pointerdown', second.left + 1, 7);
    pointer('pointermove', -1000, 7);
    expect(badge.sdrBadgeDragFrequency()).toBe(9_900_000);
    pointer('pointermove', 2000, 7);
    expect(badge.sdrBadgeDragFrequency()).toBe(10_100_000);
  });

  it.each(['pointercancel', 'escape', 'lostcapture', 'blur', 'zoom', 'mode', 'disconnect', 'view', 'missing-spectrum', 'missing-frame'])
  ('clears badge preview and capture on %s without a release command', async reason => {
    const { controls, S, state, viewport, send } = await load(true);
    const badge = await import('../ui/sdrBadgeGesture');
    const g = badge.sdrBadgeGeometry()!;
    pointer('pointerdown', g.left + 1, 7);
    pointer('pointermove', g.left + 81, 7);
    if (reason === 'pointercancel') pointer('pointercancel', 500, 7);
    if (reason === 'escape') window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    if (reason === 'lostcapture') {
      const event = new Event('lostpointercapture');
      Object.defineProperty(event, 'pointerId', { value: 1 });
      canvas.dispatchEvent(event);
    }
    if (reason === 'blur') window.dispatchEvent(new Event('blur'));
    if (reason === 'zoom') viewport.zoomEnabled.set(false);
    if (reason === 'mode') controls.selectSdrDemod('am');
    if (reason === 'disconnect') controls.disconnectSdrSession();
    if (reason === 'view') viewport.setView({ lo: 9_900_000, hi: 10_100_000 });
    if (reason === 'missing-spectrum') vi.spyOn(document, 'getElementById').mockReturnValue(null);
    if (reason === 'missing-frame') S.setRtaData(null);
    // Renderers call this getter even when no frame arrives or pointer moves.
    expect(badge.sdrBadgeDragFrequency()).toBeNull();
    vi.mocked(send).mockClear();
    pointer('pointerup', 500, 7);
    expect(send).not.toHaveBeenCalled();
    expect(state.sdrListenHz.get()).toBe(10_000_000);
    expect(S.dragging).toBe(false);
    if (reason !== 'missing-spectrum') expect(canvas.releasePointerCapture).toHaveBeenCalledWith(1);
  });

  it('ignores the top margin when the listening badge has no spectrum frame', async () => {
    const { S, send } = await load();
    S.setRtaData(null);
    drag(410, 490, 7);
    expect(canvas.setPointerCapture).not.toHaveBeenCalled();
    expect(send).not.toHaveBeenCalled();
  });

	it.each([250, 410, 450])('zooms from Signal, dial or Filter (%s) without receiver or marker edits', async x => {
		const { S, state, viewport, filter, locate, send } = await load(true);
		const markers = S.markers.map(m => ({ ...m }));
		drag(x, x + 120);
		expect(viewport.getView()).toEqual({ lo: 9_800_000 + (x - 10) * 500, hi: 9_800_000 + (x + 110) * 500 });
		expect(S.markers).toEqual(markers);
		expect(state.sdrIfbw.get()).toBe(40_000);
		expect(filter.filterDragging()).toBe(false);
		expect(locate.sdrLocateClickPending()).toBe(false);
		expect(send).not.toHaveBeenCalled();
		expect(canvas.setPointerCapture).toHaveBeenCalledWith(1);
		expect(canvas.releasePointerCapture).toHaveBeenCalledTimes(1);
	});

	it.each([false, true])('Ctrl drag places the active marker on native bins with zoom %s', async zoom => {
		const { S, viewport, send } = await load(zoom);
		S.setActiveMkrId(2);
		drag(450, 570, 240, true);
		expect(S.markers[1]).toMatchObject({ enabled: true, mode: 'NORMAL', idx: 70, freq: 10_080_000 });
		expect(viewport.isZoomed()).toBe(false);
		expect(send).not.toHaveBeenCalled();
	});

	it.each([false, true])('selects and drags an existing diamond without a peak jump, zoom %s', async zoom => {
		const { S, plot, send } = await load(zoom);
		Object.assign(S.markers[1], { enabled: true, mode: 'NORMAL', idx: 50, freq: 10_000_000, tracking: true });
		const y = plot.getY(-50) - 15;
		pointer('pointerdown', plot.getX(50, 101), y);
		expect(S.activeMkrId).toBe(2);
		expect(S.markers[1].idx).toBe(50);
		pointer('pointermove', plot.getX(60, 101), y);
		pointer('pointerup', plot.getX(60, 101), y);
		expect(S.markers[1]).toMatchObject({ idx: 60, freq: 10_040_000, tracking: false });
		expect(send).not.toHaveBeenCalled();
	});

	it('hits a valley diamond below the trace at a scaled screen tolerance', async () => {
		const { S, plot, send } = await load(true);
		screenScale = 0.5;
		S.traces[0].powers![70] = -60;
		Object.assign(S.markers[1], { enabled: true, mode: 'NORMAL', idx: 70, freq: 10_080_000 });
		pointer('pointerdown', plot.getX(70, 101) + 16, plot.getY(-60) + 15 + 16);
		expect(S.activeMkrId).toBe(2);
		pointer('pointercancel', 570);
		expect(send).not.toHaveBeenCalled();
	});

	it.each([false, true])('a normal click queues one tune with zoom %s', async zoom => {
		const { send, locate } = await load(zoom);
		pointer('pointerdown', 570);
		pointer('pointerup', 570);
		pointer('pointerup', 570);
		expect(send).toHaveBeenCalledTimes(1);
		expect(send).toHaveBeenCalledWith(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', freq: 10_080_000 }));
		expect(locate.sdrLocateClickPending()).toBe(false);
	});

	it.each(['pointercancel', 'escape', 'lostcapture', 'blur', 'zoom', 'mode', 'disconnect'])('%s cancels ownership and never leaves a release tune', async reason => {
		const { controls, viewport, locate, send } = await load(true);
		pointer('pointerdown', 570);
		if (reason === 'pointercancel') pointer('pointercancel', 570);
		if (reason === 'escape') window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
		if (reason === 'lostcapture') {
			const event = new Event('lostpointercapture');
			Object.defineProperty(event, 'pointerId', { value: 1 });
			canvas.dispatchEvent(event);
		}
		if (reason === 'blur') window.dispatchEvent(new Event('blur'));
		if (reason === 'zoom') { viewport.zoomEnabled.set(false); viewport.zoomEnabled.set(true); }
		if (reason === 'mode') controls.selectSdrDemod('am');
		if (reason === 'disconnect') controls.disconnectSdrSession();
		vi.mocked(send).mockClear();
		pointer('pointerup', 570);
		expect(send).not.toHaveBeenCalled();
		expect(locate.sdrLocateClickPending()).toBe(false);
		expect(canvas.releasePointerCapture).toHaveBeenCalledTimes(1);
	});

	it('restores marker selection and position after cancellation', async () => {
		const { S, plot, send } = await load(true);
		Object.assign(S.markers[1], { enabled: true, mode: 'NORMAL', idx: 50, freq: 10_000_000, tracking: true });
		const before = { ...S.markers[1] };
		const y = plot.getY(-50) - 15;
		pointer('pointerdown', 410, y);
		pointer('pointermove', 570, y);
		pointer('pointercancel', 570, y);
		pointer('pointerup', 570, y);
		expect(S.activeMkrId).toBe(1);
		expect(S.markers[1]).toEqual(before);
		expect(send).not.toHaveBeenCalled();
	});

	it('uses screen pixels for the marquee threshold under UI scaling', async () => {
		const { viewport, send } = await load(true);
		screenScale = 0.5;
		drag(570, 582); // six screen pixels, below the eight-pixel threshold
		expect(viewport.isZoomed()).toBe(false);
		drag(570, 590); // ten screen pixels
		expect(viewport.getView()).toEqual({ lo: 10_080_000, hi: 10_090_000 });
		expect(send).not.toHaveBeenCalled();
	});

	it('keeps Filter edge ownership with zoom off', async () => {
		const { state, filter, send } = await load();
		pointer('pointerdown', 450);
		expect(filter.filterDragging()).toBe(true);
		pointer('pointermove', 470);
		pointer('pointerup', 470);
		expect(state.sdrIfbw.get()).toBe(60_000);
		expect(send).toHaveBeenCalledTimes(1);
		expect(send).toHaveBeenCalledWith(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', ifbw: 60_000 }));
		expect(vi.mocked(send).mock.calls[0][0]).not.toHaveProperty('freq');
	});

	it('uses both diamond coordinates instead of stealing clicks on its vertical line', async () => {
		const { S, plot, send } = await load(true);
		Object.assign(S.markers[1], { enabled: true, mode: 'NORMAL', idx: 70, freq: 10_080_000 });
		const y = plot.getY(-50) - 15 + 12;
		pointer('pointerdown', 570, y);
		pointer('pointerup', 570, y);
		expect(S.activeMkrId).toBe(1);
		expect(send).toHaveBeenCalledWith(expect.objectContaining({ freq: 10_080_000 }));
	});

	it.each(['pointercancel', 'escape'])('cancels a free-tune preview on %s', async reason => {
		const { state, send } = await load();
		pointer('pointerdown', 570);
		pointer('pointermove', 650);
		expect(state.sdrListenHz.get()).toBe(10_120_000);
		if (reason === 'pointercancel') pointer('pointercancel', 650);
		else window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
		pointer('pointerup', 650);
		expect(state.sdrListenHz.get()).toBe(10_000_000);
		expect(send).not.toHaveBeenCalled();
	});

	it('free-tunes a plain drag with zoom off and suppresses locate hover during capture', async () => {
		const { state, locate, send } = await load();
		pointer('pointerdown', 570);
		pointer('pointermove', 650);
		canvas.dispatchEvent(new MouseEvent('mousemove', { clientX: 250, clientY: 240 }));
		expect(locate.sdrLocateClickPending()).toBe(false);
		expect(state.sdrListenHz.get()).toBe(10_120_000);
		pointer('pointerup', 650);
		expect(send).toHaveBeenCalledTimes(1);
		expect(send).toHaveBeenCalledWith(expect.objectContaining({ freq: 10_120_000 }));
	});

	it('keeps the frequency axis meaning even with Ctrl and zoom enabled', async () => {
		const { S, viewport, locate, axis, send } = await load(true);
		pointer('pointerdown', 410, 465, true);
		expect(axis.axisDragging()).toBe(true);
		expect(S.markers.every(marker => !marker.enabled)).toBe(true);
		expect(locate.sdrLocateClickPending()).toBe(false);
		pointer('pointercancel', 410, 465, true);
		expect(axis.axisDragging()).toBe(false);
		expect(viewport.isZoomed()).toBe(false);
		expect(send).not.toHaveBeenCalled();
	});

	it.each(['demod', 'sdr', 'rta', 'preset', 'band'])('discards tuning drafts before %s takeover focus and change events', async action => {
		const { controls, state } = await load();
		document.body.insertAdjacentHTML('beforeend', `
			<div id="sdr-step-settings-panel"><input id="input-sdr-step-fine"></div>
			<div id="sdr-demod-group"><button id="mode-demod" data-sdr-demod="am">AM</button></div>
			<button id="btn-mode-sdr">SDR</button><button id="btn-mode-rta">RTA</button>
			<button id="mode-preset" data-action="preset">Preset</button>
			<button id="mode-band" data-sdr-band="fm">FM</button>`);
		controls.bindActions();
		const input = document.getElementById('input-sdr-step-fine') as HTMLInputElement;
		const before = state.currentSdrStepHz('fine');
		input.focus();
		input.value = String(before + 123);
		input.dispatchEvent(new Event('input', { bubbles: true }));
		const id = action === 'sdr' || action === 'rta' ? `btn-mode-${action}` : `mode-${action}`;
		const button = document.getElementById(id) as HTMLButtonElement;
		button.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true }));
		input.dispatchEvent(new Event('change', { bubbles: true }));
		button.focus();
		expect(state.currentSdrStepHz('fine')).toBe(before);
		expect(input.value).toBe(String(before));
		expect(input.dataset.edited).toBeUndefined();
	});

	it('ignores unowned margins and rejects clean clicks released outside the plot vertically', async () => {
		const { send } = await load(true);
		pointer('pointerdown', 1, 1);
		pointer('pointerup', 1, 1);
		expect(canvas.setPointerCapture).not.toHaveBeenCalled();
		pointer('pointerdown', 570);
		pointer('pointerup', 570, 479);
		expect(send).not.toHaveBeenCalled();
		pointer('pointerdown', 570);
		pointer('pointerup', 570);
		expect(send).toHaveBeenCalledTimes(1);
	});
});
