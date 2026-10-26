// Radiofax paper is continuous Gray8, not an SSTV RGB frame. The ingress owns protocol
// validation; this store owns page identity, revision replacement, history and memory bounds.

export const RADIOFAX_HISTORY_MAX = 3;
export const RADIOFAX_CHANNEL_BYTES = 20 * 1024 * 1024;
export const RADIOFAX_AUDIO_LOW_HZ = 300;
export const RADIOFAX_AUDIO_HIGH_HZ = 2_700;

export function radiofaxBandHz(): [number, number] {
	return [RADIOFAX_AUDIO_LOW_HZ, RADIOFAX_AUDIO_HIGH_HZ];
}

export interface RadiofaxPicture {
	/** Gray8, row-major. Its height is the number of rows received in this revision. */
	gray: Uint8Array;
	width: number;
	height: number;
}

export interface RadiofaxMeta {
	serial: string;
	epoch: string;
	hz: number | null;
	ioc: number;
	lpm: number;
	lines: number;
	complete: boolean;
	partial: boolean;
	segmentIndex: number;
	transmissionId: string;
	/** Wall-clock milliseconds at which this page segment was first seen. */
	at: number;
}

export interface RadiofaxEntry {
	id: string;
	picture: RadiofaxPicture;
	meta: RadiofaxMeta;
	revision: number;
}

/** State fields are telemetry, not receiver configuration. */
export type RadiofaxStage = 'searching' | 'fallback' | 'apt' | 'phasing' | 'receiving' | 'degraded' | 'complete' | 'partial';
export type RadiofaxClockStatus = 'nominal' | 'acquiring' | 'locked' | 'tracking' | 'degraded';
export type RadiofaxClockSource = 'nominal' | 'phasing' | 'image_content' | 'manual';
export interface RadiofaxDecoderState {
	running: boolean;
	stage: RadiofaxStage;
	hasPicture: boolean;
	signalLevel: number;
	carrierCoherence: number;
	ioc: 288 | 576 | null;
	lpm: number | null;
	modulation: 'fm' | 'am';
	polarity: 'normal' | 'inverted';
	width: number;
	lines: number;
	elapsedSeconds: number;
	clockStatus: RadiofaxClockStatus;
	clockSource: RadiofaxClockSource;
	phasePixels: number;
	clockPpm: number;
	clockConfidence: number;
	complete: boolean;
	partial: boolean;
	reason: string | null;
}

export interface RadiofaxStatus {
	current: RadiofaxEntry | null;
	/** Earlier page segments, newest first. */
	history: RadiofaxEntry[];
	/** A pinned history page. Null follows the latest page. */
	viewId: string | null;
	decoder: RadiofaxDecoderState | null;
	bytes: number;
}

export interface RadiofaxRows {
	width: number;
	row: number;
	rows: number;
	gray: Uint8Array;
	revision: number;
}

export type RadiofaxPageMeta = Omit<RadiofaxMeta, 'at'>;
type Channel = 1 | 2 | 3;

const emptyStatus = (): RadiofaxStatus => ({ current: null, history: [], viewId: null, decoder: null, bytes: 0 });
const contexts: Record<Channel, RadiofaxStatus> = { 1: emptyStatus(), 2: emptyStatus(), 3: emptyStatus() };
let selectedChannel: Channel = 1;
let status = contexts[selectedChannel];
const listeners = new Set<() => void>();
const capacities = new WeakMap<RadiofaxEntry, number>();

function notify(): void {
	contexts[selectedChannel] = status;
	for (const listener of listeners) listener();
}

function publish(channel: Channel): void {
	if (channel !== selectedChannel) return;
	status = contexts[channel];
	notify();
}

function entryBytes(entry: RadiofaxEntry | null): number {
	return entry?.picture.gray.buffer.byteLength ?? 0;
}

function bound(status: RadiofaxStatus): RadiofaxStatus {
	const history = status.history.slice(0, RADIOFAX_HISTORY_MAX);
	let bytes = entryBytes(status.current) + history.reduce((sum, entry) => sum + entryBytes(entry), 0);
	while (history.length && bytes > RADIOFAX_CHANNEL_BYTES) bytes -= entryBytes(history.pop()!);
	return { ...status, history, bytes };
}

export function setRadiofaxChannel(channel: Channel): void {
	selectedChannel = channel;
	status = contexts[channel];
	notify();
}

export function radiofaxStatus(): RadiofaxStatus {
	return status;
}

export function radiofaxVisible(): RadiofaxEntry | null {
	if (status.viewId) {
		const viewed = [status.current, ...status.history].find((entry) => entry?.id === status.viewId);
		if (viewed) return viewed;
	}
	return status.current ?? status.history[0] ?? null;
}

export function setRadiofaxView(viewId: string | null): void {
	if (status.viewId === viewId) return;
	status = { ...status, viewId };
	notify();
}

/** Follow the latest page, or return to live explicitly after browsing history. */
export function followRadiofaxLatest(): void {
	setRadiofaxView(null);
}

/**
 * Append rows to one page segment. A newer revision replaces that segment from row zero;
 * an older revision is stale and rejected. `identity` includes both stream and metadata epochs.
 */
export function appendRadiofaxRows(
	chunk: RadiofaxRows,
	meta: RadiofaxPageMeta,
	identity: string,
	channel: Channel = selectedChannel,
	now = Date.now(),
): void {
	const id = JSON.stringify([identity, meta.serial, meta.transmissionId, meta.segmentIndex]);
	const context = contexts[channel];
	const previous = context.current;
	const samePage = previous?.id === id;
	if (samePage && chunk.revision < previous.revision) throw new Error('Stale radiofax image revision');
	if (samePage && previous.picture.width !== chunk.width) throw new Error('Radiofax image width changed');
	const replace = !samePage || chunk.revision > previous!.revision;
	const nextRow = replace || !previous ? 0 : previous.picture.height;
	if (chunk.row > nextRow) throw new Error('Radiofax picture row gap');
	if (replace && chunk.row !== 0) throw new Error('Radiofax picture missing first rows');
	const height = replace ? chunk.row + chunk.rows : Math.max(nextRow, chunk.row + chunk.rows);
	const bytes = height * chunk.width;
	if (bytes > RADIOFAX_CHANNEL_BYTES) throw new Error('Radiofax channel memory limit');
	const previousCapacity = samePage && !replace ? capacities.get(previous) ?? 0 : 0;
	const capacity = previousCapacity >= bytes ? previousCapacity
		: Math.min(RADIOFAX_CHANNEL_BYTES, Math.max(bytes, replace ? chunk.width * 256 : previousCapacity * 2));
	const reuse = previousCapacity >= bytes && previousCapacity > 0;
	const buffer = reuse && previous ? previous.picture.gray.buffer : new ArrayBuffer(capacity);
	const gray = new Uint8Array(buffer, 0, bytes);
	if (!replace && previous) gray.set(previous.picture.gray.subarray(0, previous.picture.height * chunk.width));
	gray.set(chunk.gray, chunk.row * chunk.width);
	const entry: RadiofaxEntry = {
		id,
		picture: { gray, width: chunk.width, height },
		meta: { ...meta, at: samePage ? previous.meta.at : now },
		revision: chunk.revision,
	};
	const history = !samePage && previous ? [previous, ...context.history] : context.history;
	contexts[channel] = bound({ ...context, current: entry, history });
	capacities.set(entry, capacity);
	publish(channel);
}

export function setRadiofaxDecoderState(decoder: RadiofaxDecoderState | null, channel: Channel = selectedChannel): void {
	contexts[channel] = { ...contexts[channel], decoder };
	publish(channel);
}

export function clearRadiofax(channel: Channel = selectedChannel): void {
	contexts[channel] = emptyStatus();
	publish(channel);
}

export function subscribeRadiofax(listener: () => void): () => void {
	listeners.add(listener);
	return () => listeners.delete(listener);
}
