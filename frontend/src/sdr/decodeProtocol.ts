// SDE2 is the native receiver's bounded, source-clocked event stream.
import type { DrmReceiverStatus } from './drmLog';
import type { RadiofaxClockSource, RadiofaxClockStatus, RadiofaxStage } from './radiofaxLog';
import { parseRadiofaxTransmissionId } from './radiofaxLog';
export const MAX_DECODE_BYTES = 2 * 1024 * 1024;
const U64_MAX = (1n << 64n) - 1n;
export interface ReceiverEvent {
	type: string;
	snapshotAck?: boolean;
	text?: string; endsLine?: boolean; share?: number; rms?: number; keyed?: boolean;
	centerHz?: number; pitchHz?: number; afcHz?: number; wpm?: number;
	frequencyHz?: number; timeOffsetS?: number; snrDb?: number; count?: number; windowStart?: string;
	stereo?: boolean; peakDevHz?: number; pilotPct?: number; peakDbFs?: string | number; peakHoldDb?: string;
	rdsLocked?: boolean; rdsPi?: number; rdsGroups?: number; ps?: string; rt?: string;
	carrierHz?: number | null; pilotDevHz?: number; pilotSnrDb?: number; scopeL?: number[]; scopeR?: number[];
	syncEnabled?: boolean; syncLocked?: boolean; amSyncGeneration?: number;
	carrierOffsetHz?: number | null; modulationPct?: number | null; inputDbFs?: number | null;
	rmsDbFs?: number; scope?: number[];
	lines?: string[] | number; points?: { re: number; im: number }[];
	width?: number; height?: number; row?: number; rows?: number; rgb?: string | number[];
	gray?: string; revision?: number;
	meta?: { name?: string; modeIndex?: number; vis?: number; serial?: string | number; hz?: number | null;
		epoch?: string | number; ioc?: number; lpm?: number; transmissionId?: string; segmentIndex?: number;
		lines?: number; progress?: number; complete?: boolean; partial?: boolean; missingRows?: number[]; reason?: string | null };
	running?: boolean; pushes?: number; attempts?: number; bufferedSamples?: number;
	bufferedSeconds?: number; hasPicture?: boolean; level?: number; srcLevel?: number;
	stage?: RadiofaxStage; signalLevel?: number; carrierCoherence?: number; ioc?: 288 | 576 | null;
	lpm?: number | null; modulation?: 'fm' | 'am'; polarity?: 'normal' | 'inverted';
	elapsedSeconds?: number; clockStatus?: RadiofaxClockStatus; clockSource?: RadiofaxClockSource;
	phasePixels?: number; clockPpm?: number; clockConfidence?: number; complete?: boolean; partial?: boolean;
	reason?: string | null; state?: string; index?: number; message?: string;
	status?: DrmReceiverStatus;
}
export interface DecodeEnvelope {
	version: 1;
	channel: number;
	channel_id: number;
	epoch: number;
	seq: number;
	input_index: string;
	input_rate: number;
	input_fraction_num: string;
	input_fraction_den: string;
	delay_seconds: number;
	delay_known: boolean;
	causal_presentation: boolean;
	mode: string;
	event: ReceiverEvent;
}
interface ClockState {
	identity: string;
	epoch: number;
	channelId: number;
	seq: number;
	whole: bigint;
	num: bigint;
	den: bigint;
	rate: number;
	mode: string;
}
function uint64(value: unknown): bigint {
	if (typeof value !== 'string' || !/^(0|[1-9][0-9]{0,19})$/.test(value)) throw new Error('Invalid SDE2 source integer');
	const result = BigInt(value);
	if (result > U64_MAX) throw new Error('SDE2 source integer overflow');
	return result;
}
function record(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function validateEnvelope(value: unknown): asserts value is DecodeEnvelope {
	if (!record(value) || value.version !== 1 || !record(value.event)) throw new Error('Invalid SDE2 envelope');
	for (const key of ['channel', 'channel_id', 'epoch', 'seq']) {
		if (typeof value[key] !== 'number' || !Number.isInteger(value[key]) || value[key] < 0 || value[key] > 0xffffffff) throw new Error('Invalid SDE2 identity');
	}
	if (typeof value.channel !== 'number' || value.channel < 1 || value.channel > 3
		|| typeof value.input_rate !== 'number' || !Number.isFinite(value.input_rate) || value.input_rate <= 0
		|| typeof value.delay_seconds !== 'number' || !Number.isFinite(value.delay_seconds) || value.delay_seconds < 0
		|| typeof value.delay_known !== 'boolean' || typeof value.causal_presentation !== 'boolean'
		|| typeof value.mode !== 'string' || typeof value.event.type !== 'string') throw new Error('Invalid SDE2 envelope');
	if (value.event.type === 'start' && value.event.snapshotAck !== undefined && typeof value.event.snapshotAck !== 'boolean') {
		throw new Error('Invalid receiver snapshotAck');
	}
	const d = value.event;
	for (const key of ['share', 'rms', 'centerHz', 'pitchHz', 'afcHz', 'wpm', 'frequencyHz', 'timeOffsetS', 'snrDb', 'count',
		'peakDevHz', 'pilotPct', 'rdsPi', 'rdsGroups', 'carrierHz', 'pilotDevHz', 'pilotSnrDb', 'width', 'height', 'row', 'rows',
		'pushes', 'attempts', 'bufferedSamples', 'bufferedSeconds', 'level', 'srcLevel', 'index', 'signalLevel',
		'carrierCoherence', 'ioc', 'lpm', 'revision', 'elapsedSeconds', 'phasePixels', 'clockPpm', 'clockConfidence']) {
		if (d[key] !== undefined && d[key] !== null && (typeof d[key] !== 'number' || !Number.isFinite(d[key]))) throw new Error(`Invalid receiver ${key}`);
	}
	for (const key of ['text', 'windowStart', 'peakHoldDb', 'ps', 'rt', 'message', 'modulation', 'polarity']) {
		if (d[key] !== undefined && typeof d[key] !== 'string') throw new Error(`Invalid receiver ${key}`);
	}
	if (d.reason !== undefined && d.reason !== null && typeof d.reason !== 'string') throw new Error('Invalid receiver reason');
	for (const key of ['endsLine', 'keyed', 'stereo', 'rdsLocked', 'running', 'hasPicture', 'complete', 'partial']) {
		if (d[key] !== undefined && typeof d[key] !== 'boolean') throw new Error(`Invalid receiver ${key}`);
	}
	if (d.type === 'am-info') {
		// AM telemetry is strict: sync flags are required booleans, measurements are
		// required and either finite numbers or explicit nulls (never silently zero).
		if (typeof d.syncEnabled !== 'boolean' || typeof d.syncLocked !== 'boolean') throw new Error('Invalid AM sync flags');
		if (typeof d.amSyncGeneration !== 'number' || !Number.isInteger(d.amSyncGeneration)
			|| d.amSyncGeneration < 0 || d.amSyncGeneration > 0xffffffff) throw new Error('Invalid AM sync generation');
		for (const key of ['carrierHz', 'carrierOffsetHz', 'modulationPct', 'inputDbFs']) {
			if (d[key] !== null && (typeof d[key] !== 'number' || !Number.isFinite(d[key]))) throw new Error(`Invalid AM ${key}`);
		}
		for (const key of ['rmsDbFs', 'peakDbFs']) {
			if (typeof d[key] !== 'number' || !Number.isFinite(d[key])) throw new Error(`Invalid AM ${key}`);
		}
		if (!Array.isArray(d.scope) || d.scope.length > 256
			|| d.scope.some((v: unknown) => typeof v !== 'number' || !Number.isFinite(v))) throw new Error('Invalid AM scope');
	} else if (d.peakDbFs !== undefined && typeof d.peakDbFs !== 'string') {
		// WFM still reports peaks as preformatted text.
		throw new Error('Invalid receiver peakDbFs');
	}
	if (d.type === 'radiofax-image') {
		const meta = d.meta;
		if (!record(meta) || typeof meta.lines !== 'number' || !Number.isInteger(meta.lines) || meta.lines < 1 || meta.lines > 2400
			|| !Array.isArray(meta.missingRows) || meta.missingRows.some((row, index, list) =>
				typeof row !== 'number' || !Number.isInteger(row) || row < 0 || row >= Number(meta.lines) || (index > 0 && row <= list[index - 1]))
			|| (meta.reason !== null && typeof meta.reason !== 'string')) throw new Error('Invalid radiofax picture metadata');
		parseRadiofaxTransmissionId(meta.transmissionId);
	}
	if (d.type === 'radiofax-state') {
		if (typeof d.lines !== 'number' || !Number.isInteger(d.lines) || d.lines < 0 || d.lines > 2400) throw new Error('Invalid radiofax lines');
		if (d.reason !== null && typeof d.reason !== 'string') throw new Error('Invalid radiofax reason');
		// Telemetry enums are the receiver's own vocabulary; a future stage must be visible as an
		// error, not silently coerced into a wrong clock summary.
		const radiofaxEnums: [keyof ReceiverEvent, readonly string[]][] = [
			['stage', ['searching', 'fallback', 'apt', 'phasing', 'receiving', 'degraded', 'complete', 'partial']],
			['clockStatus', ['nominal', 'acquiring', 'locked', 'tracking', 'degraded']],
			['clockSource', ['nominal', 'phasing', 'image_content', 'manual']],
		];
		for (const [key, allowed] of radiofaxEnums) {
			if (!allowed.includes(String(d[key]))) throw new Error(`Invalid radiofax ${key}`);
		}
		if (d.ioc !== undefined && d.ioc !== null && d.ioc !== 288 && d.ioc !== 576) throw new Error('Invalid radiofax ioc');
		if (typeof d.clockConfidence !== 'number' || !Number.isFinite(d.clockConfidence)) throw new Error('Invalid receiver clockConfidence');
		if (d.modulation !== 'fm' && d.modulation !== 'am') throw new Error('Invalid radiofax modulation');
		if (d.polarity !== 'normal' && d.polarity !== 'inverted') throw new Error('Invalid radiofax polarity');
	}
	if (d.type === 'drm' && d.lines !== undefined && (!Array.isArray(d.lines) || d.lines.some((line) => typeof line !== 'string'))) throw new Error('Invalid receiver lines');
	if (d.status !== undefined) {
		const s = d.status;
		if (!record(s) || typeof s.locked !== 'boolean' || !Array.isArray(s.services)) throw new Error('Invalid DRM status');
		for (const key of ['mode', 'stationLabel']) {
			if (s[key] !== null && typeof s[key] !== 'string') throw new Error('Invalid DRM station');
		}
		for (const key of ['facOk', 'facErrors', 'sdcOk']) {
			if (typeof s[key] !== 'number' || !Number.isFinite(s[key])) throw new Error('Invalid DRM counters');
		}
		if (s.carrierOffsetHz !== null && (typeof s.carrierOffsetHz !== 'number' || !Number.isFinite(s.carrierOffsetHz))) throw new Error('Invalid DRM carrier');
		for (const service of s.services) {
			if (!record(service) || (service.label !== null && typeof service.label !== 'string')
				|| (typeof service.serviceId !== 'number' && typeof service.serviceId !== 'string')) throw new Error('Invalid DRM service');
			for (const key of ['audioCoding', 'sampleRate', 'channels']) {
				if (service[key] !== null && (typeof service[key] !== 'number' || !Number.isFinite(service[key]))) throw new Error('Invalid DRM service format');
			}
		}
	}
}
/** Stateful per-channel validation: identity changes require a start; loss is fatal. */
export class DecodeSequence {
	private readonly channels = new Map<number, ClockState>();
	accept(data: ArrayBuffer): DecodeEnvelope {
		if (data.byteLength < 8 || data.byteLength - 8 > MAX_DECODE_BYTES) throw new Error('Invalid SDE2 frame size');
		const bytes = new Uint8Array(data);
		if (bytes[0] !== 83 || bytes[1] !== 68 || bytes[2] !== 69 || bytes[3] !== 50
			|| new DataView(data).getUint32(4, true) !== data.byteLength - 8) throw new Error('Invalid SDE2 header');
		const d: unknown = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(8)));
		validateEnvelope(d);
		const whole = uint64(d.input_index), num = uint64(d.input_fraction_num), den = uint64(d.input_fraction_den);
		if (den === 0n || num >= den) throw new Error('Invalid SDE2 source fraction');
		const identity = `${d.epoch}:${d.channel_id}`;
		const previous = this.channels.get(d.channel);
		if (d.seq === 0) {
			if (d.event.type !== 'start' || previous?.identity === identity) throw new Error('Invalid SDE2 stream start');
			if (previous && (d.epoch < previous.epoch || (d.epoch === previous.epoch && d.channel_id < previous.channelId))) {
				throw new Error('Stale SDE2 identity');
			}
		} else {
			if (!previous || previous.identity !== identity || previous.seq + 1 !== d.seq) throw new Error('SDE2 identity or sequence gap');
			if (d.event.type === 'start' || previous.mode !== d.mode) throw new Error('SDE2 mode changed without boundary');
			if (previous.rate !== d.input_rate) throw new Error('SDE2 source rate changed without boundary');
			// FT8 search completes asynchronously and retains its original transmission clock.
			if (!d.causal_presentation && d.event.type !== 'ft8' && (whole - previous.whole) * den * previous.den
				+ num * previous.den - previous.num * den < 0n) throw new Error('SDE2 source clock moved backwards');
		}
		const delayed = d.seq !== 0 && previous && (whole - previous.whole) * den * previous.den
			+ num * previous.den - previous.num * den < 0n;
		this.channels.set(d.channel, delayed
			? { ...previous, seq: d.seq }
			: { identity, epoch: d.epoch, channelId: d.channel_id, seq: d.seq, whole, num, den, rate: d.input_rate, mode: d.mode });
		return d;
	}
}
