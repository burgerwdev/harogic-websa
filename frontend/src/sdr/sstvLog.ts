// The SSTV picture store: the worker's ingress fills it, the decode window renders it.
//
// Shape: one live picture (`current`, growing), finished pictures in `history` (newest first, capped),
// and the pane's view state. One transmission is one *serial*; entries are keyed by `serial-mode`, so
// the same picture growing across reports updates in place, and a finished one moves to the history.
//
// Viewing: `activeMode` picks the mode whose picture set the pane shows (null = the live picture),
// `viewId` pins one entry. Reception is independent of both: nothing the operator does here touches
// the decoder. `selected` lists the entries ticked for deletion.

import type { SstvDecoderState } from './types';

export interface SstvPicture {
	/** RGB888, row-major, `width * height * 3` bytes. */
	rgb: Uint8Array;
	width: number;
	height: number;
}

/** What the decoder reported about one picture. */
export interface SstvMeta {
	/** Mode display name, from the Rust mode table (`sstv_mode_table`). */
	name: string;
	/** Index into that table: what the mode buttons key off. */
	modeIndex: number;
	/** The VIS code the calibration header carried. */
	vis: number;
	width: number;
	height: number;
	/** Scanlines decoded so far. */
	lines: number;
	/** `lines / height` as a whole percentage, 0..100. */
	progress: number;
	/** True once the last scanline arrived. */
	complete: boolean;
	/** The decoder's transmission counter (see the note above). */
	serial: number | string;
	/** Receiver stream epoch: serials restart when a stream changes. */
	epoch?: string;
	/** Wall clock (epoch ms) the transmission was first seen. */
	at: number;
	/** Absolute frequency the receiver was tuned to, in Hz. */
	hz: number;
}

/** One stored picture. The id is stable across the picture's growth. */
export interface SstvEntry {
	id: string;
	picture: SstvPicture;
	meta: SstvMeta;
	selected: boolean;
}

export interface SstvStatus {
	current: SstvEntry | null;
	history: SstvEntry[];
	/** Which mode's picture set the pane shows: null = the live picture, whatever its mode. */
	activeMode: number | null;
	/** The entry the pane draws: null = the newest of the active set. */
	viewId: string | null;
	decoder: SstvDecoderState | null;
}

/** How many finished pictures are kept: a picture is ~245 kB, so this bounds the pane's memory. */
export const SSTV_HISTORY_MAX = 6;

/**
 * The audio band the decoder's own front end reads, in Hz above the dial. These are the plugin's
 * `AUDIO_LO_HZ`/`AUDIO_HI_HZ` (`wasm/src/digital/sstv/mod.rs`) and they are the authority: the
 * panadapter draws this as the SSTV passband.
 */
export const SSTV_AUDIO_LOW_HZ = 300;
export const SSTV_AUDIO_HIGH_HZ = 2_700;

/** Where the picture's own tones sit inside that band: sync, black, white. */
export const SSTV_SYNC_HZ = 1_200;
export const SSTV_BLACK_HZ = 1_500;
export const SSTV_WHITE_HZ = 2_300;

/** The band the panadapter highlights for SSTV: the decoder's own front-end band. */
export function sstvBandHz(): [number, number] {
	return [SSTV_AUDIO_LOW_HZ, SSTV_AUDIO_HIGH_HZ];
}

const emptyStatus = (): SstvStatus => ({ current: null, history: [], activeMode: null, viewId: null, decoder: null });
const contexts: Record<1 | 2 | 3, SstvStatus> = { 1: emptyStatus(), 2: emptyStatus(), 3: emptyStatus() };
let selectedChannel: 1 | 2 | 3 = 1;
let status = contexts[selectedChannel];
const listeners = new Set<() => void>();

function notify(): void {
	contexts[selectedChannel] = status;
	for (const listener of listeners) listener();
}

export function setSstvChannel(channel: 1 | 2 | 3): void {
	selectedChannel = channel;
	status = contexts[channel];
	notify();
}


/**
 * Record the decoder's latest picture. The first sighting dates the transmission and fixes its
 * frequency; later reports of the same serial update the entry in place. A completed picture that is
 * followed by a new serial moves to the history.
 */
export function updateSstvPicture(picture: SstvPicture, meta: Omit<SstvMeta, 'at' | 'hz'> & { hz: number }, channel: 1 | 2 | 3 = selectedChannel): void {
	const context = contexts[channel];
	const previous = context.current;
	const id = JSON.stringify([meta.epoch ?? '', String(meta.serial), meta.modeIndex]);
	const grew = previous !== null && previous.id === id;
	const entry: SstvEntry = {
		id, picture,
		meta: { ...meta, at: grew ? previous.meta.at : Date.now(), hz: grew ? previous.meta.hz : meta.hz },
		selected: grew ? previous.selected : false,
	};
	const history = !grew && previous ? [previous, ...context.history].slice(0, SSTV_HISTORY_MAX) : context.history;
	contexts[channel] = { ...context, current: entry, history };
	if (channel === selectedChannel) {
		status = contexts[channel];
		notify();
	}
}

/** Show the live picture (`null`) or one stored entry by id. */
export function setSstvView(viewId: string | null): void {
	if (status.viewId === viewId) return;
	status = { ...status, viewId };
	notify();
}

/** Show one mode's picture set (`null` = the live picture, whatever its mode). */
export function setSstvModeFilter(activeMode: number | null): void {
	if (status.activeMode === activeMode) return;
	status = { ...status, activeMode, viewId: null };
	notify();
}

/** Tick or untick one entry for deletion. */
export function setSstvSelected(id: string, selected: boolean): void {
	const apply = (entry: SstvEntry): SstvEntry =>
		entry.id === id ? { ...entry, selected } : entry;
	const touched =
		(status.current && status.current.id === id) ||
		status.history.some((entry) => entry.id === id);
	if (!touched) return;
	status = {
		...status,
		current: status.current ? apply(status.current) : null,
		history: status.history.map(apply),
	};
	notify();
}

/** The entry the pane draws: the viewed one, else the newest of the active set, else the live one. */
export function sstvVisible(): SstvEntry | null {
	const set = sstvActiveSet();
	if (status.viewId) {
		const viewed = set.find((entry) => entry.id === status.viewId);
		if (viewed) return viewed;
	}
	return set[0] ?? null;
}

/** The pictures of the active mode, newest first (the live one first when it matches). */
export function sstvActiveSet(): SstvEntry[] {
	const all = status.current ? [status.current, ...status.history] : [...status.history];
	if (status.activeMode === null) return all;
	return all.filter((entry) => entry.meta.modeIndex === status.activeMode);
}

/** Pictures stored per mode index (the mode buttons' badges). */
export function sstvModeCounts(): Map<number, number> {
	const counts = new Map<number, number>();
	const add = (modeIndex: number) => counts.set(modeIndex, (counts.get(modeIndex) ?? 0) + 1);
	if (status.current) add(status.current.meta.modeIndex);
	for (const entry of status.history) add(entry.meta.modeIndex);
	return counts;
}

/** Pictures seen this session. */
export function sstvCount(): number {
	return status.history.length + (status.current ? 1 : 0);
}

/** Delete the ticked entries. The live picture is never deleted here (Clear handles it). */
export function deleteSstvSelected(): void {
	const dropped = new Set(
		[status.current, ...status.history]
			.filter((entry): entry is SstvEntry => entry !== null && entry.selected)
			.map((entry) => entry.id),
	);
	if (dropped.size === 0) return;
	status = {
		...status,
		current: status.current && !dropped.has(status.current.id) ? status.current : null,
		history: status.history.filter((entry) => !dropped.has(entry.id)),
	};
	notify();
}

/** Delete every picture of one mode (the current one included). */
export function deleteSstvMode(modeIndex: number): void {
	const current = status.current && status.current.meta.modeIndex === modeIndex ? null : status.current;
	status = {
		...status,
		current,
		history: status.history.filter((entry) => entry.meta.modeIndex !== modeIndex),
		viewId: null,
		decoder: current ? status.decoder : null,
	};
	notify();
}

/** Delete everything. */
export function deleteSstvAll(channel: 1 | 2 | 3 = selectedChannel): void {
	const context = contexts[channel];
	if (!context.current && context.history.length === 0 && !context.decoder) return;
	contexts[channel] = { ...context, current: null, history: [], viewId: null, decoder: null };
	if (channel === selectedChannel) {
		status = contexts[channel];
		notify();
	}
}

export function clearSstv(channel: 1 | 2 | 3 = selectedChannel): void {
	deleteSstvAll(channel);
}

export function sstvStatus(): SstvStatus {
	return status;
}

/** Record what the decoder is doing (the worker's own counters, once per stats round). */
export function setSstvDecoderState(decoder: SstvDecoderState | null, channel: 1 | 2 | 3 = selectedChannel): void {
	const previous = contexts[channel].decoder;
	if (previous && decoder && previous.running === decoder.running && previous.pushes === decoder.pushes && previous.attempts === decoder.attempts && previous.bufferedSamples === decoder.bufferedSamples && previous.bufferedSeconds === decoder.bufferedSeconds && previous.hasPicture === decoder.hasPicture && previous.level === decoder.level && previous.srcLevel === decoder.srcLevel) return;
	contexts[channel] = { ...contexts[channel], decoder };
	if (channel === selectedChannel) {
		status = contexts[channel];
		notify();
	}
}

export function subscribeSstv(listener: () => void): () => void {
	listeners.add(listener);
	return () => {
		listeners.delete(listener);
	};
}
