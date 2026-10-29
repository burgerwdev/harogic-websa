import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as S from '../core/store';
import { resetAll } from '../core/params';
import { renderSpectrumZoomOverview } from '../render/zoomOverview';
import { __testSeedCapture, setView, zoomEnabled } from '../ui/spectrumViewport';
import { sdrListenHz } from '../ui/sdrState';
import { axisDragMove, axisDragStart, resetAxisPreview } from '../ui/axisDrag';
import { graphMode } from '../ui/graphMode';

const CAP = { lo: 0, hi: 100 };
type Call = { op: 'moveTo' | 'lineTo'; x: number; y: number };

function drawOverview(freq: Float64Array, spec: Float32Array, width = 100): Call[] {
	const calls: Call[] = [];
	const context = {
		setTransform: vi.fn(), clearRect: vi.fn(), fillRect: vi.fn(), beginPath: vi.fn(),
		stroke: vi.fn(), strokeRect: vi.fn(), fillText: vi.fn(),
		save: vi.fn(), restore: vi.fn(), setLineDash: vi.fn(), closePath: vi.fn(), fill: vi.fn(),
		moveTo: vi.fn((x: number, y: number) => calls.push({ op: 'moveTo', x, y })),
		lineTo: vi.fn((x: number, y: number) => calls.push({ op: 'lineTo', x, y })),
	};
	const canvas = document.createElement('canvas');
	canvas.id = 'zoom-overview';
	Object.defineProperties(canvas, {
		clientWidth: { value: width },
		clientHeight: { value: 40 },
	});
	vi.spyOn(canvas, 'getContext').mockReturnValue(context as unknown as CanvasRenderingContext2D);
	document.body.append(canvas);
	S.setSdrMode(true);
	S.setRtaData({ freq, spec, startHz: CAP.lo, stopHz: CAP.hi });
	S.setRtaDisplays([spec, null, null, null]);
	__testSeedCapture(CAP);
	renderSpectrumZoomOverview();
	return calls;
}

function yAt(value: number, values: Float32Array): number {
	const finite = [...values].filter(Number.isFinite);
	const lo = Math.min(...finite), hi = Math.max(...finite);
	const pad = (hi - lo) * 0.1;
	return (1 - (value - (lo - pad)) / (hi - lo + 2 * pad)) * 40;
}

beforeEach(() => {
	document.body.replaceChildren();
	resetAll();
	resetAxisPreview();
	S.setSdrMode(false);
	S.setRtaData(null);
	S.setRtaDisplays([null, null, null, null]);
	__testSeedCapture(null);
	zoomEnabled.set(true);
	sdrListenHz.confirm(0);
});

describe('SDR overview native trace', () => {
	it('connects sparse samples at their native-Hz positions', () => {
		const freq = Float64Array.from([0, 10, 60, 100]);
		const spec = Float32Array.from([-80, -20, -60, -40]);
		const calls = drawOverview(freq, spec);
		expect(calls.map(call => call.op)).toEqual(['moveTo', 'lineTo', 'lineTo', 'lineTo']);
		expect(calls.map(call => call.x)).toEqual([0, 10, 60, 100]);
		calls.forEach((call, i) => expect(call.y).toBeCloseTo(yAt(spec[i], spec), 10));
	});

	it('lifts the pen at a NaN instead of bridging the gap', () => {
		const freq = Float64Array.from([0, 25, 50, 75, 100]);
		const spec = Float32Array.from([-80, -20, NaN, -60, -40]);
		const calls = drawOverview(freq, spec);
		expect(calls.map(call => [call.op, call.x])).toEqual([
			['moveTo', 0], ['lineTo', 25], ['moveTo', 75], ['lineTo', 100],
		]);
	});

	it('retains dense-column extrema in native source order', () => {
		const freq = Float64Array.from([0, 1, 2, 3, 4, 96, 97, 98, 99, 100]);
		const spec = Float32Array.from([-50, -90, -10, -20, -70, -10, -90, -20, -30, -70]);
		const calls = drawOverview(freq, spec, 4);
		expect(calls[0].op).toBe('moveTo');
		expect(calls.slice(1).every(call => call.op === 'lineTo')).toBe(true);
		const xs = calls.map(call => call.x);
		expect(xs).toEqual([...xs].sort((a, b) => a - b));
		for (const index of [0, 1, 2, 4, 5, 6, 9]) {
			const vertex = calls.find(call => Math.abs(call.x - freq[index] / 100 * 4) < 1e-10);
			expect(vertex).toBeDefined();
			expect(vertex!.y).toBeCloseTo(yAt(spec[index], spec), 10);
		}
	});

	it.each([25, 75, 100])('keeps the listen indicator at %s Hz visible outside the zoom box', listen => {
		sdrListenHz.confirm(listen);
		const calls = drawOverview(Float64Array.from([0, 50, 100]), Float32Array.from([-80, -20, -60]));
		setView({ lo: 40, hi: 60 }, 1);
		calls.length = 0;
		renderSpectrumZoomOverview();
		// The final path is the flag; the preceding vertical line shares its RF coordinate.
		const line = calls.slice(-5, -3);
		expect(line.map(point => point.x)).toEqual([listen, listen]);
		expect(line[1].y).toBe(40);
	});

	it('moves the indicator with tuning and hides it outside capture or outside SDR', () => {
		sdrListenHz.confirm(25);
		const calls = drawOverview(Float64Array.from([0, 50, 100]), Float32Array.from([-80, -20, -60]));
		sdrListenHz.confirm(75);
		calls.length = 0;
		renderSpectrumZoomOverview();
		expect(calls.slice(-5, -3).map(point => point.x)).toEqual([75, 75]);
		sdrListenHz.confirm(125);
		calls.length = 0;
		renderSpectrumZoomOverview();
		expect(calls.every(point => point.y !== 0 && point.y !== 40)).toBe(true);
		sdrListenHz.confirm(75);
		S.setSdrMode(false);
		S.setRtaMode(true);
		calls.length = 0;
		renderSpectrumZoomOverview();
		expect(calls.every(point => point.y !== 0 && point.y !== 40)).toBe(true);
	});

	it.each(['std', 'rta', 'sdr'] as const)('previews overview motion before a %s pan is released', mode => {
		const calls = drawOverview(Float64Array.from([0, 50, 100]), Float32Array.from([-80, -20, -60]));
		const canvas = document.getElementById('zoom-overview') as HTMLCanvasElement;
		const ctx = canvas.getContext('2d')!;
		const freq = Float64Array.from([100e6, 150e6, 200e6]);
		const spec = Float32Array.from([-80, -20, -60]);
		graphMode.confirm(mode);
		S.setSdrMode(mode === 'sdr');
		S.setRtaMode(mode !== 'std');
		S.setFreqArray(freq);
		S.traces[0].powers = spec;
		S.setRtaData({ freq, spec, startHz: 100e6, stopHz: 200e6 });
		__testSeedCapture({ lo: 100e6, hi: 200e6 });
		setView({ lo: 140e6, hi: 160e6 }, 1);
		sdrListenHz.confirm(150e6);
		axisDragStart('x', 400, 450, { x: 0, y: 0, w: 800, h: 400 });
		calls.length = 0;
		renderSpectrumZoomOverview();
		expect(calls[1].x).toBe(50);
		axisDragMove(600, 450);
		calls.length = 0;
		renderSpectrumZoomOverview();
		// Overview remaps old RF samples onto the target band immediately, while
		// the zoom box remains fixed relative to that moving observation frame.
		expect(calls[1].x).toBeCloseTo(55, 10);
		expect(ctx.strokeRect).toHaveBeenLastCalledWith(40, 0.5, 20, 39);
		if (mode === 'sdr') {
			for (const point of calls.slice(-5, -3)) expect(point.x).toBeCloseTo(55, 10);
		}
		resetAxisPreview();
		calls.length = 0;
		renderSpectrumZoomOverview();
		expect(calls[1].x).toBe(50);
	});


});
