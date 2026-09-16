/**
 * The render/DSP hot path must never read layout.
 *
 * A `getBoundingClientRect()` / `clientWidth` read on a per-frame path forces a style and
 * layout flush for every delivered frame. The waterfall did exactly that until the canvas
 * became box-derived: the plot's CSS box now arrives from core/store.ts via a ResizeObserver
 * (and the waterfall derives its width from the same logical plot rectangle), so the frame
 * loop is pure arithmetic. This test walks the hot-path directories and fails on the first
 * layout read, so the regression cannot come back through a helper unnoticed.
 *
 * `core/store.ts` is deliberately not covered: measuring the box there, on resize, is the
 * whole point.
 */
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const HOT_DIRS = ['render', 'dsp', 'meas'];
const READ = /\b(getBoundingClientRect|clientWidth|clientHeight|offsetWidth|offsetHeight|offsetTop|offsetLeft|scrollWidth|scrollHeight|getComputedStyle)\b/;

/** Source without comments: the rule is about calls, and the comments explain the rule. */
function code(text: string): string {
	return text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '');
}

/** src/ - vitest runs with the frontend package as cwd, but do not depend on it. */
function srcDir(): string {
	let dir = process.cwd();
	for (let i = 0; i < 4; i++) {
		if (existsSync(resolve(dir, 'src', 'render', 'spectrum.ts'))) return resolve(dir, 'src');
		dir = resolve(dir, '..');
	}
	throw new Error(`cannot locate src/ from ${process.cwd()}`);
}

function hotPathFiles(): [string, string][] {
	const src = srcDir();
	const out: [string, string][] = [];
	for (const dir of HOT_DIRS) {
		const base = resolve(src, dir);
		for (const name of readdirSync(base)) {
			if (!name.endsWith('.ts')) continue;
			out.push([`${dir}/${name}`, code(readFileSync(resolve(base, name), 'utf8'))]);
		}
	}
	return out;
}

describe('render hot path', () => {
	it('never reads layout (the size arrives from the ResizeObserver in core/store.ts)', () => {
		const offenders: string[] = [];
		for (const [name, text] of hotPathFiles()) {
			text.split('\n').forEach((line, i) => {
				const hit = line.match(READ);
				if (hit) offenders.push(`${name}:${i + 1}: ${hit[1]}`);
			});
		}
		expect(offenders).toEqual([]);
	});

	it('covers the directories it claims to (a moved file must not silently escape)', () => {
		const files = hotPathFiles().map(([name]) => name);
		expect(files).toContain('render/spectrum.ts');
		expect(files).toContain('render/waterfall.ts');
		expect(files.length).toBeGreaterThan(6);
	});
});
