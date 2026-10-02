// The DRM decode status: what the DRM receiver locked and decoded, as a small readout rather than
// a growing log (unlike FT8/CW, a DRM broadcast is continuous, so the panel shows the *current*
// service, not a history).
//
// A leaf module like `cwLog`: the worker's ingress fills it, the decode window renders it, and
// neither needs to know about the other.

export interface DrmStatus {
	/** Decoded metadata lines (mode/bandwidth, station label, FAC SNR). */
	lines: string[];
	/** FAC SNR in dB, when measured. */
	snrDb: number | null;
	/** Equalised FAC 4-QAM constellation points (I, Q). */
	constellation: { re: number; im: number }[];
}

let status: DrmStatus = { lines: [], snrDb: null, constellation: [] };
const listeners = new Set<() => void>();

function notify(): void {
	for (const fn of listeners) fn();
}

/** Record a fresh decode (the receiver locked and produced metadata). */
export function setDrmDecode(lines: string[], snrDb: number | null): void {
	status = { ...status, lines, snrDb };
	notify();
}

/** Replace the constellation points (posted once, with the first decode). */
export function setDrmConstellation(points: { re: number; im: number }[]): void {
	status = { ...status, constellation: points };
	notify();
}

/** Clear everything (the operator cleared the window or left the mode). */
export function clearDrm(): void {
	status = { lines: [], snrDb: null, constellation: [] };
	notify();
}

export function drmStatus(): DrmStatus {
	return status;
}

export function subscribeDrm(fn: () => void): () => void {
	listeners.add(fn);
	return () => {
		listeners.delete(fn);
	};
}
