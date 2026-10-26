import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	appendRadiofaxRows,
	clearRadiofax,
	endRadiofaxStream,
	followRadiofaxLatest,
	radiofaxStatus,
	radiofaxVisible,
	RADIOFAX_CHANNEL_BYTES,
	RADIOFAX_HISTORY_MAX,
	setRadiofaxChannel,
	setRadiofaxView,
	subscribeRadiofax,
} from '../sdr/radiofaxLog';

interface Chunk {
	width: number;
	row: number;
	rows: number;
	gray: Uint8Array;
	revision: number;
}

const meta = (overrides: Record<string, unknown> = {}) => ({
	serial: '7', epoch: 'e1', hz: 9_110_000, ioc: 576, lpm: 120, lines: 4, complete: false,
	partial: false, segmentIndex: 0, transmissionId: 'tx1', ...overrides,
});
const chunk = (row: number, values: number[], revision = 0, width = 1): Chunk => ({
	width, row, rows: values.length, gray: Uint8Array.from(values), revision,
});

beforeEach(() => {
	vi.useFakeTimers();
	vi.setSystemTime(new Date('2026-10-26T12:00:00Z'));
	for (const channel of [1, 2, 3] as const) clearRadiofax(channel);
	setRadiofaxChannel(1);
});

afterEach(() => vi.useRealTimers());

describe('radiofax page store', () => {
	it('appends rows of one segment in order and keeps the first sighting', () => {
		appendRadiofaxRows(chunk(0, [1, 2]), meta(), 'id1');
		appendRadiofaxRows(chunk(2, [3, 4]), meta(), 'id1');
		const entry = radiofaxStatus().current!;
		expect(entry.picture.height).toBe(4);
		expect(Array.from(entry.picture.gray)).toEqual([1, 2, 3, 4]);
		expect(entry.meta.at).toBe(Date.now());
		expect(entry.revision).toBe(0);
	});

	it('replaces the same page on a newer revision and rejects stale revisions', () => {
		appendRadiofaxRows(chunk(0, [1, 2]), meta(), 'id1');
		appendRadiofaxRows(chunk(0, [9], 1), meta(), 'id1');
		const entry = radiofaxStatus().current!;
		expect(entry.picture.height).toBe(1);
		expect(entry.picture.gray[0]).toBe(9);
		expect(entry.revision).toBe(1);
		expect(() => appendRadiofaxRows(chunk(1, [5], 0), meta(), 'id1')).toThrow(/revision/);
	});

	it('marks interrupted pages partial and resumes the source revision after reconnect', () => {
		appendRadiofaxRows(chunk(0, [1, 2], 5), meta(), 'id1');
		endRadiofaxStream();
		expect(radiofaxStatus().current!.meta.partial).toBe(true);
		expect(radiofaxStatus().decoder).toBeNull();
		appendRadiofaxRows(chunk(2, [3], 5), meta(), 'id1');
		expect(Array.from(radiofaxStatus().current!.picture.gray)).toEqual([1, 2, 3]);
		expect(radiofaxStatus().current!.meta.partial).toBe(false);
		appendRadiofaxRows(chunk(0, [4], 6), meta({ complete: true }), 'id1');
		endRadiofaxStream();
		expect(radiofaxStatus().current!.meta.complete).toBe(true);
		expect(radiofaxStatus().current!.meta.partial).toBe(false);
	});

	it('rejects row gaps and a replacement that starts beyond row zero', () => {
		// A page with nothing yet: any nonzero start is a gap first, then a replacement check.
		expect(() => appendRadiofaxRows(chunk(3, [1]), meta(), 'id1')).toThrow(/row gap|first rows/);
		appendRadiofaxRows(chunk(0, [1]), meta(), 'id1');
		expect(() => appendRadiofaxRows(chunk(2, [2]), meta(), 'id1')).toThrow(/row gap/);
		// New revision restarts the page, so its rows must begin at zero again.
		expect(() => appendRadiofaxRows(chunk(1, [3], 5), meta(), 'id1')).toThrow(/row gap/);
	});

	it('separates pages by serial, segment and both stream/metadata epochs', () => {
		appendRadiofaxRows(chunk(0, [1]), meta(), 'id1');
		appendRadiofaxRows(chunk(0, [2]), meta({ serial: '8' }), 'id1');
		appendRadiofaxRows(chunk(0, [3]), meta({ segmentIndex: 1 }), 'id1');
		appendRadiofaxRows(chunk(0, [4]), meta(), 'id2');
		appendRadiofaxRows(chunk(0, [5]), meta({ epoch: 'e2' }), 'id1');
		const history = radiofaxStatus().history;
  expect(history).toHaveLength(RADIOFAX_HISTORY_MAX);
  expect(radiofaxStatus().current!.picture.gray[0]).toBe(5);
  expect(history.map((entry) => entry.picture.gray[0])).toEqual([4, 3, 2]);
	});

	it('caps history at three pages and the per-channel byte budget', () => {
		for (let page = 0; page <= RADIOFAX_HISTORY_MAX; page++) {
			appendRadiofaxRows(chunk(0, [page]), meta({ serial: String(page) }), 'id1');
		}
		// The first-seen page keeps its place: history is capped, not reordered.
		expect(radiofaxStatus().history).toHaveLength(RADIOFAX_HISTORY_MAX);
		expect(radiofaxStatus().current!.picture.gray[0]).toBe(RADIOFAX_HISTORY_MAX);
		// One page wider than the whole per-channel budget must be refused outright.
		clearRadiofax();
		const width = 1024, rows = Math.ceil(RADIOFAX_CHANNEL_BYTES / width) + 2;
		expect(() => appendRadiofaxRows({ width, row: 0, rows, gray: new Uint8Array(width * rows), revision: 0 },
			meta({ serial: 'big' }), 'id1')).toThrow(/memory limit/);
		// Fitting pages stay, and their stored bytes never exceed the budget.
		clearRadiofax();
		for (let page = 0; page < 6; page++) {
			appendRadiofaxRows({ width, row: 0, rows: 512, gray: new Uint8Array(width * 512), revision: 0 },
				meta({ serial: String(page) }), 'id1');
		}
		expect(radiofaxStatus().history).toHaveLength(RADIOFAX_HISTORY_MAX);
		expect(radiofaxStatus().bytes).toBeLessThanOrEqual(RADIOFAX_CHANNEL_BYTES);
	});

	it('isolates channels and notifies only the selected one', () => {
		const notified = vi.fn();
		const off = subscribeRadiofax(notified);
		appendRadiofaxRows(chunk(0, [1]), meta(), 'id1', 1);
		expect(notified).toHaveBeenCalledTimes(1);
		appendRadiofaxRows(chunk(0, [2]), meta({ serial: '2' }), 'id2', 2);
		expect(notified).toHaveBeenCalledTimes(1);
		expect(radiofaxStatus().current!.picture.gray[0]).toBe(1);
		setRadiofaxChannel(2);
		expect(radiofaxStatus().current!.picture.gray[0]).toBe(2);
		expect(radiofaxStatus().history).toHaveLength(0);
		setRadiofaxChannel(1);
		off();
	});

	it('pins a viewed history page and resumes following the latest on demand', () => {
		appendRadiofaxRows(chunk(0, [1]), meta(), 'id1');
		appendRadiofaxRows(chunk(0, [2]), meta({ serial: '8' }), 'id1');
		const first = radiofaxStatus().history[0];
		setRadiofaxView(first.id);
		expect(radiofaxVisible()!.picture.gray[0]).toBe(1);
		appendRadiofaxRows(chunk(0, [3]), meta({ serial: '9' }), 'id1');
		expect(radiofaxVisible()!.picture.gray[0]).toBe(1);
		followRadiofaxLatest();
		expect(radiofaxVisible()!.picture.gray[0]).toBe(3);
		expect(radiofaxStatus().viewId).toBeNull();
	});
});
