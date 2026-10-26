import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DecodeEnvelope } from '../sdr/decodeProtocol';

/**
 * Radiofax ingress: base64 Gray8 rows become one stored page per
 * stream-epoch/serial/transmission/segment, and protocol-invalid frames stop the stream.
 *
 * The store module is imported fresh alongside iqStream so the per-channel state starts empty
 * and the two modules share one module registry per test.
 */

class Receiver {
	static current: Receiver;
	onmessage: ((event: { data: { type: string; envelopes?: DecodeEnvelope[]; message?: string } }) => void) | null = null;
	onerror: ((event: { message: string }) => void) | null = null;
	constructor() { Receiver.current = this; }
	postMessage(): void {}
	terminate(): void {}
}

const gray = (values: number[]): string => btoa(String.fromCharCode(...values));

function envelope(event: Record<string, unknown>, seq = 1): DecodeEnvelope {
	return { version: 1, channel: 1, channel_id: 17, epoch: 9, seq, input_index: '1',
		input_rate: 62_500_000, input_fraction_num: '0', input_fraction_den: '1',
		delay_seconds: 0.001, delay_known: true, causal_presentation: false, mode: 'radiofax',
		event: event as DecodeEnvelope['event'] };
}

function image(row: number, values: number[], overrides: Record<string, unknown> = {}): DecodeEnvelope {
	return envelope({ type: 'radiofax-image', width: 4, row, rows: values.length,
		gray: gray(values), revision: 0,
		meta: { serial: '7', epoch: 'me1', hz: 9_110_000, ioc: 576, lpm: 120, lines: 2,
			complete: false, partial: false, segmentIndex: 0, transmissionId: 'tx1' }, ...overrides });
}

async function load() {
	vi.resetModules();
	vi.stubGlobal('Worker', Receiver);
	const stream = await import('../sdr/iqStream');
	const store = await import('../sdr/radiofaxLog');
	return { stream, store };
}

beforeEach(() => {
	vi.useFakeTimers();
	document.body.innerHTML = '';
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.useRealTimers();
	document.body.innerHTML = '';
});

describe('radiofax ingress', () => {
	it('assembles base64 Gray8 rows into one page per segment', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'start' }, 0));
		stream.consumeDecode(image(0, [1, 2]));
		stream.consumeDecode(image(2, [3, 4]));
		const entry = store.radiofaxStatus().current!;
		expect(entry.picture.width).toBe(4);
		expect(entry.picture.height).toBe(4);
		expect(Array.from(entry.picture.gray)).toEqual([1, 2, 3, 4]);
		expect(entry.meta.hz).toBe(9_110_000);
		expect(entry.meta.transmissionId).toBe('tx1');
	});

	it('rejects malformed pixels and metadata instead of storing them', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'start' }, 0));
		const bad: DecodeEnvelope[] = [
			image(0, [1, 2], { gray: 'not base64!!' }),
			image(0, [1, 2], { gray: gray([1, 2, 3]) }),
			image(0, [1, 2], { width: 0 }),
			image(1, [1, 2], { revision: 5 }),
			image(0, [1, 2], { meta: { serial: '7', epoch: 'me1', hz: 1, ioc: 576, lpm: 0, lines: 1,
				complete: false, partial: false, segmentIndex: 0, transmissionId: 'tx1' } }),
		];
		for (const frame of bad) expect(() => stream.consumeDecode(frame)).toThrow();
		expect(store.radiofaxStatus().current).toBeNull();
	});

	it('separates stream epochs and segments into distinct pages', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'start' }, 0));
		stream.consumeDecode(image(0, [1]));
		stream.consumeDecode(image(0, [2], { meta: { serial: '7', epoch: 'me1', hz: 1, ioc: 576, lpm: 120,
			lines: 1, complete: false, partial: false, segmentIndex: 1, transmissionId: 'tx1' } }));
		const history = store.radiofaxStatus().history;
		expect(history).toHaveLength(1);
		expect(history[0].picture.gray[0]).toBe(1);
		expect(store.radiofaxStatus().current!.picture.gray[0]).toBe(2);
	});

	it('maps radiofax-state telemetry into the store verbatim', async () => {
		const { stream, store } = await load();
		stream.consumeDecode(envelope({ type: 'start' }, 0));
		stream.consumeDecode(envelope({ type: 'radiofax-state', running: true, stage: 'receiving',
			hasPicture: true, signalLevel: 0.4, carrierCoherence: 0.75, ioc: 576, lpm: 120,
			modulation: 'fm', polarity: 'normal', width: 1810, elapsedSeconds: 12.5,
			clockStatus: 'locked', clockSource: 'phasing', phasePixels: -0.5, clockPpm: 1.25,
			clockConfidence: 0.8, complete: false, partial: false, reason: null }));
		const decoder = store.radiofaxStatus().decoder!;
		expect(decoder.stage).toBe('receiving');
		expect(decoder.ioc).toBe(576);
		expect(decoder.modulation).toBe('fm');
		expect(decoder.clockSource).toBe('phasing');
		expect(decoder.clockPpm).toBe(1.25);
	});
});
