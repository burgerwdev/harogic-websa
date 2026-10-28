import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { RadiofaxPageMeta, RadiofaxRows } from '../sdr/radiofaxLog';
import type * as RadiofaxStore from '../sdr/radiofaxLog';
let store: typeof RadiofaxStore;
// A fresh module exercises independent browser sessions, including clear's live suppression.
const transmissions = new Map<string, string>();
const transmission = (label: string): string => {
	if (!transmissions.has(label)) transmissions.set(label, String(transmissions.size + 1));
	return transmissions.get(label)!;
};
beforeEach(async () => { transmissions.clear(); vi.resetModules(); store = await import('../sdr/radiofaxLog'); });
const meta = (overrides: Partial<RadiofaxPageMeta> = {}): RadiofaxPageMeta => ({
	serial: '1', epoch: 'meta1', hz: 9_110_000, ioc: 576, lpm: 120, lines: 1, complete: false,
	partial: false, segmentIndex: 0, missingRows: [], reason: null, ...overrides,
	transmissionId: transmission(overrides.transmissionId ?? 'tx1'),
});
const chunk = (row: number, values: number[], revision = 0): RadiofaxRows => ({
	width: 1, row, rows: values.length, gray: Uint8Array.from(values), revision,
});
function receive(tx: string, mib = 1, segmentIndex = 0): void {
	const width = 1024, rows = mib * 1024;
	store.appendRadiofaxRows({ width, row: 0, rows, gray: new Uint8Array(width * rows), revision: 0 },
		meta({ serial: `${tx}-${segmentIndex}`, transmissionId: tx, segmentIndex, lines: rows }), 'stream1');
}

describe('radiofax logical page retention', () => {
	it('groups sequential segments with different serials and retains more than three fragments', () => {
		for (let segmentIndex = 0; segmentIndex < 6; segmentIndex++) receive('long', 1, segmentIndex);
		expect(store.radiofaxStatus().pages).toHaveLength(1);
		expect(store.radiofaxVisiblePage()!.segments.map((entry) => entry.meta.segmentIndex)).toEqual([0, 1, 2, 3, 4, 5]);
		expect(store.radiofaxVisiblePage()!.lines).toBe(6 * 1024);
		expect(store.radiofaxStatus().evicted).toBe(0);
	});
	it('keeps original positions and cumulative missing rows through revisions', () => {
		store.appendRadiofaxRows(chunk(0, [1, 128]), meta({ lines: 2, missingRows: [1], reason: 'signal loss' }), 'stream1', 1, 123);
		store.appendRadiofaxRows(chunk(2, [3]), meta({ lines: 3, missingRows: [1] }), 'stream1');
		expect([...store.radiofaxVisible()!.picture.gray]).toEqual([1, 128, 3]);
		store.appendRadiofaxRows(chunk(0, [9, 128, 8], 1), meta({ lines: 3, missingRows: [1] }), 'stream1');
		expect(store.radiofaxVisible()!.meta.at).toBe(123);
		expect(store.radiofaxVisiblePage()!.missingRows).toEqual([1]);
		expect(() => store.appendRadiofaxRows(chunk(1, [4], 0), meta(), 'stream1')).toThrow(/revision/);
		expect(() => store.appendRadiofaxRows(chunk(5, [4], 1), meta(), 'stream1')).toThrow(/gap/);
	});
	it('updates old segment revisions without changing current, page order or pinned history', () => {
		receive('old');
		const old = store.radiofaxStatus().pages[0];
		receive('live');
		store.setRadiofaxView(old.id);
		const currentId = store.radiofaxStatus().current!.id;
		store.appendRadiofaxRows({ width: 1024, row: 0, rows: 1, gray: new Uint8Array(1024).fill(7), revision: 1 },
			meta({ serial: 'old-0', transmissionId: 'old' }), 'stream1');
		expect(store.radiofaxStatus().current!.id).toBe(currentId);
		expect(store.radiofaxStatus().pages.map((page) => page.segments[0].meta.transmissionId)).toEqual(['live', 'old'].map(transmission));
		expect(store.radiofaxStatus().viewId).toBe(old.id);
		expect(store.radiofaxVisible()!.picture.gray[0]).toBe(7);
		receive('new');
		expect(store.radiofaxVisiblePage()!.id).toBe(old.id);
		store.followRadiofaxLatest();
		expect(store.radiofaxVisible()!.meta.transmissionId).toBe(transmission('new'));
	});
	it('keeps the highest live segment current when an earlier snapshot segment arrives late', () => {
		receive('live', 1, 2);
		const currentId = store.radiofaxStatus().current!.id;
		receive('live', 1, 0);
		expect(store.radiofaxStatus().current!.id).toBe(currentId);
		expect(store.radiofaxVisiblePage()!.segments.map((entry) => entry.meta.segmentIndex)).toEqual([0, 2]);
	});
	it('evicts whole unprotected pages oldest first, protecting viewed and current pages', () => {
		receive('viewed', 8);
		store.setRadiofaxView(store.radiofaxStatus().pages[0].id);
		receive('old', 4);
		receive('live', 8);
		receive('live', 4, 1);
		expect(store.radiofaxStatus().pages.map((page) => page.segments[0].meta.transmissionId)).toEqual(['live', 'viewed'].map(transmission));
		expect(store.radiofaxStatus().pages[0].segments).toHaveLength(2);
		expect(store.radiofaxStatus().evicted).toBe(1);
		expect(store.radiofaxStatus().bytes).toBeLessThanOrEqual(store.RADIOFAX_CHANNEL_BYTES);
	});
	it('blocks protected growth without throwing, gaps, or oversized allocations', () => {
		receive('viewed', 8);
		store.setRadiofaxView(store.radiofaxStatus().pages[0].id);
		receive('live', 12);
		expect(() => receive('live', 1, 1)).not.toThrow();
		expect(store.radiofaxStatus().retention).toBe('blocked');
		expect(store.radiofaxStatus().retentionReason).toBe('memory limit');
		expect(store.radiofaxStatus().current!.meta.partial).toBe(true);
		expect(() => store.appendRadiofaxRows(chunk(20, [9]), meta({ serial: 'live-1', transmissionId: 'live', segmentIndex: 1 }), 'stream1')).not.toThrow();
		expect(store.radiofaxStatus().bytes).toBe(20 * 1024 * 1024);
		expect(store.radiofaxStatus().pages).toHaveLength(2);
	});
	it('retains received paper and pinned view on stream end without creating empty attempts', () => {
		store.endRadiofaxStream();
		expect(store.radiofaxStatus().pages).toEqual([]);
		receive('old');
		const old = store.radiofaxVisiblePage()!.id;
		store.setRadiofaxView(old);
		store.endRadiofaxStream();
		expect(store.radiofaxVisiblePage()!.id).toBe(old);
		expect(store.radiofaxVisible()!.meta.partial).toBe(true);
		expect(store.radiofaxVisible()!.meta.reason).toBe('stream ended');
		expect(store.radiofaxStatus().decoder).toBeNull();
		store.appendRadiofaxRows(chunk(0, [4]), meta(), 'stream2');
		expect(store.radiofaxStatus().pages).toHaveLength(2);
		expect(store.radiofaxVisiblePage()!.id).toBe(old);
	});
	it('preserves all received rows when a replacement revision is interrupted', () => {
		store.appendRadiofaxRows(chunk(0, [10, 20, 30, 40]), meta({ lines: 4 }), 'stream1');
		store.appendRadiofaxRows(chunk(0, [11, 21], 1), meta({ lines: 2, complete: true }), 'stream1');
		expect([...store.radiofaxVisible()!.picture.gray]).toEqual([11, 21, 30, 40]);
		expect(store.radiofaxVisiblePage()!.lines).toBe(4);
		store.endRadiofaxStream();
		expect([...store.radiofaxVisible()!.picture.gray]).toEqual([11, 21, 30, 40]);
		expect(store.radiofaxVisiblePage()!.complete).toBe(false);
		expect(store.radiofaxVisiblePage()!.partial).toBe(true);
		expect(store.radiofaxVisiblePage()!.reason).toBe('stream ended');
	});
	it('requires contiguous replacement rows despite retaining the older tail', () => {
		store.appendRadiofaxRows(chunk(0, [10, 20, 30, 40]), meta({ lines: 4 }), 'stream1');
		store.appendRadiofaxRows(chunk(0, [11], 1), meta(), 'stream1');
		expect(() => store.appendRadiofaxRows(chunk(3, [41], 1), meta({ lines: 4 }), 'stream1')).toThrow(/gap/);
		store.appendRadiofaxRows(chunk(1, [21, 31, 41], 1), meta({ lines: 4, complete: true }), 'stream1');
		store.endRadiofaxStream();
		expect([...store.radiofaxVisible()!.picture.gray]).toEqual([11, 21, 31, 41]);
		expect(store.radiofaxVisiblePage()!.complete).toBe(true);
		expect(store.radiofaxVisiblePage()!.partial).toBe(false);
	});
	it('replaces old missing-row metadata only as the revised rows arrive', () => {
		store.appendRadiofaxRows(chunk(0, [10, 20, 128, 40]), meta({ lines: 4, missingRows: [2] }), 'stream1');
		store.appendRadiofaxRows(chunk(0, [11], 1), meta(), 'stream1');
		expect(store.radiofaxVisiblePage()!.missingRows).toEqual([2]);
		store.appendRadiofaxRows(chunk(1, [21, 31, 41], 1), meta({ lines: 4, complete: true }), 'stream1');
		expect(store.radiofaxVisiblePage()!.missingRows).toEqual([]);
		expect([...store.radiofaxVisible()!.picture.gray]).toEqual([11, 21, 31, 41]);
	});
	it('deletes whole pages, clears dangling view and suppresses live rows until the next transmission', () => {
		receive('old');
		const old = store.radiofaxVisiblePage()!.id;
		receive('live');
		store.setRadiofaxView(old);
		store.deleteRadiofaxPage(old);
		expect(store.radiofaxStatus().viewId).toBeNull();
		store.deleteRadiofaxPage(store.radiofaxVisiblePage()!.id);
		expect(store.radiofaxStatus().current).toBeNull();
		expect(store.radiofaxStatus().bytes).toBe(0);
		expect(store.radiofaxStatus().currentSuppressed).toBe(true);
		expect(() => store.appendRadiofaxRows(chunk(99, [1]), meta({ transmissionId: 'live' }), 'stream1')).not.toThrow();
		expect(store.radiofaxStatus().pages).toEqual([]);
		receive('next');
		expect(store.radiofaxStatus().currentSuppressed).toBe(false);
		expect(store.radiofaxVisible()!.meta.transmissionId).toBe(transmission('next'));
		store.clearRadiofax();
		receive('next');
		expect(store.radiofaxStatus().pages).toEqual([]);
		receive('after-clear');
		expect(store.radiofaxStatus().pages).toHaveLength(1);
	});
	it('ignores late revisions of deleted and evicted transmissions without stealing current', () => {
		receive('old');
		receive('live');
		const currentId = store.radiofaxStatus().current!.id;
		store.deleteRadiofaxPage(store.radiofaxStatus().pages[1].id);
		store.appendRadiofaxRows({ width: 1024, row: 0, rows: 1, gray: new Uint8Array(1024).fill(9), revision: 2 },
			meta({ serial: 'old-0', transmissionId: 'old' }), 'stream1');
		expect(store.radiofaxStatus().pages).toHaveLength(1);
		expect(store.radiofaxStatus().current!.id).toBe(currentId);
		receive('after');
		expect(store.radiofaxVisible()!.meta.transmissionId).toBe(transmission('after'));
	});
	it.each(['delete', 'clear'] as const)('never resurrects %s paper after more than 64 newer pages', (action) => {
		store.appendRadiofaxRows(chunk(0, [1]), meta({ serial: 'old-0', transmissionId: 'old' }), 'stream1');
		if (action === 'delete') store.deleteRadiofaxPage(store.radiofaxVisiblePage()!.id);
		else store.clearRadiofax();
		for (let index = 0; index < 70; index++) {
			store.appendRadiofaxRows(chunk(0, [index]), meta({ serial: `new-${index}`, transmissionId: `new-${index}` }), 'stream1');
			if (store.radiofaxStatus().pages.length > 1) store.deleteRadiofaxPage(store.radiofaxStatus().pages[1].id);
		}
		const current = store.radiofaxStatus().current;
		const pageIds = store.radiofaxStatus().pages.map((page) => page.id);
		store.appendRadiofaxRows(chunk(0, [9], 3), meta({ serial: 'old-0', transmissionId: 'old' }), 'stream1');
		expect(store.radiofaxStatus().current).toBe(current);
		expect(store.radiofaxStatus().pages.map((page) => page.id)).toEqual(pageIds);
	});
	it('accepts a late initial snapshot revision on a fresh stream and preserves historical corrections', () => {
		store.appendRadiofaxRows(chunk(0, [1], 4), meta({ serial: 'old-0', transmissionId: 'old' }), 'stream1');
		store.appendRadiofaxRows(chunk(0, [2], 7), meta({ serial: 'live-2', transmissionId: 'live', segmentIndex: 2 }), 'stream2');
		const currentId = store.radiofaxStatus().current!.id;
		store.appendRadiofaxRows(chunk(0, [8], 8), meta({ serial: 'live-0', transmissionId: 'live' }), 'stream2');
		store.appendRadiofaxRows(chunk(0, [6], 9), meta({ serial: 'live-0', transmissionId: 'live' }), 'stream2');
		store.appendRadiofaxRows(chunk(0, [9], 5), meta({ serial: 'old-0', transmissionId: 'old' }), 'stream1');
		expect(store.radiofaxStatus().current!.id).toBe(currentId);
		expect(store.radiofaxStatus().pages[0].segments.map((entry) => entry.picture.gray[0])).toEqual([6, 2]);
		expect(store.radiofaxStatus().pages[1].segments[0].picture.gray[0]).toBe(9);
	});
	it('marks a refused new transmission before budget failure and preserves its watermark through clear', () => {
		receive('viewed', 8);
		store.setRadiofaxView(store.radiofaxVisiblePage()!.id);
		receive('live', 12);
		receive('blocked', 13);
		expect(store.radiofaxStatus().retention).toBe('blocked');
		store.clearRadiofax();
		store.appendRadiofaxRows(chunk(0, [9], 2), meta({ transmissionId: 'blocked' }), 'stream1');
		expect(store.radiofaxStatus().pages).toEqual([]);
		store.appendRadiofaxRows(chunk(0, [7], 3), meta({ transmissionId: 'blocked' }), 'fresh-stream');
		expect(store.radiofaxStatus().current!.picture.gray[0]).toBe(7);
	});
	it('requires canonical u64 strings from direct store callers and orders counters beyond safe integers', () => {
		for (const transmissionId of ['label', '', '01', '-1', '1.0', '18446744073709551616', 1]) {
			expect(() => store.appendRadiofaxRows(chunk(0, [1]), { ...meta(), transmissionId } as RadiofaxPageMeta, 'stream1')).toThrow(/transmission id/);
		}
		store.appendRadiofaxRows(chunk(0, [1]), { ...meta(), transmissionId: '9007199254740992' }, 'stream1');
		store.clearRadiofax();
		store.appendRadiofaxRows(chunk(0, [2], 3), { ...meta(), transmissionId: '9007199254740992' }, 'stream1');
		expect(store.radiofaxStatus().pages).toEqual([]);
		store.appendRadiofaxRows(chunk(0, [3], 4), { ...meta(), transmissionId: '9007199254740993' }, 'stream1');
		expect(store.radiofaxStatus().current!.meta.transmissionId).toBe('9007199254740993');
	});
	it('keeps channel budgets, selection and notifications independent', () => {
		const listener = vi.fn();
		const off = store.subscribeRadiofax(listener);
		receive('one');
		store.appendRadiofaxRows(chunk(0, [2]), meta(), 'other', 2);
		expect(listener).toHaveBeenCalledTimes(1);
		store.setRadiofaxChannel(2);
		expect(store.radiofaxVisible()!.picture.gray[0]).toBe(2);
		expect(store.radiofaxStatus().viewId).toBeNull();
		off();
	});
});
