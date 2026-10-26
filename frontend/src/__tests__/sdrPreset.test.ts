/**
 * Preset cleanliness.
 *
 * Preset promises factory defaults. The persisted half of that promise lives in
 * resetSdrState(): resetAll('sdr') drops the in-memory intents, and the SDR_PREF_KEYS loop
 * removes the localStorage entries. A new SDR slot with a persistKey that is missing from
 * SDR_PREF_KEYS would survive every Preset and come back after a reload - exactly the bug
 * class the manual clearing had before the loop existed. These tests hold the line without
 * needing to read the slot's private options: they watch the storage itself.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
vi.mock('../core/wsSend', () => ({ send: vi.fn(), onCommandSent: vi.fn() }));
vi.mock('../audio/sdrAudio', () => ({
	prepareSdrAudioTransition: vi.fn(), setSdrAudioEnabled: vi.fn(), setSdrAudioMode: vi.fn(),
}));
import { allParams, resetAll } from '../core/params';
import * as S from '../core/store';
import { send } from '../core/wsSend';
import { updateStatus } from '../core/ws';
import { presetAll, syncPresetStatus } from '../ui/controls';
import { resetGraphMode } from '../ui/graphMode';
import { resetSdrState, sdrAmSync, sdrAudioOn, sdrCenterHz, sdrDecimate, sdrDemod, sdrFilterByMode, sdrIfbw, sdrListenHz } from '../ui/sdrState';
import { resetTraceFamilyModes, SDR_DEFAULT_AVG, setTraceAverage, setTraceMode } from '../ui/traceOps';
import { getView, isZoomed, setCapture, setView } from '../ui/spectrumViewport';
import { commitRefLevel } from '../ui/panels/refAmp';
import { getDisplayRef } from '../ui/displayRef';
import { getY } from '../render/plot';
import { appendRadiofaxRows, radiofaxStatus, setRadiofaxChannel } from '../sdr/radiofaxLog';

const sdrPersisted = (): string[] =>
	Object.keys(localStorage).filter((k) => k.startsWith('web-sa-sdr-'));

beforeEach(() => {
	vi.useFakeTimers();
	localStorage.clear();
	resetAll('sdr');
});

afterEach(() => {
	vi.clearAllTimers();
	vi.useRealTimers();
	document.body.replaceChildren();
});

describe('Preset clears the persisted SDR preferences', () => {
	it('clears retained fax paper only after its matching successful Preset acknowledgement', () => {
		setRadiofaxChannel(1);
		appendRadiofaxRows({ width: 1, row: 0, rows: 1, gray: Uint8Array.of(64), revision: 0 }, {
			serial: 'preset', epoch: 'meta', hz: 9_110_000, ioc: 576, lpm: 120, lines: 1, complete: false,
			partial: false, segmentIndex: 0, transmissionId: '1', missingRows: [], reason: null,
		}, 'preset-stream');
		presetAll();
		const request = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		expect(radiofaxStatus().pages).toHaveLength(1);
		syncPresetStatus('SET_PRESET', 'unrelated');
		expect(radiofaxStatus().pages).toHaveLength(1);
		syncPresetStatus('SET_PRESET', request.request_id);
		expect(radiofaxStatus().pages).toEqual([]);
		expect(radiofaxStatus().current).toBeNull();
		expect(radiofaxStatus().bytes).toBe(0);
	});
	it('every sdr-scope slot that persists leaves no storage behind', () => {
		// Write through every sdr slot the way the app does. Types differ per slot, so the
		// writes are deliberately untyped: the test only needs the persistence side effect.
		// set() persists the 'desired'-persisted slots, confirm() the backend-confirmed ones.
		const slots = allParams().filter((p) => p.scope === 'sdr');
		expect(slots.length).toBeGreaterThan(10); // the audit must see the real family
		for (const p of slots) {
			try { (p.set as unknown as (v: unknown) => void)(1); } catch { /* type rejects 1 */ }
			try { (p.confirm as unknown as (v: unknown) => boolean)(1); } catch { /* ditto */ }
		}
		resetSdrState();
		expect(sdrPersisted()).toEqual([]);
	});

	it('the per-mode step memories are covered by the reset', () => {
		localStorage.setItem('web-sa-sdr-step-normal', '{"am":2500}');
		localStorage.setItem('web-sa-sdr-step-coarse', '{"am":20000}');
		localStorage.setItem('web-sa-sdr-step-fine', '{"am":50}');
		localStorage.setItem('web-sa-sdr-locate', 'signal');
		localStorage.setItem('web-sa-sdr-channel-step', '12500');
		localStorage.setItem('web-sa-sdr-channel-anchor', '1000');
		localStorage.setItem('web-sa-sdr-step-bw', '{"256":2500}'); // the retired key
		resetSdrState();
		for (const key of ['web-sa-sdr-step-normal', 'web-sa-sdr-step-coarse', 'web-sa-sdr-step-fine',
			'web-sa-sdr-locate', 'web-sa-sdr-channel-step', 'web-sa-sdr-channel-anchor',
			'web-sa-sdr-step-bw']) {
			expect(localStorage.getItem(key)).toBeNull();
		}
	});

	it('clears confirmed per-mode Filter preferences and their in-memory values', () => {
		sdrFilterByMode.set({ cw: 900, am: 4500 });
		expect(localStorage.getItem('web-sa-sdr-filter-by-mode')).toBe('{"cw":900,"am":4500}');
		resetSdrState();
		expect(sdrFilterByMode.get()).toEqual({});
		expect(localStorage.getItem('web-sa-sdr-filter-by-mode')).toBeNull();
	});

	it('resets the accepted AM detector to the factory choice and removes its saved preference', () => {
		sdrAmSync.confirm(true);
		sdrAmSync.set(true);
		resetSdrState();
		expect(sdrAmSync.get()).toBe(false);
		expect(sdrAmSync.desiredValue()).toBe(false);
		expect(localStorage.getItem('web-sa-sdr-am-sync')).toBeNull();
		sdrAmSync.confirm(false);
		expect(sdrAmSync.confirmedValue()).toBe(false);
		expect(sdrAmSync.pending()).toBe(false);
	});

	it('storage the app does not own survives a Preset', () => {
		// Appearance and layout preferences (lang/theme/ui-scale/rail/panel) are not
		// measurement state: Preset must not touch them.
		localStorage.setItem('web-sa-theme', 'light');
		localStorage.setItem('web-sa-lang', 'zh');
		localStorage.setItem('web-sa-rail', 'collapsed');
		localStorage.setItem('web-sa-panel', 'hidden');
		resetSdrState();
		expect(localStorage.getItem('web-sa-theme')).toBe('light');
		expect(localStorage.getItem('web-sa-lang')).toBe('zh');
		expect(localStorage.getItem('web-sa-rail')).toBe('collapsed');
		expect(localStorage.getItem('web-sa-panel')).toBe('hidden');
	});

	it('accepts native SDR preset defaults without replaying old filters or changing raw Ref placement', () => {
		resetGraphMode();
		resetTraceFamilyModes();
		sdrAudioOn.set(false);
		S.setActiveTraceIdx(0);
		document.body.innerHTML = '<input id="input-offset"><input id="input-sdr-center"><input id="input-sdr-listen">' +
			'<select id="select-sdr-decimate"><option>16</option><option>64</option></select>' +
			'<select id="select-trace-mode"><option>CLEAR_WRITE</option><option>AVERAGE</option><option>MAX_HOLD</option></select>' +
			'<div id="trace-avg-row"><select id="select-trace-avg"><option>16</option><option>0</option></select></div>' +
			'<span id="trace-avg-status"></span>';
		// Native STATUS has sibling sdr and req.sdr objects; channel records use mode,
		// while both SDR objects report demod. Effective capture geometry is in sdr.actual.
		const status = (center: number, decimate: number, demod: string, if_bw: number) => {
			const request = { center, listen: center, decimate, demod, if_bw, ref: 0, ref_set: false };
			const span = 50e6 / decimate;
			return { cmd: 'STATUS', mode: 'sdr', connected: true, center, span, ref: 0,
				req: { swp: {}, rta: {}, sdr: request }, actual: {},
				sdr: { ...request, actual: { start: center - span / 2, stop: center + span / 2 },
					channels: [{ channel: 1, enabled: true, freq: center, mode: demod, if_bw,
						am_sync: false, am_sync_generation: 0 }] } };
		};
		updateStatus(status(14.2e6, 64, 'cw', 900));
		sdrFilterByMode.set({ cw: 900, am: 4500 });
		setTraceMode('AVERAGE');
		setTraceAverage(0);
		S.rtaAvgN.fill(87);
		S.rtaAvgSum.fill(new Float32Array([87]));
		setCapture({ lo: 14e6, hi: 14.4e6 });
		setView({ lo: 14.19e6, hi: 14.21e6 });
		commitRefLevel(-80);
		const oldY = getY(-65);
		presetAll();
		const preset = vi.mocked(send).mock.calls.at(-1)![0] as { request_id: string };
		updateStatus({ ...status(1e9, 16, 'am', 6000), response_to: 'SET_PRESET', request_id: preset.request_id });
		expect([sdrCenterHz.get(), sdrListenHz.get(), sdrDecimate.get(), sdrDemod.get(), sdrIfbw.get()])
			.toEqual([1e9, 1e9, 16, 'am', 6000]);
		expect(sdrFilterByMode.get()).toEqual({ am: 6000 });
		expect(JSON.parse(localStorage.getItem('web-sa-sdr-filter-by-mode')!)).toEqual({ am: 6000 });
		expect(localStorage.getItem('web-sa-sdr-demod')).toBe('am');
		expect(localStorage.getItem('web-sa-sdr-ifbw')).toBe('6000');
		expect(localStorage.getItem('web-sa-sdr-center')).toBe('1000000000');
		expect((document.getElementById('input-sdr-center') as HTMLInputElement).value).toBe('1000.000000');
		expect((document.getElementById('input-sdr-listen') as HTMLInputElement).value).toBe('1000.000000');
		expect(S.traces[0].mode).toBe('AVERAGE');
		expect(S.traces[0].avgTargetRta).toBe(SDR_DEFAULT_AVG);
		expect(S.rtaAvgN).toEqual([0, 0, 0, 0]);
		expect(S.rtaAvgSum).toEqual([null, null, null, null]);
		expect((document.getElementById('select-trace-mode') as HTMLSelectElement).value).toBe('AVERAGE');
		expect((document.getElementById('select-trace-avg') as HTMLSelectElement).value).toBe(String(SDR_DEFAULT_AVG));
		expect(document.getElementById('trace-avg-row')!.style.display).toBe('');
		expect(document.getElementById('trace-avg-status')!.textContent).toBe(String(SDR_DEFAULT_AVG));
		expect(isZoomed()).toBe(false);
		expect(getView()).toBeNull();
		expect(getDisplayRef()).toBe(-80);
		expect(getY(-65)).toBe(oldY);
	});
});
