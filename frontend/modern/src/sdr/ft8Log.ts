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
}

/**
 * Where the decoder looks for tones, in Hz of audio **above the dial** (the FT8 band).
 *
 * The protocol puts its eight tones across this band, so it is a property of FT8 and not of the UI's
 * IF filter: that filter sets the channel the device hands over and the analog modes' audio passband,
 * while the decoder reads the whole channelized baseband either way.
 *
 * These are the decoder's own `F_MIN_HZ`/`F_MAX_HZ` (`wasm/src/digital/ft8/mod.rs`) and they are the
 * authority: the panadapter draws this band as the FT8 passband (`demodBandHz`), so a value that
 * drifts from the decoder highlights a band the decoder does not read. It said 200 while the decoder
 * said 100 (the lower edge is 100 on purpose: a PlutoSDR's clock error at 411 MHz measures -809 Hz,
 * which pushes a 1 kHz-offset signal under a 200 Hz floor). The lower edge is also what the band
 * *above the dial* starts at - the decoder's audio frequency is measured from the listen frequency,
 * never symmetrically about it.
 */
export const FT8_SEARCH_LOW_HZ = 100;
export const FT8_SEARCH_HIGH_HZ = 3_000;
/** The frequency inside that band a clicked decode is parked at (WSJT-X does the same). */
export const FT8_TUNE_HZ = 1_000;

/**
 * The dial frequency that puts a decoded signal at [`FT8_TUNE_HZ`] inside the decoder's band.
 *
 * Tuning to the signal's own frequency would put it at DC, outside the band the decoder searches -
 * which is how a clicked row used to make a signal undecodable.
 */
export function ft8DialFor(signalHz: number): number {
	return signalHz - FT8_TUNE_HZ;
}

/** How many decodes are kept (a band's worth of traffic; the window scrolls). */
export const MAX_FT8_SPOTS = 200;

let spots: Ft8Spot[] = [];
const listeners = new Set<(spots: Ft8Spot[]) => void>();

function notify(): void {
	for (const listener of listeners) listener(spots);
}

/** Record one decode (newest first). Returns the spot that was appended. */
export function addFt8Spot(report: Ft8Report, at = Date.now()): Ft8Spot {
	const spot: Ft8Spot = {
		at,
		hz: (Number(report.centerHz) || 0) + (Number(report.frequencyHz) || 0),
		text: String(report.text || ''),
		snrDb: Number(report.snrDb) || 0,
		offsetHz: Number(report.frequencyHz) || 0,
		timeOffsetS: Number(report.timeOffsetS) || 0,
	};
	spots = [spot, ...spots].slice(0, MAX_FT8_SPOTS);
	notify();
	return spot;
}

/** The decodes, newest first. */
export function ft8Spots(): Ft8Spot[] {
	return spots;
}

export function clearFt8Spots(): void {
	if (spots.length === 0) return;
	spots = [];
	notify();
}

/** Watch the log (the window re-renders); returns the unsubscribe function. */
export function subscribeFt8Spots(listener: (spots: Ft8Spot[]) => void): () => void {
	listeners.add(listener);
	return () => {
		listeners.delete(listener);
	};
}

/** UTC time of day for the table (`12:34:56`), which is what an FT8 operator schedules on. */
export function utcClock(at: number): string {
	const d = new Date(at);
	const pad = (value: number) => String(value).padStart(2, '0');
	return `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`;
}
