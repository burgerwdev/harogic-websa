// The SDR pipeline's shared types.
//
// A leaf module on purpose: the worker's ingress (`iqStream`) and the WASM wrapper (`wasmPipeline`)
// both need these, and having one import the other just for a type is what made the import graph
// circular (the architecture guard counts those, cycle baseline 0).
import type { DspModule } from './wasm';

export interface PipelineParams {
	/** Rate of the channelized baseband (`STATUS.sdr.actual.ddc_rate`). */
	fsIn: number;
	/** Rate the consumer plays at: the AudioWorklet's `sampleRate`. */
	outRate: number;
	/** Plugin id (am/dsb/usb/lsb/cw/nfm/wfm/pm, or a digital protocol such as ft8). */
	mode: string;
	ifBw: number;
	pitch: number;
	/** De-emphasis in microseconds for the browser's audio chain (0 = off, < 0 = the mode's default). */
	deemphUs: number;
}

/** A decoded FT8 transmission, as the worker reports it. */
export interface Ft8Report {
	text: string;
	/** Audio frequency of tone 0 inside the channel, in Hz (about 200..3000). */
	frequencyHz: number;
	/** Where the transmission started inside the pushed buffer, in seconds. */
	timeOffsetS: number;
	/** Sync-correlation SNR estimate in dB (a diagnostic, not a calibrated measurement). */
	snrDb: number;
	/** Decodes this session (the readout's counter). */
	count: number;
	/** Centre of the channel the transmission was found in, in Hz (the tuned frequency). */
	centerHz: number;
}

/** What the SSTV decoder reports about the picture it is showing (the ABI's nine words). */
export interface SstvImageStatus {
	width: number;
	height: number;
	/** Scanlines decoded in this picture so far. */
	lines: number;
	/** `lines / height` as a whole percentage. */
	progress: number;
	/** The mode's VIS code. */
	vis: number;
	/** Bumped once per transmission: the same serial is the same picture, growing. */
	serial: number;
	complete: boolean;
	/** Index into the SSTV mode table (the mode buttons' order). */
	modeIndex: number;
}

/** One decoded SSTV picture: RGB888, row-major, `width * height * 3` bytes. */
export interface SstvPicture {
	rgb: Uint8Array;
	width: number;
	height: number;
}

/**
 * What the SSTV decoder is doing while no picture has decoded yet.
 *
 * "No picture" has two very different causes — the decoder is not being fed, or it is fed and finds
 * no signal — and a bench failure looks identical without these numbers.
 */
export interface SstvDecoderCounters {
	/** Decode attempts made so far. */
	attempts: number;
	/** Audio samples the decoder holds towards its next attempt (its own 8 kHz clock). */
	bufferedSamples: number;
	bufferedSeconds: number;
	hasPicture: boolean;
	/** RMS of the buffered audio, 0..1. Zero means the decoder's band passes nothing. */
	level: number;
	/** RMS of the raw baseband at the worker, 0..1 (the frames' own level). Absent before the worker
	 * has measured a block. */
	srcLevel?: number;
}

/** The decoder's state as the page sees it: the counters plus the worker's own two facts. */
export interface SstvDecoderState extends SstvDecoderCounters {
	/** False when the worker has no live pipeline at all (a rejected mode, a stale artifact). */
	running: boolean;
	/** Blocks pushed into the decoder: 0 with `running` true means no baseband is arriving. */
	pushes: number;
}

/** The wrapper's own module type, re-exported so callers need one import. */
export type { DspModule };
