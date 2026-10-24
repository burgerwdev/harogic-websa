/**
 * Binary frame decoder — the single TypeScript definition of the wire format.
 *
 * Python and native encoders share the layouts below. Python-generated golden fixtures
 * (`tests/fixtures/frames/*.bin`) lock the TypeScript decoder to the wire contract.
 * All numeric header fields are little-endian. FREQ and POWR share a 16-byte header;
 * the remaining frame types have the explicit layouts shown here.
 *
 *   FREQ  magic + common header + float64[points]   (frequency axis, Hz)
 *   POWR  magic + common header + float32[points]   (power, dBm)
 *   RTAF  magic + ver(u32) pts(u32) wfLen(u16) maxD(u16) startHz(f64)   (8-byte aligned)
 *         + float64[pts] + float32[pts] + uint16[wfLen] + stopHz(f64)
 *   RTA2  RTAF layout + 'TRIG' + JSON byte length(u32) + UTF-8 capture metadata
 *   IQBF/IQAX  magic + ver(u32, 2) seq(u32) samples(u32) rate(f64) centerHz(f64)
 *              + channel_id(u32) epoch(u32) input_index(u64) group_delay(f64)
 *              + float32[2*samples] (interleaved IQ; seq 0 = empty boundary)
 *
 * Lengths are validated strictly: a frame whose declared size does not match the buffer is
 * rejected instead of being interpreted with a wrong stride. Returns null for anything that
 * is not a frame this client understands.
 */
import type { TriggerMetadata } from '../ui/triggerEvents';

export const MAGIC_FREQ = 'FREQ';
export const MAGIC_POWR = 'POWR';
export const MAGIC_RTAF = 'RTAF';
export const MAGIC_RTA2 = 'RTA2';
export const MAGIC_BASEBAND = 'IQBF';
export const MAGIC_AUXILIARY = 'IQAX';

export const COMMON_HEADER_BYTES = 16;      // magic + version + points + sweep_ms
export const RTA_HEADER_BYTES = 24;         // magic + ver + pts + wfLen + maxD + startHz
// The 56-byte header keeps the interleaved float32 payload aligned for a zero-copy view.
export const BASEBAND_HEADER_BYTES = 56;

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
  trigger?: TriggerMetadata;
}

export interface BasebandFrame {
	kind: 'baseband';
	magic: typeof MAGIC_BASEBAND | typeof MAGIC_AUXILIARY;
	version: number;
	/** Frame counter; 0 marks an empty stream boundary. */
	seq: number;
	/** Rate of the channelized baseband (the backend's DDC output rate). */
	rate: number;
	/** Centre the baseband is tuned to (the listen frequency). */
	centerHz: number;
	channelId: number;
	epoch: number;
	/** Raw input clock; u64::MAX means unavailable on the Python vendor DDC path. */
	inputIndex: bigint;
	/** Group delay in output complex samples. */
	groupDelay: number;
	/** Complex samples: `samples` I/Q pairs, interleaved in `iq` (2 float32 each). */
	samples: number;
	iq: Float32Array;
}

export type DecodedFrame = FreqFrame | PowrFrame | RtaFrame | BasebandFrame;

/** ASCII magic of a binary frame, or '' when the buffer is too short. */
export function frameMagic(data: ArrayBuffer): string {
	if (data.byteLength < 4) return '';
	const v = new DataView(data, 0, 4);
	return String.fromCharCode(v.getUint8(0), v.getUint8(1), v.getUint8(2), v.getUint8(3));
}

export function decodeFrame(data: ArrayBuffer): DecodedFrame | null {
	if (data.byteLength < 4) return null;
	const magic = frameMagic(data);
	if (magic === MAGIC_BASEBAND || magic === MAGIC_AUXILIARY) return decodeBaseband(data, magic);
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
	if (magic === MAGIC_RTAF || magic === MAGIC_RTA2) {
		if (data.byteLength < RTA_HEADER_BYTES) return null;
		const hdr = new DataView(data, 4, 20);
		const wfLen = hdr.getUint16(8, true);
		const maxDensity = hdr.getUint16(10, true);
		const startHz = hdr.getFloat64(12, true);
		const expected = RTA_HEADER_BYTES + points * 8 + points * 4 + wfLen * 2 + 8;
		if (points < 2 || wfLen < 1 || data.byteLength < expected) return null;
		let trigger: TriggerMetadata | undefined;
		if (magic === MAGIC_RTAF) {
			if (data.byteLength !== expected) return null;
		} else {
			if (data.byteLength < expected + 8) return null;
			const trailer = new DataView(data, expected);
			if (trailer.getUint32(0, true) !== 0x47495254 || trailer.getUint32(4, true) !== data.byteLength - expected - 8) return null;
			try {
				const value: unknown = JSON.parse(new TextDecoder().decode(new Uint8Array(data, expected + 8)));
				if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
				const record = value as Record<string, unknown>;
				for (const key of ['frames', 'generation', 'capture_id', 'packet_index', 'packet_count']) {
					if (record[key] != null && (typeof record[key] !== 'number' || !Number.isSafeInteger(record[key]) || Number(record[key]) < 0)) return null;
				}
				for (const key of ['waiting', 'complete', 'simulated', 'supported', 'valid', 'trigger_missed']) if (record[key] != null && typeof record[key] !== 'boolean') return null;
				if (record.source != null && typeof record.source !== 'string') return null;
				if (record.last_sdk_status != null && (typeof record.last_sdk_status !== 'number' || !Number.isSafeInteger(record.last_sdk_status))) return null;
				trigger = record;
			} catch { return null; }
		}
		let off = RTA_HEADER_BYTES;
		const freq = new Float64Array(data, off, points);
		off += points * 8;
		const spec = new Float32Array(data, off, points);
		off += points * 4;
		const wfRow = new Uint16Array(data, off, wfLen);
		off += wfLen * 2;
		const stopHz = new DataView(data, off, 8).getFloat64(0, true);
		return { kind: 'rta', version, points, sweepMs, wfLen, maxDensity, startHz, stopHz, freq, spec, wfRow, trigger };
	}
	return null;
}

function decodeBaseband(data: ArrayBuffer, magic: BasebandFrame['magic']): BasebandFrame | null {
	if (data.byteLength < BASEBAND_HEADER_BYTES) return null;
	const v = new DataView(data, 4, BASEBAND_HEADER_BYTES - 4);
	const version = v.getUint32(0, true);
	if (version !== 2) return null;
	const seq = v.getUint32(4, true);
	const samples = v.getUint32(8, true);
	const rate = v.getFloat64(12, true);
	const centerHz = v.getFloat64(20, true);
	const channelId = v.getUint32(28, true);
	const epoch = v.getUint32(32, true);
	const inputIndex = v.getBigUint64(36, true);
	const groupDelay = v.getFloat64(44, true);
	// `samples` is complex samples, so the payload is twice as wide as the count suggests.
	if (data.byteLength !== BASEBAND_HEADER_BYTES + samples * 8 || (seq === 0) !== (samples === 0)) return null;
	return { kind: 'baseband', magic, version, seq, rate, centerHz, channelId, epoch, inputIndex, groupDelay, samples,
		iq: new Float32Array(data, BASEBAND_HEADER_BYTES, samples * 2) };
}
