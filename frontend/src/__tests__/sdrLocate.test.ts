/**
 * SDR click positioning lifecycle (ui/sdrLocate.ts).
 *
 * These tests pin the contract the canvas owner relies on:
 *
 *   1. preview and commit agree - the committed frequency is exactly what the
 *      hover showed for the same pixel (channel grid, signal carrier, or free);
 *   2. a clean click commits the preview, but any changed premise (view, mode,
 *      pitch, filter, grid) demotes to the raw frequency with a notice instead
 *      of silently retuning somewhere the user did not point;
 *   3. the frozen candidate never changes after pointerdown, and cancel/invalidate
 *      clear the gesture without a commit;
 *   4. the CW dial is carrier - pitch (the sidetone sits above the dial), and an out-of-window dial is excluded;
 *   5. nothing here ever sends a command or touches the receiver.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { observeCarrierSpectrum, resetCarrierEvidence } from '../dsp/carrierSnap';
import {
	sdrChannelAnchorHz,
	sdrChannelStepHz,
	sdrDemod,
	sdrIfbw,
	sdrListenHz,
	sdrLocateMode,
	sdrPitchHz,
} from '../ui/sdrState';
import {
	__testSeedCapture,
	setCapture,
	setView,
} from '../ui/spectrumViewport';
import {
	beginSdrLocateClick,
	cancelSdrLocate,
	commitSdrLocateClick,
	invalidateSdrLocateEvidence,
	locateTargetAt,
	renderSdrLocate,
	sdrLocateClickPending,
	sdrLocateHover,
	snapRadiusHzAt,
	updateSdrLocateHover,
} from '../ui/sdrLocate';

const CAP = { lo: 100_000, hi: 163_000 };
/** A plot rectangle centre in logical canvas px (store defaults 860x480). */
const MID_X = 10 + 800 / 2;
const MID_Y = 14 + 440 / 2;

/** A 64-bin 1 kHz axis from 100 kHz with a carrier at `bin` (20 dB prominence). */
function seededCarrier(bin: number): { freq: Float64Array; powers: Float32Array } {
	const freq = new Float64Array(64);
	for (let i = 0; i < 64; i++) freq[i] = 100_000 + i * 1000;
	const powers = new Float32Array(64).fill(-100);
	powers[bin] = -80;
	observeCarrierSpectrum(freq, powers, 0, 1);
	observeCarrierSpectrum(freq, powers, 60, 1);
	observeCarrierSpectrum(freq, powers, 130, 1);
	return { freq, powers };
}

/** A 1280-bin 50 Hz axis from 100 kHz with a carrier at `bin` (20 dB prominence). */
function seededCarrierFine(bin: number): void {
	const freq = new Float64Array(1280);
	for (let i = 0; i < 1280; i++) freq[i] = 100_000 + i * 50;
	const powers = new Float32Array(1280).fill(-100);
	powers[bin] = -80;
	observeCarrierSpectrum(freq, powers, 0, 1);
	observeCarrierSpectrum(freq, powers, 60, 1);
	observeCarrierSpectrum(freq, powers, 130, 1);
}

/** Pin performance.now so evidence times (0/60/130) are current inside `fn`. */
function withClock(ms: number, fn: () => void): void {
	const spy = vi.spyOn(performance, 'now').mockReturnValue(ms);
	try {
		fn();
	} finally {
		spy.mockRestore();
	}
}

beforeEach(() => {
	resetAll('sdr');
	resetCarrierEvidence();
	__testSeedCapture(CAP);
	setCapture(CAP);
	S.setSdrMode(true);
	sdrDemod.set('am');
	sdrListenHz.set(131_500);
	sdrPitchHz.set(700);
	sdrIfbw.set(6000);
	sdrLocateMode.set('free');
	sdrChannelStepHz.set(25_000);
	sdrChannelAnchorHz.set(0);
	cancelSdrLocate();
});

describe('hover and commit agree', () => {
	it('free mode returns the pointer frequency for preview and commit', () => {
		const target = locateTargetAt(MID_X, MID_Y, 1)!;
		expect(target.mode).toBe('free');
		// The view is 63 kHz over 800 px; the mid pixel is the centre.
		expect(target.targetHz).toBeCloseTo(131_500, -1);
		expect(beginSdrLocateClick(MID_X, MID_Y, 1)).toBe(true);
		// The raw click frequency is the pointer's own Hz; commit must match preview.
		expect(commitSdrLocateClick(target.targetHz)).toBeCloseTo(target.targetHz, 6);
	});

	it.each(['minimum', 'maximum'] as const)('ignores clicks beyond the RF %s while allowing its exact boundary', (edge) => {
		const boundary = edge === 'minimum' ? S.FREQ_MIN : S.FREQ_MAX;
		const direction = edge === 'minimum' ? -1 : 1;
		const capture = { lo: boundary - 10_000, hi: boundary + 10_000 };
		__testSeedCapture(capture);
		setCapture(capture);
		const outsideX = MID_X + direction * 200;
		expect(locateTargetAt(outsideX, MID_Y, 1)).toBeNull();
		beginSdrLocateClick(outsideX, MID_Y, 1);
		expect(commitSdrLocateClick(boundary + direction * 5_000)).toBeNull();
		expect(locateTargetAt(MID_X, MID_Y, 1)?.targetHz).toBe(boundary);
		beginSdrLocateClick(MID_X, MID_Y, 1);
		expect(commitSdrLocateClick(boundary)).toBe(boundary);
	});

	it.each(['minimum', 'maximum'] as const)('keeps a valid click when its channel grid point exceeds the RF %s', (edge) => {
		const boundary = edge === 'minimum' ? S.FREQ_MIN : S.FREQ_MAX;
		const direction = edge === 'minimum' ? 1 : -1;
		const raw = boundary + direction * 100;
		const capture = { lo: raw - 10_000, hi: raw + 10_000 };
		__testSeedCapture(capture);
		setCapture(capture);
		sdrLocateMode.set('channel');
		sdrChannelStepHz.set(10_000);
		sdrChannelAnchorHz.set(boundary - direction * 100);
		expect(locateTargetAt(MID_X, MID_Y, 1)).toMatchObject({ mode: 'free', targetHz: raw });
		beginSdrLocateClick(MID_X, MID_Y, 1);
		expect(commitSdrLocateClick(raw)).toBe(raw);
	});

	it('channel mode snaps to anchor + round((f-anchor)/spacing)*spacing', () => {
		sdrLocateMode.set('channel');
		const target = locateTargetAt(MID_X, MID_Y, 1)!;
		expect(target.mode).toBe('channel');
		// 131500 with anchor 0 spacing 25000: (131500-0)/25000 = 5.26 -> round 5 -> 125000.
		expect(target.targetHz).toBe(125_000);
		expect(beginSdrLocateClick(MID_X, MID_Y, 1)).toBe(true);
		expect(commitSdrLocateClick(131_500)).toBe(125_000);
	});

	it('a non-zero anchor shifts the grid without shifting the formula', () => {
		sdrLocateMode.set('channel');
		sdrChannelAnchorHz.set(7_500);
		const target = locateTargetAt(MID_X, MID_Y, 1)!;
		// 131500 with anchor 7500 spacing 25000: (131500-7500)/25000 = 4.96 -> 5 -> 132500.
		expect(target.targetHz).toBe(132_500);
	});

	it('signal mode commits the carrier the preview showed (AM: dial == carrier)', () => {
		sdrLocateMode.set('signal');
		withClock(130, () => {
			const { freq, powers } = seededCarrier(32);
			// The carrier bin centre is 132000 Hz; find the pixel that previews it.
			const x = 10 + (132_000 - 100_000) / 63_000 * 800;
			const target = locateTargetAt(x, MID_Y, 1)!;
			expect(target.mode).toBe('signal');
			expect(target.targetHz).toBe(132_000);
			expect(target.carrierHz).toBe(132_000);
			expect(beginSdrLocateClick(x, MID_Y, 1)).toBe(true);
			expect(commitSdrLocateClick(132_000)).toBe(132_000);
			void freq; void powers;
		});
	});

	it('CW subtracts the confirmed pitch from the carrier for the dial', () => {
		sdrLocateMode.set('signal');
		sdrDemod.set('cw');
		sdrIfbw.set(1000);
		withClock(130, () => {
			seededCarrierFine(640);
			const x = 10 + (132_000 - 100_000) / 63_000 * 800;
			const target = locateTargetAt(x, MID_Y, 1)!;
			expect(target.mode).toBe('signal');
			expect(target.targetHz).toBe(131_300);
			expect(target.carrierHz).toBe(132_000);
		});
	});

	it('an out-of-window CW dial excludes the candidate (falls back to free)', () => {
		sdrLocateMode.set('signal');
		sdrDemod.set('cw');
		sdrPitchHz.set(30_000);
		withClock(130, () => {
			seededCarrierFine(400);
			const x = 10 + 20_000 / 63_000 * 800;
			const target = locateTargetAt(x, MID_Y, 1)!;
			expect(target.mode).toBe('free');
		});
	});

	it('keeps a CW sidetone of at least 100 Hz', () => {
		sdrLocateMode.set('signal');
		sdrDemod.set('cw');
		sdrPitchHz.set(0);
		withClock(130, () => {
			seededCarrierFine(640);
			expect(locateTargetAt(MID_X, MID_Y, 1)?.targetHz).toBe(131_900);
		});
	});
});

describe('commit discipline', () => {
	it('never silently changes the candidate when the premise moved', () => {
		sdrLocateMode.set('channel');
		const target = locateTargetAt(MID_X, MID_Y, 1)!;
		expect(target.targetHz).toBe(125_000);
		expect(beginSdrLocateClick(MID_X, MID_Y, 1)).toBe(true);
		// The view changed between pointerdown and release.
		setView({ lo: 120_000, hi: 140_000 });
		const raw = 131_500;
		expect(commitSdrLocateClick(raw)).toBe(raw);
	});

	it('drops the gesture on cancel and on evidence invalidation', () => {
		expect(beginSdrLocateClick(MID_X, MID_Y, 1)).toBe(true);
		expect(sdrLocateClickPending()).toBe(true);
		cancelSdrLocate();
		expect(sdrLocateClickPending()).toBe(false);
		expect(commitSdrLocateClick(131_500)).toBeNull();

		expect(beginSdrLocateClick(MID_X, MID_Y, 1)).toBe(true);
		invalidateSdrLocateEvidence();
		expect(sdrLocateClickPending()).toBe(false);
	});

	it('returns null when no gesture was armed', () => {
		expect(commitSdrLocateClick(131_500)).toBeNull();
	});

	it('returns null for an invalid raw frequency', () => {
		beginSdrLocateClick(MID_X, MID_Y, 1);
		expect(commitSdrLocateClick(NaN)).toBeNull();
		beginSdrLocateClick(MID_X, MID_Y, 1);
		expect(commitSdrLocateClick(-5)).toBeNull();
	});
});

describe('premise snapshots', () => {
	it('mode, pitch, filter and grid changes each demote to raw', () => {
		for (const change of [
			() => sdrDemod.set('nfm'),
			() => sdrPitchHz.set(800),
			() => sdrIfbw.set(9000),
			() => sdrChannelStepHz.set(12_500),
			() => sdrChannelAnchorHz.set(1000),
			() => sdrListenHz.set(140_000),
			() => sdrLocateMode.set('channel'),
		] as (() => void)[]) {
			beginSdrLocateClick(MID_X, MID_Y, 1);
			change();
			expect(commitSdrLocateClick(131_500)).toBe(131_500);
		}
	});

	it('expires a frozen signal instead of submitting its old carrier', () => {
		sdrLocateMode.set('signal');
		withClock(130, () => {
			seededCarrier(32);
			updateSdrLocateHover(MID_X, MID_Y, 1);
			expect(sdrLocateHover()?.targetHz).toBe(132_000);
			beginSdrLocateClick(MID_X, MID_Y, 1);
		});
		withClock(431, () => expect(commitSdrLocateClick(131_500)).toBe(131_500));
	});
});

describe('hover lifecycle', () => {
	it('updates the preview on mousemove and clears it outside the plot', () => {
		sdrLocateMode.set('channel');
		updateSdrLocateHover(MID_X, MID_Y, 1);
		expect(sdrLocateHover()).not.toBeNull();
		updateSdrLocateHover(2, 2, 1);
		expect(sdrLocateHover()).toBeNull();
	});

	it('clears when the SDR mode is off', () => {
		updateSdrLocateHover(MID_X, MID_Y, 1);
		S.setSdrMode(false);
		updateSdrLocateHover(MID_X, MID_Y, 1);
		expect(sdrLocateHover()).toBeNull();
		S.setSdrMode(true);
	});

	it('holds a brief dropout, refuses it on release, and clears at the evidence deadline', () => {
		sdrLocateMode.set('signal');
		withClock(130, () => {
			const { freq } = seededCarrier(32);
			updateSdrLocateHover(MID_X, MID_Y, 1);
			beginSdrLocateClick(MID_X, MID_Y, 1);
			observeCarrierSpectrum(freq, new Float32Array(64).fill(-100), 200, 1);
		});
		withClock(200, () => {
			renderSdrLocate(CAP.lo, CAP.hi);
			expect(sdrLocateHover()?.targetHz).toBe(132_000);
			expect(commitSdrLocateClick(131_500)).toBe(131_500);
			renderSdrLocate(CAP.lo, CAP.hi);
			expect(sdrLocateHover()?.targetHz).toBe(132_000);
		});
		withClock(430, () => {
			expect(renderSdrLocate(CAP.lo, CAP.hi)).toBe(false);
			expect(sdrLocateHover()).toBeNull();
		});
	});

	it('never draws a moving Free target or a Signal target without evidence', () => {
		for (const mode of ['free', 'signal'] as const) {
			sdrLocateMode.set(mode);
			updateSdrLocateHover(MID_X, MID_Y, 1);
			expect(renderSdrLocate(CAP.lo, CAP.hi)).toBe(false);
			expect(sdrLocateHover()).toBeNull();
		}
	});

	it('keeps one native-bin preview across jitter and freezes that exact target on click', () => {
		sdrLocateMode.set('signal');
		const { freq, powers } = seededCarrier(32);
		withClock(130, () => updateSdrLocateHover(MID_X, MID_Y, 1));
		powers[32] = -100;
		powers[33] = -80;
		observeCarrierSpectrum(freq, powers, 190, 1);
		withClock(190, () => {
			renderSdrLocate(CAP.lo, CAP.hi);
			expect(sdrLocateHover()?.carrierHz).toBe(132_000);
			expect(locateTargetAt(MID_X, MID_Y, 1)?.carrierHz).toBe(133_000);
			beginSdrLocateClick(MID_X, MID_Y, 1);
		});
		powers[40] = -40;
		observeCarrierSpectrum(freq, powers, 250, 1);
		withClock(250, () => {
			updateSdrLocateHover(MID_X + 20, MID_Y, 1);
			renderSdrLocate(CAP.lo, CAP.hi);
			expect(sdrLocateHover()?.carrierHz).toBe(132_000);
			expect(commitSdrLocateClick(131_500)).toBe(132_000);
		});
	});

	it('uses a wider release radius while the same local carrier remains credible', () => {
		sdrLocateMode.set('signal');
		seededCarrier(32);
		const x = 10 + 32_000 / 63_000 * 800;
		withClock(130, () => {
			updateSdrLocateHover(x, MID_Y, 1);
			updateSdrLocateHover(x - 48, MID_Y, 1);
			expect(sdrLocateHover()?.carrierHz).toBe(132_000);
			expect(locateTargetAt(x - 48, MID_Y, 1)?.mode).toBe('free');
			updateSdrLocateHover(x - 53, MID_Y, 1);
			expect(sdrLocateHover()).toBeNull();
		});
	});

	it('commits the retained preview when pointerdown preserves fractional screen pixels', () => {
		sdrLocateMode.set('signal');
		seededCarrier(32);
		const pixelScale = 1.25;
		const x = 10 + 32_000 / 63_000 * 800;
		const retainedX = x - 45 * pixelScale;
		withClock(130, () => {
			updateSdrLocateHover(x, MID_Y, pixelScale);
			updateSdrLocateHover(retainedX, MID_Y, pixelScale);
			expect(sdrLocateHover()?.targetHz).toBe(132_000);
			const downX = retainedX + 0.7 * pixelScale;
			const downY = MID_Y + 0.6 * pixelScale;
			expect(locateTargetAt(downX, downY, pixelScale)?.mode).toBe('free');
			beginSdrLocateClick(downX, downY, pixelScale);
			expect(commitSdrLocateClick(100_000 + (downX - 10) / 800 * 63_000)).toBe(132_000);
		});
	});

	it('clears the preview when source evidence changes even on the same axis', () => {
		sdrLocateMode.set('signal');
		withClock(130, () => {
			const { freq, powers } = seededCarrier(32);
			updateSdrLocateHover(MID_X, MID_Y, 1);
			observeCarrierSpectrum(freq, powers, 130, 2);
			expect(renderSdrLocate(CAP.lo, CAP.hi)).toBe(false);
			expect(sdrLocateHover()).toBeNull();
		});
	});

	it('expires evidence at its original deadline despite repeated redraws', () => {
		sdrLocateMode.set('signal');
		withClock(130, () => {
			seededCarrier(32);
			updateSdrLocateHover(MID_X, MID_Y, 1);
		});
		for (const now of [200, 300, 400, 429]) {
			withClock(now, () => {
				expect(renderSdrLocate(CAP.lo, CAP.hi)).toBe(true);
				expect(sdrLocateHover()?.targetHz).toBe(132_000);
			});
		}
		for (const now of [430, 450, 500]) {
			withClock(now, () => {
				expect(renderSdrLocate(CAP.lo, CAP.hi)).toBe(false);
				expect(sdrLocateHover()).toBeNull();
				expect(S.ctx.canvas.dataset.locate).toBe('');
			});
		}
	});

	it('writes the dataset evidence contract for the real UI', () => {
		sdrLocateMode.set('channel');
		updateSdrLocateHover(MID_X, MID_Y, 1);
		renderSdrLocate(CAP.lo, CAP.hi);
		const canvas = document.getElementById('spectrum') as HTMLCanvasElement | null;
		expect(canvas).not.toBeNull();
		const parsed = JSON.parse(canvas!.dataset.locate ?? '');
		expect(parsed.mode).toBe('channel');
		expect(parsed.target).toBe(125_000);
		// No hover: the attribute clears.
		cancelSdrLocate();
		renderSdrLocate(CAP.lo, CAP.hi);
		expect(canvas!.dataset.locate).toBe('');
	});
});

describe('snap radius honesty', () => {
	it('caps the radius by the mode width, the screen window and the Hz envelope', () => {
		expect(snapRadiusHzAt(1)).toBe(3150);
		sdrDemod.set('cw');
		sdrIfbw.set(500);
		expect(snapRadiusHzAt(1)).toBe(3000);
		expect(snapRadiusHzAt(0.001)).toBeLessThan(3150);
		setView({ lo: 131_000, hi: 132_000 });
		sdrDemod.set('am');
		expect(snapRadiusHzAt(1)).toBeCloseTo(40 * 1000 / 800, 5);
	});

	it('is zero where the mode has no carrier', () => {
		sdrDemod.set('usb');
		expect(snapRadiusHzAt(1)).toBe(0);
	});
});
