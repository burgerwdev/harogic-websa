import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DecodeEnvelope } from '../sdr/decodeProtocol';

vi.mock('../audio/sdrAudio', () => ({
	prepareSdrAudioTransition: vi.fn(), setSdrAudioEnabled: vi.fn(), setSdrAudioMode: vi.fn(),
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
	document.body.innerHTML = '<input id="input-offset"><button id="btn-mode-sdr"></button><button id="btn-mode-rta"></button>';
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

// Static imports cannot re-exercise preference restoration and firstConnect at a page reload.
async function load() {
	const ws = await import('../core/ws');
	const controls = await import('../ui/controls');
	const fit = await import('../ui/refAutoScale');
	ws.connectWS();
	socket.onopen!();
	return { ws, controls, fit };
}

function status(mode: string, response: Record<string, unknown> = {}) {
	return { cmd: 'STATUS', mode, connected: true, center: 1e9, span: 1e6, ref: 0,
		req: { swp: {}, rta: {} }, actual: {}, sdr: { center: 1e9, decimate: 32,
			listen: 1e9, demod: 'am', if_bw: 6000, ref: 0, ref_set: false }, ...response };
}

function receive(payload: Record<string, unknown>) {
	socket.onmessage!({ data: JSON.stringify(payload) } as MessageEvent);
}

describe('SDR entry and reload', () => {
	it('fits a fresh entry after tuning commands, then explicit Auto supersedes a stored level', async () => {
		const { controls, fit } = await load();
		receive(status('std'));
		controls.setGraphMode('sdr');
		receive(status('sdr'));
		fit.maybeRequestSdrFit();
		const entry = socket.sent.at(-1)!;
		expect(entry).toMatchObject({ cmd: 'AUTO_SCALE', display_only: true });
		receive(status('sdr', { response_to: 'AUTO_SCALE', request_id: entry.request_id,
			auto_ref: { result: 'applied', target: -85, last_peak: -65, last_noise_floor: -115, seq: 1 } }));
		const { getDisplayRef } = await import('../ui/displayRef');
		expect(getDisplayRef()).toBe(-85);

		fit.requestSdrEntryFit();
		fit.noteSdrRefState({ ref: -45, ref_set: true });
		const count = socket.sent.length;
		fit.maybeRequestSdrFit();
		expect(socket.sent).toHaveLength(count);
		fit.autoScaleRequest();
		const explicit = socket.sent.at(-1)!;
		expect(explicit.display_only).toBeUndefined();
		receive(status('sdr', { response_to: 'AUTO_SCALE', request_id: explicit.request_id,
			auto_ref: { result: 'applied', target: -75, last_peak: -55, last_noise_floor: -105, seq: 2 } }));
		expect(getDisplayRef()).toBe(-75);
		expect(JSON.parse(document.getElementById('spectrum')!.dataset.sdrRefDbg ?? '{}')).toMatchObject({ seq: 2, result: 'applied', shown: -75, peak: -55 });
	});

	it.each(['sdr', 'rta', 'std'])('keeps confirmed %s through Preset and a fresh websocket startup', async (mode) => {
		const { controls } = await load();
		receive(status(mode));
		controls.presetAll();
		const preset = socket.sent.at(-1)!;
		receive(status(mode, { response_to: 'SET_PRESET', request_id: preset.request_id }));
		expect(localStorage.getItem('web-sa-mode')).toBe(mode);

		// Module reload is the page boundary: onopen must select the same mode without an
		// artificial storage seed, using the state that the acknowledged Preset left behind.
		vi.clearAllTimers();
		vi.resetModules();
		await load();
		expect(socket.sent.filter(command => command.cmd === 'SET_MODE')).toEqual([{ cmd: 'SET_MODE', mode }]);
		receive(status(mode));
		const { currentGraphMode } = await import('../ui/graphMode');
		expect(currentGraphMode()).toBe(mode);
		expect(document.body.dataset.deviceMode).toBe(mode);
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
		expect(socket.sent.filter(command => 'am_sync' in command)).toEqual([
			{ cmd: 'SET_SDR_CHANNEL', channel: 1, am_sync: enabled, request_id: expect.any(String) },
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
		const alert = vi.spyOn(window, 'alert').mockImplementation(() => {});
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
		alert.mockRestore();
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
