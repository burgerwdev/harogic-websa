/**
 * Pure tuning rules: per-mode step tiers, click-positioning support and grid alignment.
 *
 * The values live in slots owned by sdrState; this module derives defaults and transforms
 * frequencies, so controls and the renderer share one set of rules with no DOM access.
 */
export type TuneTier = 'normal' | 'coarse' | 'fine';

/** Click positioning: raw frequency, nearest grid point, or strongest nearby signal. */
export type LocateMode = 'free' | 'channel' | 'signal';

export function isTuneTier(value: unknown): value is TuneTier {
	return value === 'normal' || value === 'coarse' || value === 'fine';
}

export function isLocateMode(value: unknown): value is LocateMode {
	return value === 'free' || value === 'channel' || value === 'signal';
}

export interface StepTiers { normal: number; coarse: number; fine: number }

/**
 * Tier defaults per demodulation mode (Hz). Independent of the capture width: a mode keeps
 * its own ladder whatever span is captured. Voice band plans set AM/DSB (9/10 kHz channels),
 * SSB (2.7 kHz), CW (100 Hz), NFM/PM (12.5/25 kHz), WFM (100/200 kHz); protocol modes get
 * conservative values near their dial accuracy.
 */
export const STEP_TIERS: Readonly<Record<string, StepTiers>> = {
	am: { normal: 1_000, coarse: 5_000, fine: 100 },
	dsb: { normal: 1_000, coarse: 5_000, fine: 100 },
	usb: { normal: 100, coarse: 1_000, fine: 10 },
	lsb: { normal: 100, coarse: 1_000, fine: 10 },
	cw: { normal: 10, coarse: 100, fine: 1 },
	nfm: { normal: 12_500, coarse: 100_000, fine: 1_000 },
	pm: { normal: 12_500, coarse: 100_000, fine: 1_000 },
	wfm: { normal: 100_000, coarse: 1_000_000, fine: 10_000 },
	ft8: { normal: 10, coarse: 100, fine: 1 },
	sstv: { normal: 1_000, coarse: 10_000, fine: 100 },
	radiofax: { normal: 1_000, coarse: 10_000, fine: 100 },
	drm: { normal: 1_000, coarse: 10_000, fine: 100 },
	drmplus: { normal: 10_000, coarse: 100_000, fine: 1_000 },
};

const UNKNOWN_TIERS: StepTiers = { normal: 1_000, coarse: 10_000, fine: 100 };

/** Tier defaults for one mode; an unlisted mode reads the conservative generic set. */
export function stepTierDefaults(mode: string): StepTiers {
	return STEP_TIERS[mode] ?? UNKNOWN_TIERS;
}

/**
 * Signal positioning searches the live spectrum for a carrier. It is offered only in the
 * analog modes whose channel actually carries one at the tuned frequency.
 */
export function signalLocateSupported(mode: string): boolean {
	return mode === 'am' || mode === 'cw';
}

/**
 * Align to a uniform channel grid: `anchor + round((f - anchor) / spacing) * spacing`.
 * Returns null when the spacing is unusable or the aligned point leaves `bounds`.
 */
export function alignToChannelGrid(
	hz: number,
	spacingHz: number,
	anchorHz: number,
	bounds: { lo: number; hi: number },
): number | null {
	if (!Number.isFinite(hz) || !Number.isFinite(anchorHz) || !(spacingHz >= 1)) return null;
	const aligned = anchorHz + Math.round((hz - anchorHz) / spacingHz) * spacingHz;
	return aligned >= bounds.lo && aligned <= bounds.hi ? aligned : null;
}

/** Signal-search limits: pixels around the pointer, Hz around the click, evidence age. */
export const SNAP_WINDOW_PX = 40;
export const SNAP_RELEASE_PX = 52;
export const SNAP_WINDOW_HZ = 200_000;
export const SNAP_ENVELOPE_MS = 300;
/** Sub-bin interpolation is refused above this capture bin spacing (no fake precision). */
export const SNAP_MAX_BIN_HZ = 5_000;
