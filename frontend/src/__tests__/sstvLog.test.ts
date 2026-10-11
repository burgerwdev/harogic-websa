/**
 * The SSTV log's contract: one transmission in place while it grows, a new one when the decoder's
 * serial moves, a capped history, and a view selector that survives the incoming pictures.
 *
 * The distinction the log owns is the one a progressive decoder makes necessary: the decoder reports
 * the same picture every few seconds, so "the picture grew" must not read as "a new picture arrived".
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	clearSstv,
	deleteSstvAll,
	deleteSstvMode,
	deleteSstvSelected,
	setSstvDecoderState,
	setSstvModeFilter,
	setSstvSelected,
	sstvActiveSet,
	sstvModeCounts,
	setSstvView,
	sstvCount,
	sstvStatus,
	sstvVisible,
	SSTV_HISTORY_MAX,
	subscribeSstv,
	updateSstvPicture,
	type SstvPicture,
} from '../sdr/sstvLog';

function picture(marker: number): SstvPicture {
	return { rgb: new Uint8Array([marker, marker, marker]), width: 2, height: 2 };
}

const metaFor = (serial: number, modeIndex: number) => ({
	name: 'Robot 36', modeIndex, vis: 8, width: 320, height: 240,
	lines: 120, progress: 50, complete: false, serial, hz: 7_171_000,
});

function meta(serial: number, over: Partial<Parameters<typeof updateSstvPicture>[1]> = {}) {
	return {
		name: 'Robot 36', modeIndex: 5, vis: 8, width: 320, height: 240,
		lines: 120, progress: 50, complete: false, serial, hz: 7_171_000, ...over,
	};
}

beforeEach(() => {
	clearSstv();
});

describe('the SSTV log', () => {
	it('updates the same transmission in place, keeping its first sighting', () => {
		updateSstvPicture(picture(1), meta(1));
		const first = sstvStatus().current!;
		expect(sstvCount()).toBe(1);
		expect(sstvStatus().history).toHaveLength(0);

		// Same serial: the decoder re-read its buffer and the picture grew.
		vi.spyOn(Date, 'now').mockReturnValue(first.meta.at + 5_000);
		updateSstvPicture(picture(2), meta(1, { lines: 240, progress: 100, complete: true }));
		const grown = sstvStatus().current!;
		expect(sstvCount()).toBe(1);
		expect(grown.picture.rgb[0]).toBe(2);
		expect(grown.meta.progress).toBe(100);
		// The timestamp is the transmission's start, not the last update, and the frequency is the one
		// it was first tuned to.
		expect(grown.meta.at).toBe(first.meta.at);
		expect(grown.meta.hz).toBe(first.meta.hz);
		vi.restoreAllMocks();
	});

	it('moves a finished transmission into the history when a new serial arrives', () => {
		updateSstvPicture(picture(1), meta(1, { complete: true, progress: 100 }));
		vi.spyOn(Date, 'now').mockReturnValue(1_700_000_000_000);
		updateSstvPicture(picture(2), meta(2));
		expect(sstvStatus().history).toHaveLength(1);
		expect(sstvStatus().history[0].picture.rgb[0]).toBe(1);
		expect(sstvStatus().current!.picture.rgb[0]).toBe(2);
		expect(sstvCount()).toBe(2);
		vi.restoreAllMocks();
	});

	it('caps the history and keeps the newest pictures', () => {
		for (let serial = 1; serial <= SSTV_HISTORY_MAX + 4; serial++) {
			updateSstvPicture(picture(serial), meta(serial, { complete: true, progress: 100 }));
		}
		const status = sstvStatus();
		expect(status.history).toHaveLength(SSTV_HISTORY_MAX);
		// Newest first: the picture before the current one is at the head of the history.
		expect(status.history[0].picture.rgb[0]).toBe(SSTV_HISTORY_MAX + 3);
		expect(sstvCount()).toBe(SSTV_HISTORY_MAX + 1);
	});

	it('shows a history entry until the operator goes back to the live picture', () => {
		updateSstvPicture(picture(1), meta(1, { complete: true, progress: 100 }));
		updateSstvPicture(picture(2), meta(2));
		expect(sstvVisible()!.picture.rgb[0]).toBe(2);
		const viewedId = sstvStatus().history[0].id;
		setSstvView(viewedId);
		expect(sstvVisible()!.picture.rgb[0]).toBe(1);
		// A new transmission while a history entry is viewed must not yank the view.
		updateSstvPicture(picture(3), meta(3));
		expect(sstvVisible()!.id).toBe(viewedId);
		setSstvView(null);
		expect(sstvVisible()!.picture.rgb[0]).toBe(3);
	});

	it('notifies subscribers and clears everything on request', () => {
		const listener = vi.fn();
		const off = subscribeSstv(listener);
		updateSstvPicture(picture(1), meta(1));
		expect(listener).toHaveBeenCalledTimes(1);
		clearSstv();
		expect(listener).toHaveBeenCalledTimes(2);
		expect(sstvStatus()).toMatchObject({ current: null, history: [], viewId: null });
		// An empty log is already clear: no second notification for a no-op.
		clearSstv();
		expect(listener).toHaveBeenCalledTimes(2);
		off();
		updateSstvPicture(picture(2), meta(2));
		expect(listener).toHaveBeenCalledTimes(2);
	});
});

describe('the decoder state the log keeps for the empty pane', () => {
	it('records the counters and notifies only when they change', () => {
		const listener = vi.fn();
		const off = subscribeSstv(listener);
		setSstvDecoderState({ running: true, pushes: 5, attempts: 0, bufferedSamples: 8_000, bufferedSeconds: 1, hasPicture: false, level: 0.3, srcLevel: 0.2 });
		expect(listener).toHaveBeenCalledTimes(1);
		expect(sstvStatus().decoder?.bufferedSeconds).toBe(1);
		// The worker re-reports the same state on every stats round; a re-render per round would repaint
		// the pane for nothing.
		setSstvDecoderState({ running: true, pushes: 5, attempts: 0, bufferedSamples: 8_000, bufferedSeconds: 1, hasPicture: false, level: 0.3, srcLevel: 0.2 });
		expect(listener).toHaveBeenCalledTimes(1);
		setSstvDecoderState({ running: true, pushes: 5, attempts: 1, bufferedSamples: 24_000, bufferedSeconds: 3, hasPicture: false, level: 0.3, srcLevel: 0.2 });
		expect(listener).toHaveBeenCalledTimes(2);
		expect(sstvStatus().decoder?.attempts).toBe(1);
		off();
	});

	it('is cleared with the pictures', () => {
		setSstvDecoderState({ running: true, pushes: 5, attempts: 2, bufferedSamples: 4_000, bufferedSeconds: 0.5, hasPicture: false, level: 0.3, srcLevel: 0.2 });
		updateSstvPicture(picture(1), meta(1));
		clearSstv();
		expect(sstvStatus().decoder).toBe(null);
		expect(sstvStatus().current).toBe(null);
	});
});

describe('the per-mode picture sets', () => {
	it('counts the stored pictures per mode', () => {
		updateSstvPicture(picture(1), meta(1, { modeIndex: 5 }));
		updateSstvPicture(picture(2), meta(2, { modeIndex: 5 }));
		updateSstvPicture(picture(3), meta(3, { modeIndex: 0 }));
		const counts = sstvModeCounts();
		expect(counts.get(5)).toBe(2);
		expect(counts.get(0)).toBe(1);
	});

	it('filters the active mode without touching the stored order', () => {
		updateSstvPicture(picture(1), metaFor(1, 5));
		updateSstvPicture(picture(2), meta(2, { modeIndex: 0 }));
		setSstvModeFilter(5);
		const set = sstvActiveSet();
		expect(set).toHaveLength(1);
		expect(set[0].meta.modeIndex).toBe(5);
		// Back to the live set: every picture, newest first.
		setSstvModeFilter(null);
		expect(sstvActiveSet()).toHaveLength(2);
	});

	it('deletes the ticked entries and keeps the rest', () => {
		updateSstvPicture(picture(1), metaFor(1, 5));
		updateSstvPicture(picture(2), meta(2, { modeIndex: 5 }));
		const victim = sstvStatus().history[0].id;
		setSstvSelected(victim, true);
		deleteSstvSelected();
		expect(sstvStatus().history).toHaveLength(0);
		expect(sstvStatus().current!.id).not.toBe(victim);
	});

	it('deletes one mode entirely, keeping other modes', () => {
		updateSstvPicture(picture(1), metaFor(1, 5));
		updateSstvPicture(picture(2), metaFor(2, 0));
		deleteSstvMode(5);
		expect(sstvStatus().current!.meta.modeIndex).toBe(0);
		expect(sstvCount()).toBe(1);
	});

	it('deletes everything', () => {
		updateSstvPicture(picture(1), metaFor(1, 5));
		updateSstvPicture(picture(2), metaFor(2, 0));
		deleteSstvAll();
		expect(sstvCount()).toBe(0);
	});

	
})
