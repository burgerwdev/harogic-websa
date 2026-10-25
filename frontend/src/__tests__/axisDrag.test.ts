/**
 * Axis-label gestures: the hit test, the preview mapping and the commit policy.
 *
 * The gesture exists to make the device round trip invisible. These tests pin the three things
 * that make that true instead of merely plausible:
 *
 *   1. nothing is sent while the pointer moves (one request per gesture, not one per pixel),
 *   2. the content follows the finger (dragging the frequency row right shows lower frequencies,
 *      dragging the level labels down raises the reference) and a wheel zoom keeps the value under
 *      the pointer where it is,
 *   3. the preview is dropped when the drawn data catches up, not on release - that is what keeps
 *      the trace from snapping back to the old window while the device reconfigures (0.3-1 s for a
 *      frequency change, ~1.9 s for a Ref change on the bench).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { setWS } from '../core/wsSend';
import { graphMode, resetGraphMode } from '../ui/graphMode';
import { resetFrequencyEditors } from '../ui/panels/frequency';
import { confirmRefLevel, effectiveRefLevel, refLevel } from '../ui/refState';
import { getDisplayRef, noteDisplayRefReport, setDisplayRef } from '../ui/displayRef';
import { displayOffset, displayUnit } from '../ui/displayState';
import { commitLevelOffset, rejectManualRef } from '../ui/panels/refAmp';
import { getY } from '../render/plot';
import {
	IDLE_COMMIT_MS,
	axisBandAt,
	axisDragEnd,
	axisDragMove,
	axisDragStart,
	axisDragging,
	axisWheel,
	confirmAxisRef,
	confirmSdrAxisWindow,
	previewFreqWindow,
	previewLevelWindow,
	rejectAxisRef,
	resetAxisPreview,
	xAxisTransform,
	yAxisTransform,
} from '../ui/axisDrag';

const RECT = { x: 10, y: 14, w: 800, h: 400 };
/** A canvas point just inside the frequency row / the level labels. */
const IN_X_BAND = { x: 200, y: RECT.y + RECT.h + 10 };
const IN_Y_BAND = { x: RECT.x + RECT.w + 20, y: 200 };

const sent: any[] = [];

function openSocket() {
	sent.length = 0;
	setWS({ readyState: WebSocket.OPEN, send: (m: string) => sent.push(JSON.parse(m)) } as any);
}

/** A committed swept frame: the window the data on screen was measured in. */
function sweptFrame(lo: number, hi: number) {
	S.setFreqArray(new Float64Array([lo, hi]));
}

beforeEach(() => {
	document.body.replaceChildren();
	resetAll();
	resetGraphMode();
	graphMode.confirm('std');    // resetGraphMode clears intent, not the last confirmed SDR mode
	resetAxisPreview();          // no gesture survives a test, including the CHAIN window
	openSocket();
	displayUnit.set('dBm');
	displayOffset.set(0);
	S.setDbPerDiv(10);
	refLevel.set(-20);
	setDisplayRef('preset', -20);
	S.setConfigVersion(0);
	sweptFrame(100e6, 200e6);
});

afterEach(() => {
	resetFrequencyEditors();
	resetAxisPreview();
	vi.useRealTimers();
});

describe('the axis bands', () => {
	it('is the frequency row under the graticule and the level labels to its right', () => {
		expect(axisBandAt(IN_X_BAND.x, IN_X_BAND.y, RECT)).toBe('x');
		expect(axisBandAt(IN_Y_BAND.x, IN_Y_BAND.y, RECT)).toBe('y');
		expect(axisBandAt(400, 200, RECT)).toBeNull();              // inside the plot
		expect(axisBandAt(400, RECT.y + RECT.h, RECT)).toBeNull();  // the graticule edge itself
		expect(axisBandAt(5, 5, RECT)).toBeNull();                  // the top-left margin
	});

	it('gives the corner to the frequency row', () => {
		expect(axisBandAt(IN_Y_BAND.x, IN_X_BAND.y, RECT)).toBe('x');
	});
});

describe('dragging the frequency axis', () => {
	it('pans the window with the finger and sends one request on release', () => {
		expect(axisDragStart('x', 400, IN_X_BAND.y, RECT)).toBe(true);
		axisDragMove(500, IN_X_BAND.y);                       // 100 px of 800 = 1/8 of the span
		expect(sent).toEqual([]);                             // moving sends nothing at all
		const t = xAxisTransform()!;
		expect(t.sx).toBeCloseTo(1, 9);                       // a pan does not rescale
		expect(t.dxFrac).toBeCloseTo(0.125, 9);               // ...it slides the trace right
		expect(previewFreqWindow()).toEqual({ lo: 87.5e6, hi: 187.5e6 });
		axisDragEnd();
		expect(sent).toEqual([{ cmd: 'SET_FREQ', center: 137.5e6, span: 100e6, request_id: expect.any(String) }]);
	});

	it('keeps the preview until the drawn frame matches the request, then drops it', () => {
		axisDragStart('x', 400, IN_X_BAND.y, RECT);
		axisDragMove(500, IN_X_BAND.y);
		axisDragEnd();
		// The request is in flight: the old frame is still on screen and stays where the gesture
		// put it (this is the difference between a preview and a snap-back).
		expect(xAxisTransform()).not.toBeNull();
		sweptFrame(87.5e6, 187.5e6);                          // the device confirms the window
		expect(xAxisTransform()).toBeNull();
		expect(previewFreqWindow()).toBeNull();
	});

	it('sends nothing when the pointer did not move', () => {
		axisDragStart('x', 400, IN_X_BAND.y, RECT);
		axisDragEnd();
		expect(sent).toEqual([]);
	});

	it('zooms the span around the pointer, keeping the frequency under it', () => {
		axisDragStart('x', IN_X_BAND.x, IN_X_BAND.y, RECT);
		axisWheel('x', -100, IN_X_BAND.x, IN_X_BAND.y, RECT);   // wheel up = zoom in
		const anchorFrac = (IN_X_BAND.x - RECT.x) / RECT.w;
		const t = xAxisTransform()!;
		expect(t.sx).toBeCloseTo(Math.exp(0.15), 6);            // the span shrank by exp(-0.15)
		// Where the anchor frequency was, is where it still is.
		expect(RECT.x + (anchorFrac * t.sx + t.dxFrac) * RECT.w).toBeCloseTo(IN_X_BAND.x, 6);
	});

	it('does not leave a wheel gesture holding the pointer state', () => {
		// Reported while reviewing: a wheel session was created with the pointer marked down, so
		// after a zoom the canvas kept panning as the mouse merely hovered (and the release that
		// belonged to an in-plot gesture was swallowed). A wheel has no pointer state at all.
		axisWheel('x', -100, IN_X_BAND.x, IN_X_BAND.y, RECT);
		expect(axisDragging()).toBe(false);
		const zoomed = previewFreqWindow();
		axisDragMove(IN_X_BAND.x + 200, IN_X_BAND.y);           // a hover must not pan
		expect(previewFreqWindow()).toEqual(zoomed);
		expect(sent).toEqual([]);                                // ...and the hover sends nothing
	});

	it('commits a wheel zoom once, with no pointer state involved', () => {
		// A wheel has no release to wait for: the stillness timer IS the end of the gesture.
		vi.useFakeTimers();
		axisWheel('x', -100, IN_X_BAND.x, IN_X_BAND.y, RECT);
		vi.advanceTimersByTime(IDLE_COMMIT_MS + 1);
		expect(sent.filter((m) => m.cmd === 'SET_FREQ')).toHaveLength(1);
		expect(axisDragging()).toBe(false);
	});

	it('snaps an RTA zoom to a span the hardware actually offers', () => {
		document.body.innerHTML =
			'<div id="rta-freq-settings"></div>' +
			'<select id="select-rta-span"><option value="12700000">12.7M</option>' +
			'<option value="6347656">6.35M</option><option value="3173828">3.17M</option></select>';
		graphMode.confirm('rta');
		S.setRtaData({ freq: new Float64Array([100e6, 200e6]), startHz: 100e6, stopHz: 200e6 });
		axisDragStart('x', 400, IN_X_BAND.y, RECT);
		axisWheel('x', 100, 400, IN_X_BAND.y, RECT);            // wheel down = zoom out
		axisDragEnd();
		const cmd = sent.find((m) => m.cmd === 'SET_RTA');
		expect(cmd).toBeTruthy();
		expect([12700000, 6347656, 3173828]).toContain(cmd.span);
		expect((document.getElementById('select-rta-span') as HTMLSelectElement).value)
			.toBe(String(cmd.span));
	});

	it('rebases an SDR gesture onto the accepted passband center and continues from it', () => {
		document.body.innerHTML = '<select id="select-sdr-decimate"><option value="16">3.125M</option></select>';
		graphMode.confirm('sdr');
		const span = 3_125_000;
		const center = 100e6;
		S.setRtaData({ freq: new Float64Array([center - span / 2, center + span / 2]),
			startHz: center - span / 2, stopHz: center + span / 2 });
		axisDragStart('x', 400, IN_X_BAND.y, RECT);
		axisDragMove(500, IN_X_BAND.y);
		axisDragEnd();
		const requested = sent[0].center;
		const accepted = requested + 127_000;
		// A status from before the command cannot replace its pending preview.
		confirmSdrAxisWindow(center, span, 0);
		expect(previewFreqWindow()?.lo).toBeCloseTo(requested - span / 2, 6);
		confirmSdrAxisWindow(accepted, span, 1);
		expect(previewFreqWindow()).toEqual({ lo: accepted - span / 2, hi: accepted + span / 2 });
		S.setRtaData({ freq: new Float64Array([accepted - span / 2, accepted + span / 2]),
			startHz: accepted - span / 2, stopHz: accepted + span / 2 });
		expect(previewFreqWindow()).toBeNull();
		// The quick follow-on drag uses the accepted center, including its hardware fit.
		axisDragStart('x', 400, IN_X_BAND.y, RECT);
		axisDragMove(480, IN_X_BAND.y);
		axisDragEnd();
		expect(sent[1].center).toBeCloseTo(accepted - span / 10, 6);
	});
});

describe('dragging the level labels', () => {
	it('moves the trace with the finger and commits one Ref on release', () => {
		expect(axisDragStart('y', IN_Y_BAND.x, 200, RECT)).toBe(true);
		axisDragMove(IN_Y_BAND.x, 240);                        // 40 px of 400 = 1/10 of 100 dB
		expect(sent).toEqual([]);
		const t = yAxisTransform()!;
		expect(t.sy).toBeCloseTo(1, 9);
		expect(t.dyFrac).toBeCloseTo(0.1, 9);
		axisDragEnd();
		expect(sent).toEqual([{ cmd: 'SET_REF', mode: 'manual', ref: -10 }]);
	});

	it('pans the level OFFSET in the relative (dB) display instead of the device Ref', () => {
		// In the relative display the top of the graticule is a rendering offset pinned to 0, not a
		// device reference: the gesture pans the value that actually moves that axis, which is the
		// client's own offset - so it applies as the pointer moves and nothing is requested.
		displayUnit.set('dB');
		commitLevelOffset(60);
		const reference = getDisplayRef();
		const base = displayOffset.get();
		expect(axisDragStart('y', IN_Y_BAND.x, 200, RECT)).toBe(true);
		axisDragMove(IN_Y_BAND.x, 240);                        // 40 px of 400 = 10 dB down
		expect(displayOffset.get()).toBeCloseTo(base - 10, 6);
		axisDragMove(IN_Y_BAND.x, 1200);                       // offsets are not manual Ref requests
		expect(displayOffset.get()).toBeCloseTo(base - 250, 6);
		axisDragMove(IN_Y_BAND.x, -1000);
		expect(displayOffset.get()).toBeCloseTo(base + 300, 6);
		expect(yAxisTransform()).toBeNull();                    // applied live: no preview layer
		axisDragEnd();
		expect(sent).toEqual([]);                               // ...and no device request at all
		expect(getDisplayRef()).toBe(reference);
	});

	it('applies a dB/div zoom immediately and still needs the Ref preview', () => {
		vi.useFakeTimers();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisWheel('y', -100, IN_Y_BAND.x, 200, RECT);   // wheel up = finer amplitude steps
		expect(S.dbPerDiv).toBeLessThan(10);                   // client-side scale: instant
		expect(S.dbPerDiv).toBeGreaterThan(0.5);
		expect(sent.filter((m) => m.cmd === 'SET_REF')).toEqual([]);
		vi.advanceTimersByTime(IDLE_COMMIT_MS + 1);            // a still pointer ends the gesture
		const refs = sent.filter((m) => m.cmd === 'SET_REF');
		expect(refs).toHaveLength(1);
		// The level under the pointer stays put, so the top line of a shorter window sits lower.
		expect(refs[0].ref).toBeLessThan(-20);
	});

	it('follows a slow drag with an idle commit instead of one request per move', () => {
		vi.useFakeTimers();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 210);
		expect(sent).toEqual([]);
		vi.advanceTimersByTime(IDLE_COMMIT_MS + 1);
		expect(sent).toHaveLength(1);
		expect(sent[0].cmd).toBe('SET_REF');
		// ...and the pointer can keep going afterwards.
		axisDragMove(IN_Y_BAND.x, 230);
		axisDragEnd();
		expect(sent.filter((m) => m.cmd === 'SET_REF').length).toBe(2);
	});

	it.each([false, true])('keeps a held drag after the settle deadline (acknowledged: %s)', (acknowledged) => {
		vi.useFakeTimers();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		vi.advanceTimersByTime(IDLE_COMMIT_MS + 1);
		if (acknowledged) confirmAxisRef(-10);
		vi.advanceTimersByTime(6500);
		expect(axisDragging()).toBe(true);
		expect(previewLevelWindow()?.ref).toBe(-10);
		axisDragMove(IN_Y_BAND.x, 280);
		expect(previewLevelWindow()?.ref).toBe(0);
		axisDragEnd();
		expect(axisDragging()).toBe(false);
		expect(sent).toEqual([
			{ cmd: 'SET_REF', mode: 'manual', ref: -10 },
			{ cmd: 'SET_REF', mode: 'manual', ref: 0 },
		]);
		confirmAxisRef(0);
		expect(previewLevelWindow()).toBeNull();
	});

	it('starts the unacknowledged preview deadline on release after a long hold', () => {
		vi.useFakeTimers();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		vi.advanceTimersByTime(IDLE_COMMIT_MS + 6500);
		axisDragEnd();
		expect(sent).toEqual([{ cmd: 'SET_REF', mode: 'manual', ref: -10 }]);
		expect(previewLevelWindow()?.ref).toBe(-10);
		vi.advanceTimersByTime(6001);
		expect(previewLevelWindow()).toBeNull();
	});

	it.each([0, 60, -60].flatMap(offset => [
		{ offset, bound: 100 + offset, raw: 40, beyond: 800, further: 1000, reverse: 240, final: -10 + offset },
		{ offset, bound: -200 + offset, raw: -160, beyond: -800, further: -1000, reverse: 160, final: -30 + offset },
	]))('previews the manual $bound display bound at offset $offset and allows reversal', ({ offset, bound, raw, beyond, further, reverse, final }) => {
		vi.useFakeTimers();
		commitLevelOffset(offset);
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, beyond);
		expect(previewLevelWindow()?.ref).toBe(bound);
		vi.advanceTimersByTime(IDLE_COMMIT_MS + 1);
		expect(sent).toEqual([{ cmd: 'SET_REF', mode: 'manual', ref: raw }]);
		expect(previewLevelWindow()?.ref).toBe(bound);
		expect(getDisplayRef()).toBe(bound);
		axisDragMove(IN_Y_BAND.x, further);
		vi.advanceTimersByTime(IDLE_COMMIT_MS + 1);
		expect(sent).toEqual([{ cmd: 'SET_REF', mode: 'manual', ref: raw }]);
		expect(previewLevelWindow()?.ref).toBe(bound);
		axisDragMove(IN_Y_BAND.x, reverse);
		expect(previewLevelWindow()?.ref).toBe(final);
		axisDragEnd();
		expect(sent).toEqual([
			{ cmd: 'SET_REF', mode: 'manual', ref: raw },
			{ cmd: 'SET_REF', mode: 'manual', ref: final - offset },
		]);
		expect(previewLevelWindow()?.ref).toBe(sent[1].ref + offset);
		confirmAxisRef(final - offset);
		expect(previewLevelWindow()).toBeNull();
	});

	it.each(['std', 'rta', 'sdr'] as const)('settles display +100 on raw +40 in %s and pans locally thereafter', mode => {
		graphMode.confirm(mode);
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 680);
		axisDragEnd();
		expect(getDisplayRef()).toBe(100);
		expect(sent).toEqual([{ cmd: 'SET_REF', mode: 'manual', ref: 40 }]);
		confirmAxisRef(100);
		expect(previewLevelWindow()?.ref).toBe(100);
		noteDisplayRefReport(40);
		confirmAxisRef(40);
		expect(previewLevelWindow()).toBeNull();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 160);
		axisDragEnd();
		expect(getDisplayRef()).toBe(90);
		expect(previewLevelWindow()).toBeNull();
		expect(sent).toHaveLength(1);
	});

	it.each([60, -60])('keeps marker coordinates stable when absolute offset changes to %s', (offset) => {
		const markerY = getY(-55);
		commitLevelOffset(offset);
		expect(getDisplayRef()).toBe(-20 + offset);
		expect(getY(-55)).toBeCloseTo(markerY, 8);
		expect(sent).toEqual([]);
	});

	it('shifts a held axis preview with the offset and continues from the same raw reference', () => {
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		const markerY = getY(-55);
		expect(previewLevelWindow()?.ref).toBe(-10);
		commitLevelOffset(60);
		expect(previewLevelWindow()?.ref).toBe(50);
		expect(getY(-55)).toBeCloseTo(markerY, 8);
		expect(sent).toEqual([]);
		axisDragMove(IN_Y_BAND.x, 280);
		expect(previewLevelWindow()?.ref).toBe(60);
		axisDragEnd();
		expect(sent).toEqual([{ cmd: 'SET_REF', mode: 'manual', ref: 0 }]);
		commitLevelOffset(-60);
		expect(previewLevelWindow()?.ref).toBe(-60);
		expect(getDisplayRef()).toBe(-60);
		expect(sent).toHaveLength(1);
		confirmAxisRef(0);
		expect(previewLevelWindow()).toBeNull();
	});

	it('commits a return to the starting Ref after an idle commit', () => {
		vi.useFakeTimers();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		vi.advanceTimersByTime(IDLE_COMMIT_MS + 1);
		axisDragMove(IN_Y_BAND.x, 200);
		axisDragEnd();
		expect(sent).toEqual([
			{ cmd: 'SET_REF', mode: 'manual', ref: -10 },
			{ cmd: 'SET_REF', mode: 'manual', ref: -20 },
		]);
	});

	it('does not repeat an idle-committed target on release and waits for its acknowledgement', () => {
		vi.useFakeTimers();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		vi.advanceTimersByTime(IDLE_COMMIT_MS + 1);
		axisDragEnd();
		expect(sent).toEqual([{ cmd: 'SET_REF', mode: 'manual', ref: -10 }]);
		// The optimistic display already matches, but that does not acknowledge the command.
		expect(getDisplayRef()).toBe(-10);
		expect(previewLevelWindow()?.ref).toBe(-10);
		confirmAxisRef(-20);
		expect(previewLevelWindow()?.ref).toBe(-10);
		confirmAxisRef(-10);
		expect(previewLevelWindow()).toBeNull();
	});

	it('keeps lowering the requested Ref across rapid gestures despite a different effective hardware Ref', () => {
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, -120); // -80 dB; deliberately past the trace and plot edge
		axisDragEnd();
		// Hardware gain fitting cannot pull the requested display back towards the signal.
		confirmRefLevel('std', -47.5, -100, -100);
		setDisplayRef('mode', refLevel.get());
		expect(effectiveRefLevel.get()).toBe(-47.5);
		confirmAxisRef(-100);
		expect(previewLevelWindow()).toBeNull();
		expect(getDisplayRef()).toBe(-100);
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 160);
		axisDragEnd();
		expect(sent).toEqual([
			{ cmd: 'SET_REF', mode: 'manual', ref: -100 },
			{ cmd: 'SET_REF', mode: 'manual', ref: -110 },
		]);
	});

	it('keeps the newest rapid drag pending through an older acknowledgement', () => {
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		axisDragEnd();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		axisDragEnd();
		confirmAxisRef(-10);
		expect(previewLevelWindow()?.ref).toBe(0);
		confirmAxisRef(0);
		expect(previewLevelWindow()).toBeNull();
		expect(sent.map(message => message.ref)).toEqual([-10, 0]);
	});

	it('retains the pending acknowledgement across a second press without movement', () => {
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		axisDragEnd();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragEnd();
		expect(sent).toEqual([{ cmd: 'SET_REF', mode: 'manual', ref: -10 }]);
		expect(previewLevelWindow()?.ref).toBe(-10);
		confirmAxisRef(-10);
		expect(previewLevelWindow()).toBeNull();
	});

	it('starts the next drag from a newer panel display instead of a cached axis target', () => {
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		axisDragEnd();
		confirmAxisRef(-10);
		setDisplayRef('user', -60);
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 160);
		axisDragEnd();
		expect(sent.map(message => message.ref)).toEqual([-10, -70]);
	});

	it('keeps SDR display Ref independent and drops rejected previews', () => {
		graphMode.confirm('sdr');
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		axisDragEnd();
		confirmRefLevel('sdr', -47.5, -10, -10);
		noteDisplayRefReport(-10);
		confirmAxisRef(-10);
		expect(getDisplayRef()).toBe(-10);
		expect(previewLevelWindow()).toBeNull();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 160);
		axisDragEnd();
		rejectManualRef();
		rejectAxisRef();
		expect(previewLevelWindow()).toBeNull();
		axisDragStart('y', IN_Y_BAND.x, 200, RECT);
		axisDragMove(IN_Y_BAND.x, 240);
		axisDragEnd();
		expect(sent.map(message => message.ref)).toEqual([-10, -20, 0]);
	});
});
