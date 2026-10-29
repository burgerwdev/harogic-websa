import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DecodeEnvelope } from '../sdr/decodeProtocol';

vi.mock('../audio/sdrAudio', () => ({
	prepareSdrAudioTransition: vi.fn(), setSdrAudioEnabled: vi.fn(), setSdrAudioMode: vi.fn(),
	setSdrReceiverAudioError: vi.fn(),
}));

class DisplaySocket {
	static OPEN = 1;
	static CONNECTING = 0;
	readyState = 1;
	binaryType = '';
	onopen: (() => void) | null = null;
	onmessage: ((event: MessageEvent) => void) | null = null;
	onclose: (() => void) | null = null;
	onerror: (() => void) | null = null;
	sent: Record<string, unknown>[] = [];
	send(message: string) { this.sent.push(JSON.parse(message)); }
}
let socket: DisplaySocket;

beforeEach(() => {
	vi.resetModules();
	vi.useFakeTimers();
	localStorage.clear();
	document.body.innerHTML = '<input id="input-offset"><button id="btn-mode-sdr"></button><button id="btn-mode-rta"></button>' +
		'<select id="select-trace-mode"><option>CLEAR_WRITE</option><option>AVERAGE</option><option>MAX_HOLD</option><option>OFF</option></select>' +
		'<div id="trace-avg-row"><select id="select-trace-avg"><option>16</option><option>64</option><option>0</option></select></div>' +
		'<span id="trace-avg-status"></span><button id="btn-view-freeze"></button>';
	vi.stubGlobal('WebSocket', class extends DisplaySocket {
		constructor() { super(); socket = this; }
	});
});
afterEach(() => {
	vi.clearAllTimers();
	vi.useRealTimers();
	vi.unstubAllGlobals();
	document.body.replaceChildren();
});

// Static imports cannot re-exercise preference restoration at a page reload.
async function load() {
	const ws = await import('../core/ws');
	const controls = await import('../ui/controls');
	const fit = await import('../ui/refAutoScale');
	ws.connectWS();
	socket.onopen!();
	return { ws, controls, fit };
}

function status(mode: string, response: Record<string, unknown> = {}) {
	const sdr = { center: 1e9, decimate: 16, listen: 1e9, demod: 'am', if_bw: 6000,
		ref: 0, ref_set: false };
	return { cmd: 'STATUS', mode, connected: true, center: 1e9, span: 3.125e6, ref: 0,
		auto_ref: { result: 'idle', target: null, pending: null, seq: 0, adjusting: false,
			hardware_adjusting: false, last_peak: null, last_noise_floor: null },
		req: { swp: {}, rta: {}, sdr: { ...sdr } }, actual: {}, sdr: { ...sdr,
			actual: { start: 998437500, stop: 1001562500 } }, ...response };
}

function receive(payload: Record<string, unknown>) {
	socket.onmessage!({ data: JSON.stringify(payload) } as MessageEvent);
}

function receiverStatus(freq: number, mode: string, version: number, response: Record<string, unknown> = {}) {
	const snapshot = status('sdr');
	return { ...snapshot, config_version: version, ...response,
		sdr: { ...snapshot.sdr, listen: freq, demod: mode,
			...(response.sdr as Record<string, unknown> | undefined) } };
}
describe('SDR entry and reload', () => {
	it('hands an RTA listen frequency to both SDR capture and receiver', async () => {
		const { controls } = await load();
		// Static imports would retain the module graph discarded by load()'s resetModules fixture.
		const { sdrCenterHz, sdrListenHz, sdrDecimate } = await import('../ui/sdrState');
		receive(status('rta'));
		sdrCenterHz.confirm(1e9);
		sdrListenHz.confirm(1e9);
		sdrDecimate.confirm(16);
		socket.sent.length = 0;

		controls.listenAtFreq(216e6);
		expect(socket.sent).toMatchObject([{ cmd: 'SET_MODE', mode: 'sdr' }]);
		expect(sdrCenterHz.get()).toBe(216e6);
		expect(sdrListenHz.get()).toBe(216e6);

		// The mode acknowledgement still contains the previous hardware tuning.
		receive(status('sdr'));
		expect(socket.sent.filter(command => command.cmd === 'SET_SDR')).toEqual([
			{ cmd: 'SET_SDR', center: 216e6, decimate: 16, request_id: expect.any(String) },
		]);
		expect(socket.sent.filter(command => command.cmd === 'SET_SDR_CHANNEL' && 'freq' in command)).toEqual([
			{ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 216e6, request_id: expect.any(String) },
		]);
	});

	it('recovers from a missing tune ACK after periodic state and timeout', async () => {
		await load();
		// These modules must share load()'s fresh graph after vi.resetModules().
		const { requestSdrTune } = await import('../ui/sdrTuneRequest');
		const { sdrListenHz } = await import('../ui/sdrState');
		const S = await import('../core/store');
		receive(receiverStatus(1e9, 'cw', 1));
		requestSdrTune(1e9 + 1000);
		requestSdrTune(1e9 + 2000);
		const first = socket.sent.at(-1)!;
		for (let i = 0; i < 3; i++) {
			vi.advanceTimersByTime(900);
			receive(receiverStatus(1e9 + 1000, 'cw', 2));
		}
		vi.advanceTimersByTime(300);
		expect(sdrListenHz.get()).toBe(1e9 + 1000);
		expect(sdrListenHz.pending()).toBe(false);
		expect(S.statusError).not.toBeNull();
		requestSdrTune(1e9 + 3000);
		const next = socket.sent.at(-1)!;
		receive(receiverStatus(1e9 + 3000, 'cw', 3,
			{ response_to: 'SET_SDR_CHANNEL', request_id: next.request_id }));
		receive(receiverStatus(1e9 + 1000, 'cw', 2,
			{ response_to: 'SET_SDR_CHANNEL', request_id: first.request_id }));
		expect(sdrListenHz.get()).toBe(1e9 + 3000);
		expect(sdrListenHz.pending()).toBe(false);
		expect(S.statusError).toBeNull();
		expect(socket.sent.filter(command => String(command.request_id).startsWith('sdr-tune-'))
			.map(command => command.freq)).toEqual([1e9 + 1000, 1e9 + 3000]);
	});

	it.each(['local demod', 'external demod', 'external capture'])('cancels tuning superseded by %s', async context => {
		const { controls } = await load();
		// These modules must share load()'s fresh graph after vi.resetModules().
		const { requestSdrTune } = await import('../ui/sdrTuneRequest');
		const { sdrListenHz, sdrDemod, sdrCenterHz } = await import('../ui/sdrState');
		const S = await import('../core/store');
		receive(receiverStatus(1e9, 'cw', 1));
		requestSdrTune(1e9 + 1000);
		requestSdrTune(1e9 + 2000);
		const first = socket.sent.at(-1)!;
		if (context === 'local demod') {
			controls.selectSdrDemod('am');
			const edit = socket.sent.at(-1)!;
			receive(receiverStatus(1e9, 'am', 3,
				{ response_to: 'SET_SDR_CHANNEL', request_id: edit.request_id }));
		} else if (context === 'external demod') receive(receiverStatus(1e9, 'am', 3));
		else receive(receiverStatus(1e9, 'cw', 3, { sdr: { center: 1e9 + 10000, decimate: 32 } }));
		receive(receiverStatus(1e9 + 1000, 'cw', 2,
			{ response_to: 'SET_SDR_CHANNEL', request_id: first.request_id }));
		vi.advanceTimersByTime(3000);
		expect(sdrListenHz.get()).toBe(1e9);
		expect(sdrListenHz.pending()).toBe(false);
		expect(sdrDemod.get()).toBe(context === 'external capture' ? 'cw' : 'am');
		expect(sdrCenterHz.get()).toBe(context === 'external capture' ? 1e9 + 10000 : 1e9);
		expect(S.statusError).toBeNull();
		expect(socket.sent.filter(command => String(command.request_id).startsWith('sdr-tune-'))).toHaveLength(1);
	});

	it('settles a tune ACK older than a receiver filter snapshot without reverting frequency', async () => {
		const { controls } = await load();
		// These modules must share load()'s fresh graph after vi.resetModules().
		const { requestSdrTune } = await import('../ui/sdrTuneRequest');
		const { sdrListenHz } = await import('../ui/sdrState');
		const S = await import('../core/store');
		receive(receiverStatus(1e9, 'cw', 1));
		requestSdrTune(1e9 + 1000);
		requestSdrTune(1e9 + 2000);
		const first = socket.sent.at(-1)!;
		controls.selectSdrIfbw(500);
		const filter = socket.sent.at(-1)!;
		receive(receiverStatus(1e9 + 1000, 'cw', 3,
			{ response_to: 'SET_SDR_CHANNEL', request_id: filter.request_id }));
		receive(receiverStatus(1e9, 'cw', 2,
			{ response_to: 'SET_SDR_CHANNEL', request_id: first.request_id }));
		expect(sdrListenHz.confirmedValue()).toBe(1e9 + 1000);
		expect(S.configVersion).toBe(3);
		const next = socket.sent.at(-1)!;
		expect(next).toMatchObject({ cmd: 'SET_SDR_CHANNEL', freq: 1e9 + 2000 });
		receive(receiverStatus(1e9 + 2000, 'cw', 4,
			{ response_to: 'SET_SDR_CHANNEL', request_id: next.request_id }));
		vi.advanceTimersByTime(3000);
		expect(sdrListenHz.get()).toBe(1e9 + 2000);
		expect(sdrListenHz.pending()).toBe(false);
		expect(S.statusError).toBeNull();
	});

	it.each(['device STATUS', 'control websocket'])('retains radiofax paper and selection through %s loss', async loss => {
		const { controls } = await load();
		// Store and stream must belong to load()'s freshly isolated browser session.
		const fax = await import('../sdr/radiofaxLog');
		const stream = await import('../sdr/iqStream');
		fax.appendRadiofaxRows({ width: 1, row: 0, rows: 2, gray: Uint8Array.of(40, 80), revision: 0 }, {
			serial: 'disconnect', epoch: 'meta', hz: 9_110_000, ioc: 576, lpm: 120, lines: 2,
			complete: false, partial: false, segmentIndex: 0, transmissionId: '1', missingRows: [], reason: null,
		}, 'original-stream');
		const pageId = fax.radiofaxVisiblePage()!.id;
		fax.setRadiofaxView(pageId);
		if (loss === 'device STATUS') receive(status('sdr', { connected: false }));
		else socket.onclose!();
		expect(fax.radiofaxStatus().viewId).toBe(pageId);
		expect([...fax.radiofaxVisible()!.picture.gray]).toEqual([40, 80]);
		expect(fax.radiofaxVisible()!.meta.partial).toBe(true);
		expect(fax.radiofaxStatus().decoder).toBeNull();
		stream.consumeDecode({ version: 1, channel: 1, channel_id: 18, epoch: 10, seq: 0, input_index: '0',
			input_rate: 62500000, input_fraction_num: '0', input_fraction_den: '1', delay_seconds: 0,
			delay_known: true, causal_presentation: false, mode: 'radiofax', event: { type: 'start' } });
		expect(fax.radiofaxStatus().pages).toHaveLength(1);
		expect(fax.radiofaxVisiblePage()!.id).toBe(pageId);
		controls.disconnectSdrSession();
	});
	it.each([0, 60])('fits a fresh entry with offset %s, then explicit Auto supersedes a stored level', async offset => {
		const { controls, fit } = await load();
		// Static imports would retain the previous module graph across vi.resetModules().
		const { commitLevelOffset } = await import('../ui/panels/refAmp');
		commitLevelOffset(offset);
		receive(status('std'));
		controls.setGraphMode('sdr');
		receive(status('sdr'));
		fit.maybeRequestSdrFit();
		const entry = socket.sent.at(-1)!;
		expect(entry).toMatchObject({ cmd: 'AUTO_SCALE', display_only: true, current_ref: 0 });
		receive(status('sdr', { response_to: 'AUTO_SCALE', request_id: entry.request_id,
			auto_ref: { result: 'applied', target: -85, last_peak: -65, last_noise_floor: -115, seq: 1 } }));
		const { getDisplayRef } = await import('../ui/displayRef');
		expect(getDisplayRef()).toBe(-85 + offset);

		fit.requestSdrEntryFit();
		fit.noteSdrRefState({ ref: -45, ref_set: true });
		expect(getDisplayRef()).toBe(-45 + offset);
		const count = socket.sent.length;
		fit.maybeRequestSdrFit();
		expect(socket.sent).toHaveLength(count);
		fit.autoScaleRequest();
		const explicit = socket.sent.at(-1)!;
		expect(explicit.display_only).toBeUndefined();
		expect(explicit.current_ref).toBe(-45);
		receive(status('sdr', { response_to: 'AUTO_SCALE', request_id: explicit.request_id,
			auto_ref: { result: 'applied', target: -75, last_peak: -55, last_noise_floor: -105, seq: 2 } }));
		expect(getDisplayRef()).toBe(-75 + offset);
		expect(JSON.parse(document.getElementById('spectrum')!.dataset.sdrRefDbg ?? '{}')).toMatchObject({ seq: 2, result: 'applied', shown: -75 + offset, peak: -55 });
	});

	it.each([-200, -160, 40, 100].flatMap(ref => [0, 60].map(offset => ({ ref, offset }))))(
		'Preset preserves raw Ref $ref when resetting offset $offset, then fits SDR', async ({ ref, offset }) => {
		const { controls, fit } = await load();
		// These actions must share load()'s fresh module graph after vi.resetModules().
		const { commitRefLevel, commitLevelOffset } = await import('../ui/panels/refAmp');
		const { displayOffset } = await import('../ui/displayState');
		const { getY, plotRect } = await import('../render/plot');
		const { getDisplayRef, pendingDisplayRef } = await import('../ui/displayRef');
		const S = await import('../core/store');
		receive(status('sdr'));
		commitLevelOffset(offset);
		// The trace is above the raw -160 window and below the raw +40 window.
		commitRefLevel(ref + offset);
		expect(getDisplayRef()).toBe(ref + offset);
		const markerY = getY(-65);
		controls.presetAll();
		const preset = socket.sent.at(-1)!;
		// Backend preset() resets AutoRef to idle and increments its sequence. That
		// STATUS is processed after applyPresetSuccess has armed the next-frame fit.
		receive(status('sdr', { response_to: 'SET_PRESET', request_id: preset.request_id,
			auto_ref: { result: 'idle', target: null, pending: null, seq: 2, adjusting: false,
				hardware_adjusting: false, last_peak: null, last_noise_floor: null } }));
		expect(displayOffset.get()).toBe(0);
		expect(getDisplayRef()).toBe(ref);
		expect(getY(-65)).toBeCloseTo(markerY, 6);
		expect(pendingDisplayRef()).toBeNull();
		fit.maybeRequestSdrFit();
		const request = socket.sent.at(-1)!;
		expect(request).toMatchObject({ cmd: 'AUTO_SCALE', display_only: true, current_ref: Math.max(-160, Math.min(40, ref)), range_db: 100 });
		receive(status('sdr', { response_to: 'AUTO_SCALE', request_id: request.request_id,
			auto_ref: { result: 'adjusting', target: null, pending: null, seq: 3, adjusting: true,
				hardware_adjusting: false, last_peak: null, last_noise_floor: null } }));
		// advance_auto first calculates this display-only target, then a fresh clean
		// observation confirms it. Hardware SDR ref stays at the preset's 0 dBm.
		receive(status('sdr', { auto_ref: { result: 'adjusting', target: null, pending: -30,
			seq: 3, adjusting: true, hardware_adjusting: false, last_peak: -65, last_noise_floor: -115 } }));
		expect(getDisplayRef()).toBe(ref);
		const complete = status('sdr', { auto_ref: { result: 'applied', target: -30, pending: null,
			seq: 4, adjusting: false, hardware_adjusting: false, last_peak: -65, last_noise_floor: -115 } });
		receive(complete);
		expect(getDisplayRef()).toBe(-30);
		expect(-65).toBeLessThan(getDisplayRef());
		expect(-115).toBeGreaterThan(getDisplayRef() - S.totalDivs * S.dbPerDiv);
		const rect = plotRect();
		expect(getY(-65)).toBeCloseTo(rect.y + rect.h * 0.35, 6);
		expect(getY(-115)).toBeCloseTo(rect.y + rect.h * 0.85, 6);
		vi.advanceTimersByTime(6000);
		for (let frame = 0; frame < 3; frame++) {
			receive(complete);
			fit.maybeRequestSdrFit();
		}
		expect(getDisplayRef()).toBe(-30);
		expect(fit.autoScaleBusy()).toBe(false);
		expect(socket.sent.filter(command => command.cmd === 'AUTO_SCALE')).toHaveLength(1);
		expect(socket.sent.filter(command => command.cmd === 'SET_REF')).toMatchObject([
			{ cmd: 'SET_REF', mode: 'manual', ref: Math.max(-160, Math.min(40, ref)) },
		]);
		// Later manual intent owns the display again; a sticky fit cannot undo it.
		commitRefLevel(-45);
		receive(complete);
		fit.maybeRequestSdrFit();
		expect(getDisplayRef()).toBe(-45);
		expect(socket.sent.filter(command => command.cmd === 'AUTO_SCALE')).toHaveLength(1);
	});

	it.each(['sdr', 'std'].flatMap(mode => [true, false].flatMap(connected =>
		[true, false].flatMap(audioOn => [true, false].map(ownRef => ({ mode, connected, audioOn, ownRef }))))))(
		'restores playback and a visible axis after reload from $mode (connected=$connected, audio=$audioOn, ownRef=$ownRef)',
		async ({ mode, connected, audioOn, ownRef }) => {
			localStorage.setItem('web-sa-mode', 'sdr');
			localStorage.setItem('web-sa-sdr-audio', audioOn ? '1' : '0');
			const { fit } = await load();
			// Observe the same fresh module graph as the reloaded websocket.
			const audio = await import('../audio/sdrAudio');
			const { getY, plotRect } = await import('../render/plot');
			const { getDisplayRef } = await import('../ui/displayRef');
			const live = () => {
				const value = status('sdr');
				return { ...value, sdr: { ...value.sdr, ref: ownRef ? -40 : 0, ref_set: ownRef } };
			};
			receive({ ...live(), mode, connected });
			if (!connected) {
				vi.advanceTimersByTime(1000);
				expect(audio.setSdrAudioEnabled).not.toHaveBeenCalledWith(true);
			}
			receive(live());
			fit.maybeRequestSdrFit();
			if (!ownRef) {
				const request = socket.sent.find(command => command.cmd === 'AUTO_SCALE')!;
				expect(request.display_only).toBe(true);
				receive({ ...live(), response_to: 'AUTO_SCALE', request_id: request.request_id,
					auto_ref: { result: 'applied', target: -30, seq: 1, last_peak: -65, last_noise_floor: -115 } });
			} else {
				expect(socket.sent.some(command => command.cmd === 'AUTO_SCALE')).toBe(false);
			}
			vi.advanceTimersByTime(800);
			expect(audio.setSdrAudioEnabled).toHaveBeenLastCalledWith(audioOn);
			expect(localStorage.getItem('web-sa-sdr-audio')).toBe(audioOn ? '1' : '0');
			expect(getDisplayRef()).toBe(ownRef ? -40 : -30);
			const rect = plotRect();
			for (const power of [-65, -115]) {
				expect(getY(power)).toBeGreaterThan(rect.y);
				expect(getY(power)).toBeLessThan(rect.y + rect.h);
			}
			const commands = socket.sent.length;
			vi.mocked(audio.setSdrAudioEnabled).mockClear();
			receive(live());
			fit.maybeRequestSdrFit();
			vi.advanceTimersByTime(1000);
			expect(socket.sent).toHaveLength(commands);
			expect(audio.setSdrAudioEnabled).not.toHaveBeenCalled();
		});

	it.each(['sdr', 'rta', 'std'])('keeps confirmed %s through Preset and a fresh websocket startup', async (mode) => {
		const { controls } = await load();
		receive(status(mode));
		// Static imports would retain the module graph from before the simulated page reload.
		const S = await import('../core/store');
		const { setTraceMode, setTraceAverage, rememberTraceMode, SDR_DEFAULT_AVG } = await import('../ui/traceOps');
		setTraceMode('MAX_HOLD');
		rememberTraceMode(mode === 'sdr');
		setTraceAverage(0);
		S.rtaAvgN.fill(123);
		S.rtaAvgSum.fill(new Float32Array([123]));
		S.rtaDone.fill(true);
		controls.presetAll();
		const preset = socket.sent.at(-1)!;
		receive(status(mode, { response_to: 'SET_PRESET', request_id: preset.request_id }));
		expect(localStorage.getItem('web-sa-mode')).toBe(mode);
		const expectedTraceMode = mode === 'sdr' ? 'AVERAGE' : 'CLEAR_WRITE';
		expect(S.traces[0].mode).toBe(expectedTraceMode);
		expect(S.traces[0].avgTargetRta).toBe(SDR_DEFAULT_AVG);
		expect(S.rtaAvgN).toEqual([0, 0, 0, 0]);
		expect(S.rtaAvgSum).toEqual([null, null, null, null]);
		expect(S.rtaDone).toEqual([false, false, false, false]);
		expect((document.getElementById('select-trace-mode') as HTMLSelectElement).value).toBe(expectedTraceMode);
		expect((document.getElementById('select-trace-avg') as HTMLSelectElement).value).toBe(String(SDR_DEFAULT_AVG));
		expect(document.getElementById('trace-avg-row')!.style.display).toBe(mode === 'sdr' ? '' : 'none');
		expect(document.getElementById('trace-avg-status')!.textContent).toBe(mode === 'sdr' ? String(SDR_DEFAULT_AVG) : '');

		// A fresh browser waits for STATUS before restoring the saved graph preference.
		vi.clearAllTimers();
		vi.resetModules();
		await load();
		expect(socket.sent.filter(command => command.cmd === 'SET_MODE')).toEqual([]);
		receive(status(mode));
		expect(socket.sent.filter(command => command.cmd === 'SET_MODE')).toMatchObject([{ cmd: 'SET_MODE', mode }]);
		const { currentGraphMode } = await import('../ui/graphMode');
		expect(currentGraphMode()).toBe(mode);
		expect(document.body.dataset.deviceMode).toBe(mode);
		const reloadedStore = await import('../core/store');
		expect(reloadedStore.traces[0].mode).toBe(expectedTraceMode);
		expect(reloadedStore.traces[0].avgTargetRta).toBe(SDR_DEFAULT_AVG);
		expect(reloadedStore.rtaAvgN).toEqual([0, 0, 0, 0]);
		expect((document.getElementById('select-trace-mode') as HTMLSelectElement).value).toBe(expectedTraceMode);
	});

	it.each([false, true])('restores the saved AM detector preference (%s) on SDR entry after reload', async (enabled) => {
		localStorage.setItem('web-sa-mode', 'sdr');
		localStorage.setItem('web-sa-sdr-am-sync', enabled ? '1' : '0');
		await load();
		const { sdrAmSync } = await import('../ui/sdrState');
		const primary = (am_sync: boolean) => ({ sdr: { center: 1e9, decimate: 32,
			channels: [{ channel: 1, enabled: true, freq: 1e9, mode: 'am', if_bw: 6000, am_sync, am_sync_generation: 0 }] } });
		// The device's first report still has its native default; the module-load snapshot
		// must survive that report until the stored detector choice has been applied.
		receive(status('sdr', primary(false)));
		expect(socket.sent.filter(command => 'am_sync' in command)).toEqual([
			{ cmd: 'SET_SDR_CHANNEL', channel: 1, am_sync: enabled, request_id: expect.any(String) },
		]);
		expect(sdrAmSync.get()).toBe(enabled);
		expect(sdrAmSync.pending()).toBe(enabled);
		receive(status('sdr', primary(enabled)));
		expect(sdrAmSync.confirmedValue()).toBe(enabled);
		expect(sdrAmSync.pending()).toBe(false);
		expect(localStorage.getItem('web-sa-sdr-am-sync')).toBe(enabled ? '1' : '0');
		const count = socket.sent.filter(command => 'am_sync' in command).length;
		receive(status('sdr', primary(enabled)));
		expect(socket.sent.filter(command => 'am_sync' in command)).toHaveLength(count);
	});

	it.each([false, true])('restores the initial detector preference (%s) once after disconnected SDR startup', async (enabled) => {
		localStorage.setItem('web-sa-mode', 'sdr');
		localStorage.setItem('web-sa-sdr-am-sync', enabled ? '1' : '0');
		await load();
		// Fresh modules preserve the page-load preference snapshot across the first STATUS.
		const { sdrAmSync } = await import('../ui/sdrState');
		const primary = (am_sync: boolean) => ({ sdr: { center: 1e9, decimate: 32,
			channels: [{ channel: 1, enabled: true, freq: 1e9, mode: 'am', if_bw: 6000,
				am_sync, am_sync_generation: 0 }] } });
		receive(status('sdr', { ...primary(false), connected: false }));
		expect(socket.sent.filter(command => 'am_sync' in command)).toEqual([]);
		receive(status('sdr', primary(false)));
		expect(socket.sent.filter(command => 'am_sync' in command)).toMatchObject([
			{ cmd: 'SET_SDR_CHANNEL', channel: 1, am_sync: enabled },
		]);
		expect(sdrAmSync.get()).toBe(enabled);
		receive(status('sdr', primary(enabled)));
		expect(sdrAmSync.pending()).toBe(false);
		receive(status('sdr', { ...primary(enabled), connected: false }));
		receive(status('sdr', primary(enabled)));
		expect(socket.sent.filter(command => 'am_sync' in command)).toHaveLength(1);
	});

	it('dispatches a correlated detector rejection without undoing pending state for unrelated errors', async () => {
		localStorage.setItem('web-sa-sdr-am-sync', '1');
		await load();
		const { sdrAmSync } = await import('../ui/sdrState');

		receive(status('sdr', { sdr: { center: 1e9, decimate: 32,
			channels: [{ channel: 1, enabled: true, freq: 1e9, mode: 'am', if_bw: 6000, am_sync: false, am_sync_generation: 0 }] } }));
		const request = socket.sent.find(command => command.am_sync === true)!;
		receive({ cmd: 'ERROR', response_to: 'SET_SDR_CHANNEL', request_id: 'other-ifbw-request', error: 'rejected' });
		expect(sdrAmSync.pending()).toBe(true);
		receive({ cmd: 'ERROR', response_to: 'SET_SDR_CHANNEL', request_id: request.request_id, error: 'rejected' });
		expect(sdrAmSync.pending()).toBe(false);
		expect(sdrAmSync.desiredValue()).toBeNull();
		expect(sdrAmSync.get()).toBe(false);
		expect(localStorage.getItem('web-sa-sdr-am-sync')).toBe('0');
	});

  it('keeps repeated device errors bounded while STATUS and controls continue, beyond notice expiry', async () => {
    const { controls, fit } = await load();
    // These modules must share load()'s reset module graph to model a fresh page.
    const S = await import('../core/store');
    S.initStore();
    const { renderAll } = await import('../render/spectrum');
    const { setRenderer, renderRequestCount } = await import('../render/redraw');
    const { currentGraphMode } = await import('../ui/graphMode');
    const { setLang } = await import('../core/i18n');
    setLang('en');
    document.body.insertAdjacentHTML('beforeend', '<span id="status-error" role="alert" aria-atomic="true"></span>');
    const alert = vi.spyOn(window, 'alert').mockImplementation(() => { throw new Error('blocking dialog'); });
    try {
      receive(status('std'));
      fit.postRefNotice('Transient reference feedback', 100);
      const failure = { cmd: 'ERROR', code: 'capture_failed', msg: 'capture failure' };
      receive(failure);
      const repaints = renderRequestCount();
      for (let i = 0; i < 100; i++) receive(failure);
      expect(renderRequestCount()).toBe(repaints);
      expect(S.canvas.dataset.error).toBe('Device: capture failure');
      expect(document.querySelectorAll('#status-error')).toHaveLength(1);
      expect(document.getElementById('status-error')?.textContent).toBe(S.canvas.dataset.error);
      vi.advanceTimersByTime(101);
      expect(S.noticeText).toBeNull();
      expect(S.canvas.dataset.error).toBe('Device: capture failure');
      receive(status('rta'));
      expect(currentGraphMode()).toBe('rta');
      controls.setGraphMode('std');
      expect(socket.sent.at(-1)).toMatchObject({ cmd: 'SET_MODE', mode: 'std' });
      receive(status('std'));
      expect(currentGraphMode()).toBe('std');
      receive({ cmd: 'ERROR', code: 'another_failure', msg: 'new diagnostic' });
      expect(S.canvas.dataset.error).toBe('Device: new diagnostic');
      expect(document.getElementById('status-error')?.textContent).toBe('Device: new diagnostic');
      for (const view of ['pnm', 'harm'] as const) {
        S.setViewMode(view);
        delete S.canvas.dataset.error;
        renderAll();
        expect(S.canvas.dataset.error).toBe('Device: new diagnostic');
      }
      expect(alert).not.toHaveBeenCalled();
    } finally {
      alert.mockRestore();
      setRenderer(null);
    }
  });

	// Dynamic imports below intentionally share load()'s fresh module graph after
	// vi.resetModules(); static imports would observe a different socket/store.
	it.each([
		[{ cmd: 'SET_SDR', center: 21e6 }, { cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 21e6 }],
		[{ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 21e6 }, { cmd: 'SET_SDR_TUNE', listen: 21e6 }],
		[{ cmd: 'SET_SDR_TUNE', listen: 21e6 }, { cmd: 'SET_SDR', center: 21e6 }],
	])('clears a rejected frequency command only after a related successful retry (%j)', async (failed, retry) => {
		await load();
		const { send } = await import('../core/wsSend');
		const S = await import('../core/store');
		receive(status('sdr'));
		send(failed);
		const request = socket.sent.at(-1)!;
		receive({ cmd: 'ERROR', response_to: request.cmd, request_id: request.request_id,
			msg: 'channel frequency outside device limits' });
		const diagnostic = S.statusError;
		expect(diagnostic).toContain('channel frequency outside device limits');
		receive(status('sdr'));
		receive(status('sdr', { response_to: 'STATUS', request_id: request.request_id }));
		for (const unrelated of [
			{ cmd: 'SET_REF', ref: -30 },
			{ cmd: 'SET_SDR_CHANNEL', channel: 1, volume: 0.8 },
			{ cmd: 'SET_SDR_CHANNEL', channel: 2, freq: 21e6 },
		]) {
			send(unrelated);
			const command = socket.sent.at(-1)!;
			receive(status('sdr', { response_to: command.cmd, request_id: command.request_id }));
			expect(S.statusError).toBe(diagnostic);
		}
		send(retry);
		const recovery = socket.sent.at(-1)!;
		expect(S.statusError).toBe(diagnostic);
		receive(status('sdr', { response_to: recovery.cmd, request_id: recovery.request_id }));
		expect(S.statusError).toBeNull();
	});

	it('keeps a newer command failure when an older success or failure arrives', async () => {
		await load();
		const { send } = await import('../core/wsSend');
		const S = await import('../core/store');
		receive(status('sdr'));
		const issue = (request_id: string) => {
			send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 21e6, request_id });
			return socket.sent.at(-1)!;
		};
		const oldSuccess = issue('old-success');
		const oldFailure = issue('old-failure');
		const latest = issue('latest');
		receive({ cmd: 'ERROR', response_to: latest.cmd, request_id: latest.request_id, msg: 'new failure' });
		const diagnostic = S.statusError;
		receive(status('sdr', { response_to: oldSuccess.cmd, request_id: oldSuccess.request_id }));
		expect(S.statusError).toBe(diagnostic);
		receive({ cmd: 'ERROR', response_to: oldFailure.cmd, request_id: oldFailure.request_id, msg: 'old failure' });
		expect(S.statusError).toBe(diagnostic);
		const retry = issue('retry');
		receive(status('sdr', { response_to: retry.cmd }));
		receive(status('sdr', { response_to: 'SET_REF', request_id: retry.request_id }));
		expect(S.statusError).toBe(diagnostic);
		receive(status('sdr', { response_to: retry.cmd, request_id: retry.request_id }));
		expect(S.statusError).toBeNull();
	});

	it.each([false, true])('ignores a delayed older failure after successful recovery (prior visible failure: %s)', async visibleFailure => {
		await load();
		const { send } = await import('../core/wsSend');
		const S = await import('../core/store');
		receive(status('sdr'));
		send({ cmd: 'SET_SDR', center: 21e6, request_id: 'delayed' });
		if (visibleFailure) {
			send({ cmd: 'SET_SDR', center: 21e6, request_id: 'failed' });
			receive({ cmd: 'ERROR', response_to: 'SET_SDR', request_id: 'failed', msg: 'capture rejected' });
			expect(S.statusError).toContain('capture rejected');
		}
		send({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 21e6, request_id: 'recovery' });
		receive(status('sdr', { response_to: 'SET_SDR_CHANNEL', request_id: 'recovery' }));
		expect(S.statusError).toBeNull();
		receive({ cmd: 'ERROR', response_to: 'SET_SDR', request_id: 'delayed', msg: 'obsolete rejection' });
		expect(S.statusError).toBeNull();
		// Recovery is scoped: a later independent failure must remain visible.
		send({ cmd: 'SET_REF', ref: -30, request_id: 'unrelated' });
		receive({ cmd: 'ERROR', response_to: 'SET_REF', request_id: 'unrelated', msg: 'reference rejected' });
		expect(S.statusError).toContain('reference rejected');
	});

	it.each(['SET_MODE', 'CONNECT'])('clears command failure after confirmed %s, retaining it across a socket reconnect', async cmd => {
		await load();
		const { send } = await import('../core/wsSend');
		const S = await import('../core/store');
		receive(status('sdr'));
		send({ cmd: 'SET_SDR', center: 21e6 });
		const failure = socket.sent.at(-1)!;
		receive({ cmd: 'ERROR', response_to: failure.cmd, request_id: failure.request_id, msg: 'rejected capture' });
		const diagnostic = S.statusError;
		send({ cmd, mode: 'std' });
		const abandoned = socket.sent.at(-1)!;
		socket.onclose!();
		vi.advanceTimersByTime(1000);
		socket.onopen!();
		receive(status('std'));
		receive(status('std', { response_to: cmd, request_id: abandoned.request_id }));
		expect(S.statusError).toBe(diagnostic);
		send({ cmd, mode: 'std' });
		const recovery = socket.sent.at(-1)!;
		receive(status('std', { response_to: cmd, request_id: recovery.request_id }));
		expect(S.statusError).toBeNull();
	});

	it('does not clear an uncorrelated device failure on a successful command', async () => {
		await load();
		const { send } = await import('../core/wsSend');
		const S = await import('../core/store');
		receive({ cmd: 'ERROR', msg: 'device capture failed' });
		const diagnostic = S.statusError;
		send({ cmd: 'CONNECT' });
		const recovery = socket.sent.at(-1)!;
		receive(status('std', { response_to: recovery.cmd, request_id: recovery.request_id }));
		expect(S.statusError).toBe(diagnostic);
	});

	it.each(['RECEIVER_ERROR', 'ERROR'])('keeps %s scoped while spectrum and mode requests continue', async cmd => {
		const { controls } = await load();
		// These observers must share load()'s fresh module graph after resetModules().
		const S = await import('../core/store');
		const { pendingGraphMode } = await import('../ui/graphMode');
		S.initStore();
		document.body.insertAdjacentHTML('beforeend', '<span id="sdr-receiver-error" hidden></span>');
		receive(status('sdr'));
		controls.setGraphMode('rta');
		const error = 'receiver input queue failure: full';
		receive({ cmd, code: 'receiver_failed', error, msg: error, failed: true, scope: 'all_receivers' });
		expect(S.statusError).toBeNull();
		expect(pendingGraphMode()).toBe('rta');
		expect(document.getElementById('sdr-receiver-error')!.hidden).toBe(false);
		expect(document.getElementById('sdr-receiver-error')!.title).toContain(error);
		const points = 32;
		const data = new ArrayBuffer(24 + points * 12 + 2 + 8);
		new Uint8Array(data, 0, 4).set([82, 84, 65, 70]);
		const header = new DataView(data);
		header.setUint32(4, 1, true);
		header.setUint32(8, points, true);
		header.setUint16(12, 1, true);
		header.setFloat64(16, 998437500, true);
		const freq = new Float64Array(data, 24, points);
		for (let i = 0; i < points; i++) freq[i] = 998437500 + i * 3.125e6 / (points - 1);
		new Float32Array(data, 24 + points * 8, points).fill(-90);
		header.setFloat64(data.byteLength - 8, 1001562500, true);
		socket.onmessage!({ data } as MessageEvent);
		expect(S.rtaData?.ver).toBe(1);
		header.setUint32(4, 2, true);
		new Float32Array(data, 24 + points * 8, points).fill(-80);
		socket.onmessage!({ data } as MessageEvent);
		expect(S.rtaData?.ver).toBe(2);
		expect(S.rtaData?.spec[16]).toBeCloseTo(-80);
		expect(S.statusError).toBeNull();
		expect(pendingGraphMode()).toBe('rta');
	});

	it('restarts a failed decoder only after explicit healthy status with fresh receiver identities', async () => {
		class Receiver {
			static instances: Receiver[] = [];
			onmessage: ((event: { data: { type: string } }) => void) | null = null;
			onerror: ((event: { message: string }) => void) | null = null;
			postMessage = vi.fn();
			terminate = vi.fn();
			constructor() { Receiver.instances.push(this); }
		}
		vi.stubGlobal('Worker', Receiver);
		await load();
		// These observers must share load()'s fresh module graph after resetModules().
		const S = await import('../core/store');
		const { sdrIqStats } = await import('../sdr/iqStream');
		document.body.insertAdjacentHTML('beforeend', '<span id="sdr-receiver-error" hidden></span><span id="cur-sdr-level"></span>');
		const receiverStatus = (id: number, error: string | null | undefined) => {
			const value = status('sdr');
			return { ...value, actual: { channel_registry: [{ channel: 1, channel_id: id, epoch: 9, state: 'active' }] },
				sdr: { ...value.sdr, receiver_error: error, level_dbfs: -25 } };
		};
		receive(receiverStatus(17, null));
		const worker = Receiver.instances.at(-1)!;
		const count = Receiver.instances.length;
		receive({ cmd: 'RECEIVER_ERROR', error: 'queue full', failed: true, scope: 'all_receivers' });
		expect(worker.terminate).toHaveBeenCalledOnce();
		receive(receiverStatus(17, 'queue full'));
		expect(document.getElementById('cur-sdr-level')!.textContent).toBe('--- dBFS');
		receive(receiverStatus(17, null));
		receive(receiverStatus(18, undefined));
		const inactive = receiverStatus(18, null);
		inactive.actual.channel_registry[0].state = 'retiring';
		receive(inactive);
		expect(Receiver.instances).toHaveLength(count);
		expect(sdrIqStats().receiverFailed).toBe(true);
		S.setStatusError('unrelated capture failure');
		receive(receiverStatus(18, null));
		expect(Receiver.instances).toHaveLength(count + 1);
		expect(sdrIqStats().receiverFailed).toBe(false);
		expect(document.getElementById('sdr-receiver-error')!.hidden).toBe(true);
		expect(S.statusError).toBe('unrelated capture failure');
		worker.onmessage!({ data: { type: 'connected' } });
		expect(sdrIqStats().pipelineReady).toBe(false);
	});
	it('keeps a crashed decode worker stopped until fresh healthy receiver identity arrives', async () => {
		class Receiver {
			static instances: Receiver[] = [];
			onmessage: ((event: { data: { type: string } }) => void) | null = null;
			onerror: ((event: { message: string }) => void) | null = null;
			postMessage = vi.fn();
			terminate = vi.fn();
			constructor() { Receiver.instances.push(this); }
		}
		vi.stubGlobal('Worker', Receiver);
		await load();
		const healthy = (id: number) => ({ ...status('sdr'), actual: { channel_registry: [{ channel: 1, channel_id: id, epoch: 9, state: 'active' }] },
			sdr: { ...status('sdr').sdr, receiver_error: null } });
		receive(healthy(17));
		const old = Receiver.instances.at(-1)!;
		const count = Receiver.instances.length;
		old.onerror!({ message: 'Worker stopped' });
		receive(healthy(17));
		receive(healthy(17));
		expect(Receiver.instances).toHaveLength(count);
		expect(old.terminate).toHaveBeenCalledOnce();
		receive(healthy(18));
		expect(Receiver.instances).toHaveLength(count + 1);
	});


	it.each(['ERROR', 'RECEIVER_ERROR'])('settles %s recovery failure without a delayed spectrum error or automatic retry', async cmd => {
		const { controls } = await load();
		// This observer must share load()'s fresh module graph after resetModules().
		const S = await import('../core/store');
		receive(status('sdr'));
		controls.selectSdrDemod('am');
		const request = socket.sent.filter(command => command.cmd === 'SET_SDR_CHANNEL' && command.request_id).at(-1)!;
		receive(cmd === 'ERROR' ? { cmd, code: 'receiver_retiring', msg: 'receiver still retiring',
			response_to: 'SET_SDR_CHANNEL', request_id: request.request_id }
			: { cmd, failed: true, scope: 'all_receivers', error: 'receiver queue full' });
		const sent = socket.sent.length;
		vi.advanceTimersByTime(4000);
		expect(S.statusError).toBeNull();
		expect(socket.sent).toHaveLength(sent);
	});

	it.each(['device STATUS', 'control websocket'])('clears the actual AM display on %s loss and ignores the retired worker until connected STATUS', async (loss) => {
		class Receiver {
			static instances: Receiver[] = [];
			onmessage: ((event: { data: { type: string; envelopes?: DecodeEnvelope[] } }) => void) | null = null;
			onerror: ((event: { message: string }) => void) | null = null;
			postMessage = vi.fn();
			terminate = vi.fn();
			constructor() { Receiver.instances.push(this); }
		}
		vi.stubGlobal('Worker', Receiver);
		document.body.insertAdjacentHTML('beforeend', `<button id="btn-decode-window"></button>
			<div id="decode-window"><span id="am-title-badge" role="status"></span><div id="am-empty"></div>
			<div class="decode-am-carrier"><span id="am-carrier"></span><label>Synchronous <button id="am-sync-toggle"></button></label></div>
			<span id="am-carrier-offset"></span><span id="am-modulation"></span><span id="am-input"></span>
			<span id="am-rms"></span><span id="am-peak"></span></div>`);
		const { controls } = await load();
		// A fresh module graph is necessary to exercise socket and worker ownership together.
		const ui = await import('../ui/decodeWindow');
		const { setLang } = await import('../core/i18n');
		setLang('en');
		controls.bindActions();
		const primary = { sdr: { center: 1e9, decimate: 32, channels: [{ channel: 1, enabled: true,
			freq: 1e9, mode: 'am', if_bw: 6000, am_sync: true, am_sync_generation: 0 }] } };
		receive(status('sdr', primary));
		if (!ui.decodeWindowOpen()) ui.toggleDecodeWindow();
		const oldWorker = Receiver.instances.at(-1)!;
		const envelope = (event: DecodeEnvelope['event'], seq = 0): DecodeEnvelope => ({ version: 1,
			channel: 1, channel_id: 17, epoch: 9, seq, input_index: '0', input_rate: 62500000,
			input_fraction_num: '0', input_fraction_den: '1', delay_seconds: 0, delay_known: true,
			causal_presentation: false, mode: 'am', event });
		const locked = envelope({ type: 'am-info', syncEnabled: true, syncLocked: true, amSyncGeneration: 0,
			carrierHz: 1_000_000_001.25, carrierOffsetHz: 1.25, modulationPct: 42, inputDbFs: -26, rmsDbFs: -32, peakDbFs: -24,
			scope: [-1, 0, 1] }, 1);
		const deliver = (worker: Receiver) => {
			worker.onmessage!({ data: { type: 'frames', envelopes: [envelope({ type: 'start' }), locked] } });
			ui.refreshDecodeWindow();
		};
		deliver(oldWorker);
		const toggle = document.getElementById('am-sync-toggle') as HTMLButtonElement;
		const badge = document.getElementById('am-title-badge')!;
		expect(badge.classList.contains('on')).toBe(true);
		expect(toggle.disabled).toBe(false);
		expect(document.getElementById('am-input')!.textContent).toBe('-26.0 dBFS');
		expect(document.getElementById('am-carrier')!.textContent).toBe('1000.000001 MHz');
		if (loss === 'device STATUS') receive(status('sdr', { ...primary, connected: false }));
		else socket.onclose!();
		expect(oldWorker.terminate).toHaveBeenCalledOnce();
		expect(badge.classList.contains('on')).toBe(false);
		expect(toggle.disabled).toBe(true);
		expect(document.getElementById('am-carrier')!.textContent).toBe('—');
		expect(document.getElementById('am-input')!.textContent).toBe('—');
		deliver(oldWorker);
		expect(badge.classList.contains('on')).toBe(false);
		expect(toggle.disabled).toBe(true);
		expect(document.getElementById('am-input')!.textContent).toBe('—');
		if (loss === 'control websocket') {
			vi.advanceTimersByTime(1000);
			socket.onopen!();
			expect(toggle.disabled).toBe(true);
			expect(Receiver.instances).toHaveLength(1);
		}
		receive(status('sdr', primary));
		expect(Receiver.instances).toHaveLength(2);
		expect(badge.classList.contains('on')).toBe(false);
		expect(toggle.disabled).toBe(false);
		deliver(oldWorker);
		expect(badge.classList.contains('on')).toBe(false);
		deliver(Receiver.instances.at(-1)!);
		expect(badge.classList.contains('on')).toBe(true);
		controls.disconnectSdrSession();
	});
});
