import { getTheme } from '../core/theme';

const palette = new WeakMap<HTMLCanvasElement, { theme: string; document: Document; line: string; grid: string }>();

/** Shared presentation for actual demodulated audio samples in receiver views. */
export function drawAudioScope(
	canvas: HTMLCanvasElement,
	traces: readonly (Float32Array | readonly number[])[],
	scalePeak: number,
): void {
	const ctx = canvas.getContext('2d');
	if (!ctx) return;
	const w = canvas.width;
	const h = canvas.height;
	if (!w || !h || !traces.length) { ctx.clearRect(0, 0, w, h); return; }
	let colors = palette.get(canvas);
	if (!colors || colors.theme !== getTheme() || colors.document !== canvas.ownerDocument) {
		const styles = (canvas.ownerDocument.defaultView ?? window).getComputedStyle(canvas);
		const line = styles.getPropertyValue('--text').trim(), grid = styles.getPropertyValue('--card-border').trim();
		colors = { theme: getTheme(), document: canvas.ownerDocument,
			line: line || '#ccc', grid: grid || '#444' };
		// A Pop out document can paint before its copied stylesheet loads.
		if (line && grid) palette.set(canvas, colors);
	}
	const { line, grid } = colors;
	ctx.clearRect(0, 0, w, h);
	// Slow peak hold stabilizes the vertical scale across audio blocks. Sharing the scale
	// between lanes preserves the visible L/R balance.
	let peak = scalePeak;
	if (peak <= 1e-6) {
		for (const t of traces) for (const v of t) peak = Math.max(peak, Math.abs(v));
	}
	const laneH = h / traces.length;
	const scale = peak > 1e-6 ? (laneH / 2 - 2) / peak : 0;
	ctx.strokeStyle = grid;
	ctx.lineWidth = 1;
	ctx.beginPath();
	for (let lane = 0; lane < traces.length; lane++) {
		const mid = laneH * lane + laneH / 2;
		ctx.moveTo(0, mid + 0.5);
		ctx.lineTo(w, mid + 0.5);
		if (lane > 0) {
			ctx.moveTo(0, laneH * lane + 0.5);
			ctx.lineTo(w, laneH * lane + 0.5);
		}
	}
	ctx.stroke();
	ctx.strokeStyle = line;
	ctx.lineWidth = 1.25;
	for (let lane = 0; lane < traces.length; lane++) {
		const t = traces[lane];
		const mid = laneH * lane + laneH / 2;
		ctx.beginPath();
		const xScale = t.length > 1 ? (w - 1) / (t.length - 1) : 0;
		let pen = false, i = 0;
		while (i < t.length) {
			if (!Number.isFinite(t[i])) { pen = false; i++; continue; }
			const start = i, column = Math.floor(i * xScale);
			let min = i, max = i;
			i++;
			while (i < t.length && Number.isFinite(t[i]) && Math.floor(i * xScale) === column) {
				if (t[i] < t[min]) min = i;
				if (t[i] > t[max]) max = i;
				i++;
			}
			// Keep extrema in time order: short impulses survive at every scope width.
			let previous = -1;
			for (let vertex = 0; vertex < 4; vertex++) {
				const index = vertex === 0 ? start : vertex === 3 ? i - 1
					: vertex === 1 ? Math.min(min, max) : Math.max(min, max);
				if (index === previous) continue;
				previous = index;
				const x = index * xScale, y = mid - t[index] * scale;
				if (pen) ctx.lineTo(x, y);
				else { ctx.moveTo(x, y); pen = true; }
			}
		}
		ctx.stroke();
	}
}

