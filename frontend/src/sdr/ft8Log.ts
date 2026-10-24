// The FT8 decode log: what the protocol decoded, newest first.
//
// The worker reports one transmission at a time (text plus the timing it was found at); this keeps
// the recent ones so the operator can read a band's worth of traffic instead of the last line. A
// leaf module on purpose: the worker's ingress fills it, the window renders it, and neither needs to
// know about the other.
import type { Ft8Report } from './types';

export interface Ft8Spot {
	/** Wall clock (epoch ms) the decode arrived - the operator's own clock, for the table's order. */
	at: number;
	/** Absolute frequency of tone 0 in Hz: the channel's centre plus the audio offset. */
	hz: number;
	/** The decoded text (`CQ JO1WKO PM95`). */
	text: string;
	/** Sync-correlation SNR estimate in dB (a diagnostic, not a calibrated measurement). */
	snrDb: number;
	/** Audio offset inside the channel, in Hz (the `DF` an FT8 operator reads). */
	offsetHz: number;
	/** Where the transmission started relative to the slot, in seconds. */
	timeOffsetS: number;
	/** Backend stream and source window identity, when available. */
	sourceKey?: string;
	windowStart?: string;
}

/**
 * Where the decoder looks for tones, in Hz of audio **above the dial** (the FT8 band).
 *
 * The protocol puts its eight tones across this band, so it is a property of FT8 and not of the UI's
 * IF filter: that filter sets the channel the device hands over and the analog modes' audio passband,
 * while the decoder reads the whole channelized baseband either way.
 *
 * These limits match the decoder's `F_MIN_HZ`/`F_MAX_HZ` (`wasm/src/digital/ft8/mod.rs`).
 * The panadapter uses the same band for `demodBandHz`. Decoder audio frequencies are
 * measured above the listen frequency, never symmetrically about it.
 */
export const FT8_SEARCH_LOW_HZ = 100;
export const FT8_SEARCH_HIGH_HZ = 3_000;
/** The frequency inside that band a clicked decode is parked at (WSJT-X does the same). */
export const FT8_TUNE_HZ = 1_000;

/**
 * Tune below the decoded signal by `FT8_TUNE_HZ` so it lies inside the decoder search band.
 */
export function ft8DialFor(signalHz: number): number {
	return signalHz - FT8_TUNE_HZ;
}

/** How many decodes are kept (a band's worth of traffic; the window scrolls). */
export const MAX_FT8_SPOTS = 200;

const contexts: Record<1 | 2 | 3, Ft8Spot[]> = { 1: [], 2: [], 3: [] };
let selectedChannel: 1 | 2 | 3 = 1;
const listeners = new Set<(spots: Ft8Spot[]) => void>();

export interface Ft8DecoderState {
	bufferedSamples: number;
	bufferedSeconds: number;
	attempts: number;
	count: number;
	level: number;
	epoch: number;
}
const decoderStates: Record<1 | 2 | 3, Ft8DecoderState | null> = { 1: null, 2: null, 3: null };

export function setFt8DecoderState(state: Ft8DecoderState | null, channel: 1 | 2 | 3 = selectedChannel): void {
	decoderStates[channel] = state;
	if (channel === selectedChannel) notify();
}

export function ft8DecoderState(): Ft8DecoderState | null {
	return decoderStates[selectedChannel];
}

function notify(): void {
	for (const listener of listeners) listener(contexts[selectedChannel]);
}

export function setFt8Channel(channel: 1 | 2 | 3): void {
	selectedChannel = channel;
	notify();
}

/** Record one decode (newest first). Returns the spot that was appended. */
export function addFt8Spot(report: Ft8Report, at = Date.now(), channel: 1 | 2 | 3 = selectedChannel): Ft8Spot {
	const spot: Ft8Spot = {
		at,
		hz: (Number(report.centerHz) || 0) + (Number(report.frequencyHz) || 0),
		text: String(report.text || ''),
		snrDb: Number(report.snrDb) || 0,
		offsetHz: Number(report.frequencyHz) || 0,
		timeOffsetS: Number(report.timeOffsetS) || 0,
		sourceKey: report.sourceKey,
		windowStart: report.windowStart,
	};
	const spots = contexts[channel];
	if (spot.sourceKey !== undefined && spot.windowStart !== undefined) {
		const existing = spots.find((item) => item.sourceKey === spot.sourceKey && item.windowStart === spot.windowStart && item.text === spot.text && item.hz === spot.hz);
		if (existing) return existing;
	}
	contexts[channel] = [spot, ...spots].slice(0, MAX_FT8_SPOTS);
	if (channel === selectedChannel) notify();
	return spot;
}

/** The decodes, newest first. */
export function ft8Spots(): Ft8Spot[] {
	return contexts[selectedChannel];
}

export function clearFt8Spots(channel: 1 | 2 | 3 = selectedChannel): void {
	if (contexts[channel].length === 0) return;
	contexts[channel] = [];
	if (channel === selectedChannel) notify();
}

/** Watch the log (the window re-renders); returns the unsubscribe function. */
export function subscribeFt8Spots(listener: (spots: Ft8Spot[]) => void): () => void {
	listeners.add(listener);
	return () => {
		listeners.delete(listener);
	};
}

/** Clock for the table (`12:34:56`), in UTC or the operator's local time. */
export function clock(at: number, utc: boolean): string {
	const d = new Date(at);
	const pad = (value: number) => String(value).padStart(2, '0');
	const h = utc ? d.getUTCHours() : d.getHours();
	const m = utc ? d.getUTCMinutes() : d.getMinutes();
	const s = utc ? d.getUTCSeconds() : d.getSeconds();
	return `${pad(h)}:${pad(m)}:${pad(s)}`;
}
