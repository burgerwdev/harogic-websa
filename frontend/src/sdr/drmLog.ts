// The DRM decode status: what the DRM receiver locked and decoded, as a small readout rather than
// a growing log (unlike FT8/CW, a DRM broadcast is continuous, so the panel shows the *current*
// service, not a history).
//
// A leaf module like `cwLog`: the worker's ingress fills it, the decode window renders it, and
// neither needs to know about the other.

export interface DrmService {
	serviceId: number;
	label: string | null;
	audioCoding: number | null;
	sampleRate: number | null;
	channels: number | null;
}

export interface DrmReceiverStatus {
	locked: boolean;
	mode: string | null;
	stationLabel: string | null;
	carrierOffsetHz: number | null;
	facOk: number;
	facErrors: number;
	sdcOk: number;
	services: DrmService[];
}

export interface DrmStatus {
	/** Decoded metadata lines (mode/bandwidth, station label, FAC SNR). */
	lines: string[];
	/** FAC SNR in dB, when measured. */
	snrDb: number | null;
	/** Equalised FAC 4-QAM constellation points (I, Q). */
	constellation: { re: number; im: number }[];
	receiver: DrmReceiverStatus | null;
}

const emptyStatus = (): DrmStatus => ({ lines: [], snrDb: null, constellation: [], receiver: null });
const contexts: Record<1 | 2 | 3, DrmStatus> = { 1: emptyStatus(), 2: emptyStatus(), 3: emptyStatus() };
let selectedChannel: 1 | 2 | 3 = 1;
const listeners = new Set<() => void>();

function notify(): void {
	for (const fn of listeners) fn();
}

export function setDrmChannel(channel: 1 | 2 | 3): void {
	selectedChannel = channel;
	notify();
}

export function setDrmStatus(receiver: DrmReceiverStatus, channel: 1 | 2 | 3 = selectedChannel): void {
	contexts[channel] = { ...contexts[channel], receiver };
	if (channel === selectedChannel) notify();
}

/** Record a fresh decode (the receiver locked and produced metadata). */
export function setDrmDecode(lines: string[], snrDb: number | null, channel: 1 | 2 | 3 = selectedChannel, receiver?: DrmReceiverStatus): void {
	// Empty heartbeat lines mean no new diagnostic snapshot, not lost reception.
	// A real acquisition/search snapshot replaces the old lines; mode changes clear them.
	const previous = contexts[channel];
	contexts[channel] = { ...previous, lines: lines.length ? lines : previous.lines,
		snrDb: Number.isFinite(snrDb) ? snrDb : lines.length ? null : previous.snrDb,
		receiver: receiver ?? previous.receiver };
	if (channel === selectedChannel) notify();
}

/** Replace the constellation points (posted once, with the first decode). */
export function setDrmConstellation(points: { re: number; im: number }[], channel: 1 | 2 | 3 = selectedChannel): void {
	contexts[channel] = { ...contexts[channel], constellation: points };
	if (channel === selectedChannel) notify();
}

/** Clear everything (the operator cleared the window or left the mode). */
export function clearDrm(channel: 1 | 2 | 3 = selectedChannel): void {
	contexts[channel] = emptyStatus();
	if (channel === selectedChannel) notify();
}

export function drmStatus(): DrmStatus {
	return contexts[selectedChannel];
}

export function subscribeDrm(fn: () => void): () => void {
	listeners.add(fn);
	return () => {
		listeners.delete(fn);
	};
}
