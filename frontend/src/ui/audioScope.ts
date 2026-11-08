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
	const styles = (canvas.ownerDocument.defaultView ?? window).getComputedStyle(canvas);
	const line = styles.getPropertyValue('--text').trim() || '#ccc';
	const grid = styles.getPropertyValue('--card-border').trim() || '#444';
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
		for (let i = 0; i < t.length; i++) {
			const x = (i / (t.length - 1)) * (w - 1);
			const y = mid - t[i] * scale;
			if (i === 0) ctx.moveTo(x, y);
			else ctx.lineTo(x, y);
		}
		ctx.stroke();
	}
}

