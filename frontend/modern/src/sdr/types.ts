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

/** The wrapper's own module type, re-exported so callers need one import. */
export type { DspModule };
