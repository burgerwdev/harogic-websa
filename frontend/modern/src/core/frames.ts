/**
 * Binary frame decoder — the single TypeScript definition of the wire format.
 *
 * The header layout is produced by the Python side (`web_sa/measurements/framer.py`,
 * `rta.py:_encode_rta`, `sdr.py:encode_audio`). Keeping one decoder here means the format
 * is described once per language and can be locked by golden fixtures generated from the
 * Python encoders (`tests/fixtures/frames/*.bin`, checked by `__tests__/frames.test.ts`).
 *
 * Every frame starts with a 4-byte ASCII magic and a common 12-byte header:
 *   magic(4) + version(u32) + points(u32) + sweep_ms(f32)
 * except AUDF, whose trailing field is the sample rate instead of sweep_ms.
 *
 *   FREQ  magic + common header + float64[points]   (frequency axis, Hz)
 *   POWR  magic + common header + float32[points]   (power, dBm)
 *   RTAF  magic + ver(u32) pts(u32) wfLen(u16) maxD(u16) startHz(f64)   (8-byte aligned)
 *         + float64[pts] + float32[pts] + uint16[wfLen] + stopHz(f64)
 *   AUDF  magic + seq(u32) rate(u32) samples(u32) + int16[samples]      (mono PCM)
 *   VSAD  magic + ver(u32) kind(u32) rows(u32) cols(u32) idealRows(u32) idealCols(u32)
 *         + float32[5] scalars + float32[rows*cols] + float32[idealRows*idealCols]
 *         + uint32[1] metaLen + float32[metaLen]                          (VSA measurement)
 *
 * Lengths are validated strictly: a frame whose declared size does not match the buffer is
 * rejected instead of being interpreted with a wrong stride. Returns null for anything that
 * is not a frame this client understands.
 */

export const MAGIC_FREQ = 'FREQ';
export const MAGIC_POWR = 'POWR';
export const MAGIC_RTAF = 'RTAF';
export const MAGIC_AUDIO = 'AUDF';
export const MAGIC_VSA = 'VSAD';

export const COMMON_HEADER_BYTES = 16;      // magic + version + points + sweep_ms
export const RTA_HEADER_BYTES = 24;         // magic + ver + pts + wfLen + maxD + startHz
export const AUDIO_HEADER_BYTES = 16;       // magic + seq + rate + samples
export const VSA_HEADER_BYTES = 48;         // magic + 6 u32 + 5 f32 (8-byte aligned head)

/** VSAD payload kinds, numbered in the order the `kind` field uses. */
export const VSA_KINDS = ['constellation', 'power', 'ccdf', 'spectrogram'] as const;
/**
 * The VSAD measurement block is positional: index i of the float32 block is this key.
 * A slot the backend cannot fill yet is NaN, so a panel can hide it instead of drawing 0.
 */
export const VSA_MEASURE_KEYS = [
	'mean_dbm', 'peak_dbm', 'peak_bin_dbm', 'peak_hz', 'floor_dbm', 'floor_1hz_dbm',
	'centroid_hz', 'rms_v', 'duty', 'samples', 'symbols_n', 'points', 'evm_percent',
	'mer_db', 'snr_db',
] as const;

export interface FrameHeader {
	version: number;
	points: number;
	sweepMs: number;
}

export interface FreqFrame extends FrameHeader {
	kind: 'freq';
	freq: Float64Array;
}

export interface PowrFrame extends FrameHeader {
	kind: 'powr';
	power: Float32Array;
}

export interface RtaFrame extends FrameHeader {
	kind: 'rta';
	wfLen: number;
	maxDensity: number;
	startHz: number;
	stopHz: number;
	freq: Float64Array;
	spec: Float32Array;
	wfRow: Uint16Array;
}

export interface AudioFrame {
	kind: 'audio';
	seq: number;
	rate: number;
	samples: number;
	pcm: Int16Array;
}

export interface VsaFrame {
	kind: 'vsa';
	version: number;
	/** Payload kind: a symbol cloud, a power trace, a CCDF curve or a spectrogram. */
	measure: (typeof VSA_KINDS)[number] | string;
	/** `data` is (rows, cols): interleaved I/Q for a cloud, (x, y) pairs for a curve. */
	rows: number;
	cols: number;
	data: Float32Array;
	ideal: Float32Array;
	idealRows: number;
	idealCols: number;
	symbolRateHz: number;
	cfoHz: number;
	timingSamples: number;
	evmPercent: number;
	snrDb: number;
	/** Named scalars of the whole capture (see VSA_MEASURE_KEYS); NaN when absent. */
	measurements: Record<string, number>;
}

export type DecodedFrame = FreqFrame | PowrFrame | RtaFrame | AudioFrame | VsaFrame;

/** ASCII magic of a binary frame, or '' when the buffer is too short. */
export function frameMagic(data: ArrayBuffer): string {
	if (data.byteLength < 4) return '';
	const v = new DataView(data, 0, 4);
	return String.fromCharCode(v.getUint8(0), v.getUint8(1), v.getUint8(2), v.getUint8(3));
}

export function decodeFrame(data: ArrayBuffer): DecodedFrame | null {
	if (data.byteLength < 4) return null;
	const magic = frameMagic(data);
	if (magic === MAGIC_AUDIO) return decodeAudio(data);
	if (magic === MAGIC_VSA) return decodeVsad(data);
	if (data.byteLength < COMMON_HEADER_BYTES) return null;
	const head = new DataView(data, 4, 12);
	const version = head.getUint32(0, true);
	const points = head.getUint32(4, true);
	const sweepMs = head.getFloat32(8, true);
	if (magic === MAGIC_FREQ) {
		if (data.byteLength !== COMMON_HEADER_BYTES + points * 8) return null;
		return { kind: 'freq', version, points, sweepMs, freq: new Float64Array(data, COMMON_HEADER_BYTES, points) };
	}
	if (magic === MAGIC_POWR) {
		if (data.byteLength !== COMMON_HEADER_BYTES + points * 4) return null;
		return { kind: 'powr', version, points, sweepMs, power: new Float32Array(data, COMMON_HEADER_BYTES, points) };
	}
	if (magic === MAGIC_RTAF) {
		if (data.byteLength < RTA_HEADER_BYTES) return null;
		const hdr = new DataView(data, 4, 20);
		const wfLen = hdr.getUint16(8, true);
		const maxDensity = hdr.getUint16(10, true);
		const startHz = hdr.getFloat64(12, true);
		const expected = RTA_HEADER_BYTES + points * 8 + points * 4 + wfLen * 2 + 8;
		if (points < 2 || wfLen < 1 || data.byteLength !== expected) return null;
		let off = RTA_HEADER_BYTES;
		const freq = new Float64Array(data, off, points);
		off += points * 8;
		const spec = new Float32Array(data, off, points);
		off += points * 4;
		const wfRow = new Uint16Array(data, off, wfLen);
		off += wfLen * 2;
		const stopHz = new DataView(data, off, 8).getFloat64(0, true);
		return { kind: 'rta', version, points, sweepMs, wfLen, maxDensity, startHz, stopHz, freq, spec, wfRow };
	}
	return null;
}

/**
 * VSAD (VSA data): a Tier 1 measurement as a float32 matrix plus its scalar block.
 *
 * `rows`/`cols` describe what a display can use (an evenly decimated slice), not the
 * capture, so the frame stays bounded however deep the capture was. Lengths are validated
 * strictly: a frame that does not match its declared shape is rejected rather than read
 * with a wrong stride.
 */
export function decodeVsad(data: ArrayBuffer): VsaFrame | null {
	if (data.byteLength < VSA_HEADER_BYTES) return null;
	const h = new DataView(data, 4, 44);
	const version = h.getUint32(0, true);
	const kindId = h.getUint32(4, true);
	const rows = h.getUint32(8, true);
	const cols = h.getUint32(12, true);
	const idealRows = h.getUint32(16, true);
	const idealCols = h.getUint32(20, true);
	if (cols < 1 || idealCols < 0) return null;
	const arraysBytes = (rows * cols + idealRows * idealCols) * 4;
	if (data.byteLength < VSA_HEADER_BYTES + arraysBytes + 4) return null;
	const metaLen = new DataView(data, VSA_HEADER_BYTES + arraysBytes, 4).getUint32(0, true);
	if (data.byteLength !== VSA_HEADER_BYTES + arraysBytes + 4 + metaLen * 4) return null;
	const scalars = new DataView(data, 4 + 24, 20);
	let off = VSA_HEADER_BYTES;
	const cloud = new Float32Array(data, off, rows * cols);
	off += rows * cols * 4;
	const ideal = new Float32Array(data, off, idealRows * idealCols);
	off += idealRows * idealCols * 4;
	const meta = new Float32Array(data, off + 4, metaLen);
	const measurements: Record<string, number> = {};
	for (let i = 0; i < metaLen && i < VSA_MEASURE_KEYS.length; i++) {
		measurements[VSA_MEASURE_KEYS[i]] = meta[i];
	}
	return {
		kind: 'vsa',
		version,
		measure: VSA_KINDS[kindId] ?? String(kindId),
		rows, cols, data: cloud,
		ideal, idealRows, idealCols,
		symbolRateHz: scalars.getFloat32(0, true),
		cfoHz: scalars.getFloat32(4, true),
		timingSamples: scalars.getFloat32(8, true),
		evmPercent: scalars.getFloat32(12, true),
		snrDb: scalars.getFloat32(16, true),
		measurements,
	};
}

function decodeAudio(data: ArrayBuffer): AudioFrame | null {
	if (data.byteLength < AUDIO_HEADER_BYTES) return null;
	const v = new DataView(data, 4, 12);
	const seq = v.getUint32(0, true);
	const rate = v.getUint32(4, true);
	const samples = v.getUint32(8, true);
	if (data.byteLength !== AUDIO_HEADER_BYTES + samples * 2) return null;
	return { kind: 'audio', seq, rate, samples, pcm: new Int16Array(data, AUDIO_HEADER_BYTES, samples) };
}
