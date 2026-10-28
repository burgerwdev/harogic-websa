// Gray8 storage segments belong to a logical page identified by stream epoch and transmission.
// Images remain session-local through receiver resets; explicit deletion and budget eviction remove them.
export const RADIOFAX_CHANNEL_BYTES = 20 * 1024 * 1024;
export const RADIOFAX_AUDIO_LOW_HZ = 300;
export const RADIOFAX_AUDIO_HIGH_HZ = 2_700;
export function radiofaxBandHz(): [number, number] {
	return [RADIOFAX_AUDIO_LOW_HZ, RADIOFAX_AUDIO_HIGH_HZ];
}

export interface RadiofaxPicture { gray: Uint8Array; width: number; height: number }
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
	/** Cumulative sorted unique LOCAL erased row indices, each less than lines. */
	missingRows: number[];
	reason: string | null;
	/** Browser first-received time, not station transmission time. */
	at: number;
}
export interface RadiofaxEntry {
	id: string;
	picture: RadiofaxPicture;
	meta: RadiofaxMeta;
	revision: number;
}
export interface RadiofaxPage {
	id: string;
	segments: RadiofaxEntry[];
	at: number;
	lines: number;
	complete: boolean;
	partial: boolean;
	reason: string | null;
	/** Missing rows with offsets from preceding segments applied. */
	missingRows: number[];
}
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
	/** Latest receiving segment; historical revisions do not change it. */
	current: RadiofaxEntry | null;
	/** Earlier segments, newest first. */
	history: RadiofaxEntry[];
	/** Logical pages, newest first; segments within each page are ordered. */
	pages: RadiofaxPage[];
	/** Pinned logical page id; null follows current. */
	viewId: string | null;
	decoder: RadiofaxDecoderState | null;
	/** Allocated Gray8 capacity, including spare rows. */
	bytes: number;
	evicted: number;
	retention: 'ok' | 'blocked';
	retentionReason: string | null;
	currentSuppressed: boolean;
}
export interface RadiofaxRows { width: number; row: number; rows: number; gray: Uint8Array; revision: number }
export type RadiofaxPageMeta = Omit<RadiofaxMeta, 'at'>;
/** Radiofax transmission counters preserve the full Rust u64 range on the wire. */
export function parseRadiofaxTransmissionId(value: unknown): bigint {
	if (typeof value !== 'string' || !/^(0|[1-9][0-9]{0,19})$/.test(value)) throw new Error('Invalid radiofax transmission id');
	const transmission = BigInt(value);
	if (transmission > 18_446_744_073_709_551_615n) throw new Error('Radiofax transmission id overflow');
	return transmission;
}
type Channel = 1 | 2 | 3;
interface Context {
	status: RadiofaxStatus;
	livePageId: string | null;
	suppressedPageId: string | null;
	blockedPageId: string | null;
	latestIdentity: string | null;
	latestTransmission: bigint;
}
const newContext = (): Context => ({
	status: { current: null, history: [], pages: [], viewId: null, decoder: null, bytes: 0,
		evicted: 0, retention: 'ok', retentionReason: null, currentSuppressed: false },
	livePageId: null, suppressedPageId: null, blockedPageId: null,
	latestIdentity: null, latestTransmission: -1n,
});
const contexts: Record<Channel, Context> = { 1: newContext(), 2: newContext(), 3: newContext() };
const blockedPages = new WeakSet<RadiofaxPage>();
// A revised prefix may be newer than the retained tail. Track transport progress
// separately from visible height so preserving old rows cannot hide wire gaps.
const receivedRows = new WeakMap<RadiofaxEntry, number>();
let selectedChannel: Channel = 1;
const listeners = new Set<() => void>();
function publish(channel: Channel): void {
	if (channel === selectedChannel) for (const listener of listeners) listener();
}

/** Refresh summaries without changing page order or the pinned view. */
function refresh(context: Context): void {
	const state = context.status;
	let bytes = 0;
	for (const page of state.pages) {
		let lines = 0;
		const missingRows: number[] = [];
		let reason: string | null = null;
		for (const entry of page.segments) {
			bytes += entry.picture.gray.buffer.byteLength;
			for (const row of entry.meta.missingRows) missingRows.push(lines + row);
			lines += entry.picture.height;
			if (entry.meta.reason !== null && entry.meta.reason !== 'segment_limit') reason = entry.meta.reason;
		}
		page.lines = lines;
		page.missingRows = missingRows;
		page.reason = page.segments.some((entry) => entry.meta.reason === 'snapshot_prefix_missing') ? 'snapshot_prefix_missing' : reason;
		page.complete = page.segments.at(-1)!.meta.complete;
		page.partial = page.segments.some((entry) => entry.meta.partial || entry.meta.reason === 'snapshot_prefix_missing');
	}
	state.bytes = bytes;
	state.history = state.pages.flatMap((page) => page.segments.slice().reverse()).filter((entry) => entry.id !== state.current?.id);
	if (state.viewId && !state.pages.some((page) => page.id === state.viewId)) state.viewId = null;
	state.currentSuppressed = context.suppressedPageId !== null;
}
export function setRadiofaxChannel(channel: Channel): void {
	selectedChannel = channel;
	publish(channel);
}
export function radiofaxStatus(): RadiofaxStatus {
	return contexts[selectedChannel].status;
}
export function radiofaxVisiblePage(): RadiofaxPage | null {
	const context = contexts[selectedChannel];
	const state = context.status;
	return state.pages.find((page) => page.id === (state.viewId ?? context.livePageId)) ?? state.pages[0] ?? null;
}
export function radiofaxVisible(): RadiofaxEntry | null {
	const page = radiofaxVisiblePage();
	return page?.segments.at(-1) ?? null;
}
export function setRadiofaxView(viewId: string | null): void {
	const state = contexts[selectedChannel].status;
	const next = viewId && state.pages.some((page) => page.id === viewId) ? viewId : null;
	if (state.viewId === next) return;
	state.viewId = next;
	publish(selectedChannel);
}
export function followRadiofaxLatest(): void {
	setRadiofaxView(null);
}

/**
 * Late revisions replace matching segments without becoming live. When budget blocks growth,
 * stop retaining that transmission so later row chunks cannot cause a gap or unbounded buffer.
 */
export function appendRadiofaxRows(chunk: RadiofaxRows, meta: RadiofaxPageMeta, identity: string,
	channel: Channel = selectedChannel, now = Date.now()): void {
	const context = contexts[channel];
	const state = context.status;
	const transmission = parseRadiofaxTransmissionId(meta.transmissionId);
	const pageId = JSON.stringify([identity, meta.transmissionId]);
	const page = state.pages.find((item) => item.id === pageId);
	if (!page && identity === context.latestIdentity && transmission <= context.latestTransmission) return;
	if (pageId === context.suppressedPageId || pageId === context.blockedPageId) return;
	if (page && blockedPages.has(page)) return;
	const id = JSON.stringify([identity, meta.serial, meta.transmissionId, meta.segmentIndex]);
	const previous = page?.segments.find((entry) => entry.id === id);
	if (previous && chunk.revision < previous.revision) throw new Error('Stale radiofax image revision');
	if (previous && previous.picture.width !== chunk.width) throw new Error('Radiofax image width changed');
	const replace = !previous || chunk.revision > previous.revision;
	const nextRow = replace ? 0 : receivedRows.get(previous!) ?? previous!.picture.height;
	if (chunk.row > nextRow) throw new Error('Radiofax picture row gap');
	if (replace && chunk.row !== 0) throw new Error('Radiofax picture missing first rows');
	if (chunk.rows === 0) return;
	if (!replace && previous!.meta.missingRows.some((row) => row < Math.min(meta.lines, nextRow) && !meta.missingRows.includes(row))) {
		throw new Error('Radiofax missing rows regressed');
	}
	// Record new paper before attempting retention, even if the budget refuses it.
	if (!page) {
		context.latestIdentity = identity;
		context.latestTransmission = transmission;
	}
	const received = Math.max(nextRow, chunk.row + chunk.rows);
	const height = Math.max(previous?.picture.height ?? 0, received);
	const needed = height * chunk.width;
	const oldCapacity = previous?.picture.gray.buffer.byteLength ?? 0;
	// Revisions reuse capacity; growth never reserves rows beyond the 2400-line wire segment.
	let capacity = oldCapacity >= needed ? oldCapacity
		: Math.max(needed, Math.min(RADIOFAX_CHANNEL_BYTES, chunk.width * 2400,
			oldCapacity ? oldCapacity * 2 : chunk.width * 256));
	const newPage = !page;
	const newLive = newPage || pageId === context.livePageId;
	const liveId = newPage ? pageId : context.livePageId;
	let remaining = state.bytes - oldCapacity;
	const dropped = new Set<string>();
	// Prefer exact capacity over unnecessary slack when it would consume protected-page space.
	if (remaining + capacity > RADIOFAX_CHANNEL_BYTES) capacity = needed;
	for (const candidate of state.pages.slice().reverse()) {
		if (remaining + capacity <= RADIOFAX_CHANNEL_BYTES) break;
		if (candidate.id === liveId || candidate.id === state.viewId || candidate.id === pageId) continue;
		remaining -= candidate.segments.reduce((sum, entry) => sum + entry.picture.gray.buffer.byteLength, 0);
		dropped.add(candidate.id);
	}
	if (remaining + capacity > RADIOFAX_CHANNEL_BYTES) {
		context.blockedPageId = pageId;
		if (page) blockedPages.add(page);
		state.retention = 'blocked';
		state.retentionReason = 'memory limit';
		if (page) for (const entry of page.segments) {
			entry.meta = { ...entry.meta, complete: false, partial: true, reason: 'memory limit' };
		}
		refresh(context);
		publish(channel);
		return;
	}
	const buffer = oldCapacity >= capacity && previous ? previous.picture.gray.buffer : new ArrayBuffer(capacity);
	const gray = new Uint8Array(buffer, 0, needed);
	if (previous && buffer !== previous.picture.gray.buffer) gray.set(previous.picture.gray);
	gray.set(chunk.gray, chunk.row * chunk.width);
	const entry: RadiofaxEntry = { id, picture: { gray, width: chunk.width, height },
		meta: { ...meta, lines: height, complete: meta.complete && received >= height,
			partial: meta.partial && received >= height, missingRows: previous
			? [...meta.missingRows, ...previous.meta.missingRows.filter((row) => row >= meta.lines)]
			: meta.missingRows.slice(), at: previous?.meta.at ?? now }, revision: chunk.revision };
	receivedRows.set(entry, received);
	state.pages = state.pages.filter((item) => !dropped.has(item.id));
	state.evicted += dropped.size;
	if (page) {
		page.segments = previous ? page.segments.map((item) => item.id === id ? entry : item)
			: [...page.segments, entry].sort((a, b) => a.meta.segmentIndex - b.meta.segmentIndex);
	} else {
		state.pages.unshift({ id: pageId, segments: [entry], at: now, lines: 0, complete: false,
			partial: false, reason: null, missingRows: [] });
	}
	if (newLive && (!page || state.current?.id === id
		|| (!previous && (!state.current || meta.segmentIndex > state.current.meta.segmentIndex)))) {
		context.livePageId = pageId;
		state.current = entry;
	}
	if (pageId !== context.suppressedPageId && newPage) context.suppressedPageId = null;
	if (newPage) {
		context.blockedPageId = null;
		state.retention = 'ok';
		state.retentionReason = null;
	}
	refresh(context);
	publish(channel);
}
export function setRadiofaxDecoderState(decoder: RadiofaxDecoderState | null, channel: Channel = selectedChannel): void {
	contexts[channel].status.decoder = decoder;
	publish(channel);
}
/** Close receiving state and preserve the paper; a fresh stream identity cannot splice into it. */
export function endRadiofaxStream(channel: Channel = selectedChannel): void {
	const context = contexts[channel];
	// Paper appended before a decode worker exists has no live segment, but an open page
	// is still interrupted by the same stream loss.
	for (const page of context.status.pages) {
		for (const entry of page.segments) {
			if (!entry.meta.complete && !entry.meta.partial) {
				entry.meta = { ...entry.meta, partial: true, reason: entry.meta.reason ?? 'stream ended' };
			}
		}
	}
	context.status.decoder = null;
	refresh(context);
	publish(channel);
}
/** Deleting live paper suppresses that transmission until the next one, including snapshots. */
export function deleteRadiofaxPage(pageId: string, channel: Channel = selectedChannel): void {
	const context = contexts[channel];
	const state = context.status;
	if (!state.pages.some((page) => page.id === pageId)) return;
	state.pages = state.pages.filter((page) => page.id !== pageId);
	if (pageId === context.livePageId) {
		context.suppressedPageId = pageId;
		state.current = null;
	}
	if (state.viewId === pageId) state.viewId = null;
	refresh(context);
	publish(channel);
}
/** Explicit clear suppresses the live transmission until the next transmission/stream identity. */
export function clearRadiofax(channel: Channel = selectedChannel): void {
	const previous = contexts[channel];
	const context = newContext();
	context.suppressedPageId = previous.livePageId ?? previous.suppressedPageId;
	context.livePageId = previous.livePageId;
	context.status.currentSuppressed = context.suppressedPageId !== null;
	contexts[channel] = context;
	context.latestIdentity = previous.latestIdentity;
	context.latestTransmission = previous.latestTransmission;
	publish(channel);
}
export function subscribeRadiofax(listener: () => void): () => void {
	listeners.add(listener);
	return () => listeners.delete(listener);
}
