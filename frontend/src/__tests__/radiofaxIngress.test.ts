import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DecodeEnvelope } from '../sdr/decodeProtocol';
const gray = (values: number[]): string => btoa(String.fromCharCode(...values));
function envelope(event: Record<string, unknown> & { type: string }, seq = 1): DecodeEnvelope {
	return { version: 1, channel: 1, channel_id: 17, epoch: 9, seq, input_index: '1',
		input_rate: 62_500_000, input_fraction_num: '0', input_fraction_den: '1', delay_seconds: 0.001,
		delay_known: true, causal_presentation: false, mode: 'radiofax', event: event as DecodeEnvelope['event'] };
}
const metadata = (extra: Record<string, unknown> = {}) => ({ serial: '7', epoch: 'me1', hz: 9_110_000,
	ioc: 576, lpm: 120, lines: 2, complete: false, partial: false, segmentIndex: 0,
	transmissionId: '1', missingRows: [], reason: null, ...extra });
function image(row: number, values: number[], overrides: Record<string, unknown> = {}): DecodeEnvelope {
	return envelope({ type: 'radiofax-image', width: 1, row, rows: values.length, gray: gray(values), revision: 0,
		meta: metadata({ lines: row + values.length }), ...overrides });
}
// These tests intentionally isolate stream/store module ownership to model fresh browser sessions.
async function load() {
	vi.resetModules();
	const stream = await import('../sdr/iqStream');
	const store = await import('../sdr/radiofaxLog');
	return { stream, store };
}
beforeEach(() => { vi.useFakeTimers(); document.body.innerHTML = ''; });
afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); document.body.innerHTML = ''; });

describe('radiofax ingress', () => {
	it('assembles Gray8 spans and preserves explicit missing positions and close reason', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'start' }, 0));
		stream.consumeDecode(image(0, [1, 128], { meta: metadata({ missingRows: [1], reason: 'signal loss' }) }));
		stream.consumeDecode(image(2, [3, 4], { meta: metadata({ lines: 4, missingRows: [1], partial: true, reason: 'signal loss' }) }));
		expect([...store.radiofaxVisible()!.picture.gray]).toEqual([1, 128, 3, 4]);
		expect(store.radiofaxVisible()!.meta).toMatchObject({ hz: 9_110_000, missingRows: [1], reason: 'signal loss', partial: true });
	});
	it('rejects malformed pixels, noncumulative row geometry and invalid required metadata', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'start' }, 0));
		const invalidMeta = [ { missingRows: undefined }, { reason: undefined }, { missingRows: [1, 1] },
			{ missingRows: [1, 0] }, { missingRows: [-1] }, { missingRows: [2] }, { missingRows: [0.5] },
			{ reason: 1 }, { lines: 3 }, { lines: 2401 }, { lpm: 0 },
			...['tx1', '', '01', '-1', '1.0', '18446744073709551616', 1, Number.MAX_SAFE_INTEGER + 1, null, undefined]
				.map((transmissionId) => ({ transmissionId })) ];
		for (const extra of invalidMeta) expect(() => stream.consumeDecode(image(0, [1, 2], { meta: metadata(extra) }))).toThrow();
		for (const extra of [{ gray: 'bad!' }, { gray: gray([1, 2, 3]) }, { width: 0 }, { revision: -1 }]) {
			expect(() => stream.consumeDecode(image(0, [1, 2], extra))).toThrow();
		}
		expect(store.radiofaxStatus().pages).toEqual([]);
	});
	it('accepts a fresh snapshot revision with the full u64 transmission counter', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'start' }, 0));
		stream.consumeDecode(image(0, [7], { revision: 5, meta: metadata({ lines: 1, transmissionId: '18446744073709551615' }) }));
		expect(store.radiofaxStatus().current!.revision).toBe(5);
		expect(store.radiofaxStatus().current!.meta.transmissionId).toBe('18446744073709551615');
	});
	it('rejects nonneutral missing pixels and masks that lose previously declared missing rows', async () => {
		const { stream, store } = await load();
		expect(() => stream.consumeDecode(image(0, [1, 2], { meta: metadata({ missingRows: [1] }) }))).toThrow(/missing row pixels/);
		stream.consumeDecode(image(0, [1, 128], { meta: metadata({ missingRows: [1] }) }));
		expect(() => stream.consumeDecode(image(2, [3], { meta: metadata({ lines: 3 }) }))).toThrow(/missing rows regressed/);
		expect([...store.radiofaxVisible()!.picture.gray]).toEqual([1, 128]);
	});
	it('groups different serial segments but isolates stream and metadata epochs', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'start' }, 0));
		stream.consumeDecode(image(0, [1]));
		stream.consumeDecode(image(0, [2], { meta: metadata({ serial: '8', lines: 1, segmentIndex: 1 }) }));
		expect(store.radiofaxStatus().pages).toHaveLength(1);
		expect(store.radiofaxVisiblePage()!.segments.map((entry) => entry.meta.serial)).toEqual(['7', '8']);
		stream.consumeDecode({ ...image(0, [3]), epoch: 10 });
		stream.consumeDecode(image(0, [4], { meta: metadata({ epoch: 'me2', lines: 1 }) }));
		expect(store.radiofaxStatus().pages).toHaveLength(3);
	});
	it('retains paper through reset, disconnect, mode and retune boundaries without merging epochs', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'start' }, 0));
		stream.consumeDecode(image(0, [1, 2]));
		const first = store.radiofaxVisiblePage()!.id;
		store.setRadiofaxView(first);
		stream.resetSdrIq();
		stream.setSdrIqEnabled(false);
		expect(store.radiofaxVisiblePage()!.id).toBe(first);
		expect(store.radiofaxVisible()!.meta.partial).toBe(true);
		stream.consumeDecode({ ...envelope({ type: 'start' }, 0), epoch: 10, mode: 'am' });
		expect(store.radiofaxStatus().pages).toHaveLength(1);
		stream.consumeDecode({ ...envelope({ type: 'start' }, 0), epoch: 11 });
		stream.consumeDecode({ ...image(0, [3]), epoch: 11 });
		expect(store.radiofaxStatus().pages).toHaveLength(2);
		expect(store.radiofaxVisiblePage()!.id).toBe(first);
	});
	it('maps receiver telemetry while keeping image reasons independent', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'radiofax-state', running: true, stage: 'degraded', hasPicture: true,
			signalLevel: 0.4, carrierCoherence: 0.75, ioc: 576, lpm: 120, modulation: 'fm', polarity: 'normal', width: 1810,
			lines: 25, elapsedSeconds: 12.5, clockStatus: 'degraded', clockSource: 'phasing', phasePixels: -0.5,
			clockPpm: 1.25, clockConfidence: 0.8, complete: false, partial: false, reason: 'signal loss' }));
		expect(store.radiofaxStatus().decoder).toMatchObject({ stage: 'degraded', reason: 'signal loss', clockPpm: 1.25 });
		expect(store.radiofaxStatus().pages).toEqual([]);
	});
});
