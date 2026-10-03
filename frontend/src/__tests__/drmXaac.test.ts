/**
 * xHE-AAC (MPEG-D USAC) smoke test against the *committed* `dsp.wasm` artifact.
 *
 * The libxaac decoder is wired for the DRM xHE-AAC audio service (coding 3). This test feeds the
 * committed `usac_tone` fixture (a 22-byte USAC AudioSpecificConfig followed by 45 access units,
 * 2333/768 bytes each) to the `websa_dsp_xaac_decode` export and asserts the run decodes to
 * non-silent PCM. The fixture is generated externally by libxaac's `xaacenc` (data, not source);
 * the native lossless reference decode is exercised by the same decoder in `wasm/src/xaac.rs`.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');
const FIXTURE = resolve(process.cwd(), '..', 'tests', 'fixtures', 'drm', 'usac_tone.bin');
/// 45 frames (1 x 2333 bytes + 44 x 768 bytes) => 92160 16-bit samples.
const EXPECTED_SAMPLES = 92160;

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

describe('DRM xHE-AAC audio (libxaac USAC)', () => {
	it('decodes the committed USAC fixture to non-silent PCM', async () => {
		const dsp = await instantiateDsp(artifactBytes());
		const fixture = readFileSync(FIXTURE);

		const ptr = dsp.alloc(fixture.length);
		expect(ptr).toBeGreaterThan(0);
		dsp.u8View(ptr, fixture.length).set(fixture);

		// A positive sample count means the decoder produced non-silent PCM (the export
		// returns -4 for a silent decode).
		expect(dsp.exports.websa_dsp_xaac_decode(ptr, fixture.length)).toBe(EXPECTED_SAMPLES);

		dsp.free(ptr, fixture.length);
	});

	it('rejects a short buffer instead of pretending it decoded', async () => {
		const dsp = await instantiateDsp(artifactBytes());
		const ptr = dsp.alloc(4);
		dsp.u8View(ptr, 4).fill(0);
		expect(dsp.exports.websa_dsp_xaac_decode(ptr, 4)).toBe(-1);
		dsp.free(ptr, 4);
	});
});
