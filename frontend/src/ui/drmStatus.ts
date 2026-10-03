// The DRM readout: station, robustness mode, bitrate and sync, as decoded by the backend.
//
// DRM is demodulated by the external Dream process (`web_sa/measurements/sdr.py`), so there is no
// browser kernel to read a decode from: the backend publishes the metadata in STATUS under
// `sdr.drm` and this module renders it. Kept in its own file (not controls.ts) so it can be unit
// tested without the whole control layer.

/** Minimal shape of the `sdr.drm` STATUS field. */
export interface DrmStatus {
	active?: boolean;
	station?: string;
	robustness?: string;
	bandwidth_khz?: number;
	bitrate_kbps?: number;
	audio_codec?: string;
	audio_mode?: string;
	sync?: boolean;
	snr_db?: number;
	mer_db?: number;
	error?: string;
}

/**
 * Render (or hide) the DRM row from a STATUS `sdr` object.
 *
 * The row exists only while the Dream decoder is active, so leaving the mode clears it instead of
 * leaving a stale station label on screen.
 */
export function renderDrmStatus(sdr: { drm?: DrmStatus } | null | undefined): void {
	const row = document.getElementById('drm-status-row');
	const el = document.getElementById('cur-drm-status');
	if (!row || !el) return;
	const drm = (sdr && sdr.drm) || {};
	const active = !!drm.active;
	row.style.display = active ? '' : 'none';
	if (!active) {
		el.textContent = '';
		return;
	}
	const parts: string[] = [];
	if (drm.station) parts.push(String(drm.station));
	if (drm.robustness) parts.push(`Mode ${drm.robustness}`);
	if (drm.bandwidth_khz) parts.push(`${Number(drm.bandwidth_khz)} kHz`);
	if (drm.bitrate_kbps) parts.push(`${Number(drm.bitrate_kbps).toFixed(1)} kbps`);
	if (drm.audio_codec) parts.push(String(drm.audio_codec));
	if (drm.snr_db) parts.push(`SNR ${Number(drm.snr_db).toFixed(1)} dB`);
	if (drm.mer_db) parts.push(`MER ${Number(drm.mer_db).toFixed(1)} dB`);
	parts.push(drm.sync ? 'SYNC' : 'no sync');
	if (drm.error) parts.push(String(drm.error));
	el.textContent = parts.join(' \u00b7 ');
}
