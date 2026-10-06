/**
 * DRM AAC audio smoke test against the *committed* `dsp.wasm` artifact.
 *
 * The DRM receiver's audio stage decodes AAC access units through the FDK AAC decoder in
 * `TT_DRM` transport. This test feeds the committed `aac_sine_24k` fixture (9 re-serialised
 * DRM access units, 36 bytes each) to the `websa_dsp_fdk_decode_drm_frames` export and asserts
 * the run decodes to non-silent PCM. The fixture was produced by `tools/fixtures/gen_drm_aac_fixture.py`
 * and its native lossless check is `tools/fixtures/verify_drm_aac.c`; this test is the in-repo WASM
 * counterpart (the native `cargo test` cannot link the wasm32-only codec).
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { instantiateDsp } from '../sdr/wasm';

const ARTIFACT = resolve(process.cwd(), '..', 'frontend', 'public', 'dsp.wasm');
const FIXTURE = resolve(process.cwd(), '..', 'tests', 'fixtures', 'drm', 'aac_sine_24k.drm');
/// From `tests/fixtures/drm/aac_sine_24k.json`: 9 frames of 36 bytes each.
const FRAME_BYTES = 36;

const artifactBytes = (): ArrayBuffer => {
	const buf = readFileSync(ARTIFACT);
	return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength) as ArrayBuffer;
};

describe('DRM AAC audio (FDK TT_DRM)', () => {
	it('decodes the committed AAC fixture to non-silent PCM', async () => {
		const dsp = await instantiateDsp(artifactBytes());
		const fixture = readFileSync(FIXTURE);
		expect(fixture.length % FRAME_BYTES).toBe(0);

		const ptr = dsp.alloc(fixture.length);
		expect(ptr).toBeGreaterThan(0);
		dsp.u8View(ptr, fixture.length).set(fixture);

		// A run of frames, fed one access unit per call, passes the decoder's two-frame
		// priming delay and returns the frame size only when the total energy is non-zero.
		expect(dsp.exports.websa_dsp_fdk_decode_drm_frames(ptr, fixture.length, FRAME_BYTES))
			.toBe(960);
		// A single access unit still decodes (frame size 960); the priming frame is silent.
		expect(dsp.exports.websa_dsp_fdk_decode_drm(ptr, FRAME_BYTES)).toBe(960);
		// Bad geometry is rejected before the codec runs.
		expect(dsp.exports.websa_dsp_fdk_decode_drm_frames(ptr, fixture.length, 0)).toBe(-4);

		dsp.free(ptr, fixture.length);
	});

	it('rejects a non-DRM buffer instead of pretending it decoded', async () => {
		const dsp = await instantiateDsp(artifactBytes());
		const ptr = dsp.alloc(FRAME_BYTES);
		dsp.u8View(ptr, FRAME_BYTES).fill(0xff);
		expect(dsp.exports.websa_dsp_fdk_decode_drm(ptr, FRAME_BYTES)).toBeLessThan(0);
		dsp.free(ptr, FRAME_BYTES);
	});
});
