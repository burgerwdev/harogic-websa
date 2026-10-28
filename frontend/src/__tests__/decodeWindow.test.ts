/**
 * The decode window: gated on the demodulator, fed by the FT8 and CW logs, and its rows tune.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { initDecodeWindow, ingestDecodeEvents, decodeChannel, decodeWindowOpen, decodeWindowMode, refreshDecodeWindow, setDecodeActive, setDecodeChannel, setDecodeChannelMode, setDecodeError, setDecodeTiming, setDecodeWindowAvailable, toggleDecodeWindow } from '../ui/decodeWindow';
import { addFt8Spot, clearFt8Spots, clock, ft8Spots, setFt8DecoderState } from '../sdr/ft8Log';
import { addCwText, clearCwLog, cwLog, cwLevel, setCwLevel } from '../sdr/cwLog';
import { clearDrm, drmStatus, setDrmConstellation, setDrmDecode, setDrmStatus } from '../sdr/drmLog';
import { clearSstv, setSstvDecoderState, setSstvModeFilter, setSstvSelected, sstvCount, sstvStatus, updateSstvPicture } from '../sdr/sstvLog';
import { setSstvModeTable } from '../sdr/registry';
import { setUiScale } from '../core/uiScale';
import { setLang, t } from '../core/i18n';
import type { Ft8Report } from '../sdr/types';
import { resetSdrState, sdrSstvMode } from '../ui/sdrState';
import { confirmAmSyncGeneration, setAmInfo, setWfmInfo } from '../ui/decodeWindow';
import type { AmInfo, WfmInfo } from '../ui/decodeWindow';
import type { DecodeEnvelope } from '../sdr/decodeProtocol';
import { sdrAmSync } from '../ui/sdrState';
import type * as DecodeUi from '../ui/decodeWindow';
import type * as RadiofaxStore from '../sdr/radiofaxLog';
import type * as I18nModule from '../core/i18n';
import type { RadiofaxPageMeta } from '../sdr/radiofaxLog';
import appHtml from '../../index.html?raw';

/** The shell the panel ships (index.html); the window only fills `#decode-window-rows`. */
function shell(): void {
	document.body.innerHTML = `
		<button class="btn" id="btn-decode-window" disabled>Off</button>
		<div class="decode-window" id="decode-window" style="display:none;">
			<div class="decode-window-head" id="decode-window-head">
				<span id="decode-window-title">FT8</span><span id="sstv-head-hint" style="display:none;"></span><span class="count" id="decode-window-count">0</span>
				<span id="am-title-badge" role="status" style="display:none;"></span>
				<span id="wfm-title-badge" style="display:none;">MONO</span>
				<span id="cw-meter"><span id="cw-led"></span><i id="cw-vu-fill"></i></span>
				<span class="spacer"></span>
				<input id="decode-window-opacity" type="range" min="35" max="100" step="5" value="100" />
				<button class="btn" id="btn-decode-utc">UTC</button>
				<button class="btn" id="btn-decode-clear">Clear</button>
				<button class="btn" id="btn-decode-close">Close</button>
			</div>
			<div class="decode-sstv-confirm" id="sstv-clear-confirm" role="group" aria-labelledby="sstv-clear-prompt" style="display:none;">
				<span id="sstv-clear-prompt"></span>
				<button type="button" class="btn" id="btn-sstv-delete-all">Delete all</button>
				<button type="button" class="btn" id="btn-sstv-clear-cancel">Cancel</button>
			</div>
			<div class="decode-search-row" id="decode-ft8-search-row" style="display:none;">
				<input class="decode-search" id="decode-ft8-search" type="text" placeholder="Filter messages (regex)" />
			</div>
			<div class="decode-window-body" id="decode-window-body">
				<table id="decode-window-table"><thead><tr>
					<th id="decode-window-time-th" data-sortable data-col="0">Time</th>
					<th data-sortable data-col="1">SNR</th>
					<th data-sortable data-col="2">DF</th>
					<th data-sortable data-col="3">MHz</th>
					<th data-sortable data-col="4">Message</th>
					<th data-sortable data-col="5">Country</th>
					<th data-sortable data-col="6">Location</th>
				</tr></thead><tbody id="decode-window-rows"></tbody></table>
				<div class="decode-cw" id="decode-window-cw" style="display:none;"></div>
				<div id="decode-am" style="display:none;">
					<div class="decode-am-carrier"><span id="am-carrier"></span>
						<label>Synchronous <button class="btn" id="am-sync-toggle" aria-pressed="false" aria-busy="false"></button></label></div>
					<span id="am-carrier-offset"></span><span id="am-modulation"></span>
					<span id="am-input"></span><span id="am-rms"></span><span id="am-peak"></span>
					<canvas id="am-scope" width="320" height="100"></canvas><span id="am-empty"></span>
				</div>
				<div class="decode-rds" id="decode-window-wfm" style="display:none;">
					<div class="decode-rds-service"><strong id="wfm-station">—</strong><span id="wfm-rds-pi">—</span></div>
					<span id="wfm-rt">—</span>
					<dl class="decode-rds-metrics"><div><dt id="wfm-carrier-label">Carrier</dt><dd id="wfm-carrier">—</dd></div>
						<div><dt>Deviation</dt><dd id="wfm-dev">—</dd></div><div><dt>Pilot</dt><dd id="wfm-pilot">—</dd></div></dl>
					<canvas id="wfm-scope" width="360" height="64"></canvas>
					<span id="wfm-audio">—</span>
					<div id="wfm-empty" style="display:none;">Tune to an FM broadcast station.</div>
				</div>
				<div class="decode-drm" id="decode-window-drm" style="display:none;">
					<div class="decode-drm-service"><span id="decode-drm-lock"></span><strong id="decode-drm-station"></strong><span id="decode-drm-status"></span><div id="decode-drm-mode"></div></div>
					<div id="decode-drm-main"><div id="decode-drm-mer"></div><span id="decode-drm-snr"></span>
						<dl id="decode-drm-metrics"><div><span id="decode-drm-fac-ok"></span><span id="decode-drm-fac-err"></span></div><dd id="decode-drm-msc"></dd><dd id="decode-drm-aus"></dd></dl>
						<figure><canvas id="decode-window-drm-plot" width="144" height="144"></canvas></figure>
					</div>
					<details id="decode-drm-details"><code id="decode-drm-debug"></code></details>
				</div>
				<div class="decode-sstv" id="decode-window-sstv" style="display:none;">
					<div class="decode-sstv-bar"><button class="btn" id="sstv-auto">Auto</button><div class="btn-group" id="sstv-mode-group"></div></div>
					<div class="decode-sstv-main">
						<div class="decode-sstv-thumbs" id="sstv-thumbs">
									<div class="decode-sstv-thumb-tools">
										<button class="btn" id="sstv-select-all">All</button>
										<button class="btn" id="btn-sstv-delete">Delete</button>
										<button class="btn" id="btn-sstv-save">Save</button>
									</div>
									<div class="decode-sstv-thumb-list" id="sstv-thumb-list"></div>
								</div>
						<div class="decode-sstv-side">
							<div class="decode-sstv-readout">
								<span id="sstv-mode-name">—</span><span id="sstv-vis">—</span><span id="sstv-size">—</span>
								<span id="sstv-time">—</span><span id="sstv-freq">—</span><span id="sstv-count">—</span><span id="sstv-state" style="display:none;"></span>
							</div>
							<div id="sstv-progress"><i id="sstv-progress-fill"></i><span id="sstv-progress-text">0%</span></div>
							<div class="decode-sstv-view"><canvas id="decode-sstv-canvas" width="320" height="256"></canvas></div>
						</div>
					</div>
				</div>
			</div>
			<div class="decode-resize-layer">
				<div class="decode-rz n" data-decode-rz="n"></div>
				<div class="decode-rz s" data-decode-rz="s"></div>
				<div class="decode-rz w" data-decode-rz="w"></div>
				<div class="decode-rz e" data-decode-rz="e"></div>
				<div class="decode-rz nw" data-decode-rz="nw"></div>
				<div class="decode-rz ne" data-decode-rz="ne"></div>
				<div class="decode-rz sw" data-decode-rz="sw"></div>
				<div class="decode-rz se" data-decode-rz="se"></div>
			</div>
		</div>`;
}

const report = (over: Partial<Ft8Report> = {}): Ft8Report => ({
	text: 'CQ JO1WKO PM95',
	frequencyHz: 1000,
	timeOffsetS: 0.02,
	snrDb: 12,
	count: 1,
	centerHz: 21_074_000,
	...over,
});

const rows = () => Array.from(document.querySelectorAll('#decode-window-rows tr'));
const shown = () => (document.getElementById('decode-window') as HTMLElement).style.display;

beforeEach(() => {
	setDecodeActive(true);
	sdrSstvMode.reset();
	sdrSstvMode.confirm(-1);
	setDecodeChannel(1);
	for (const channel of [1, 2, 3] as const) setDecodeChannelMode(channel, 'dsb');
});

describe('decoder channel selection', () => {
	beforeEach(() => {
		setDecodeChannel(1);
		for (const channel of [1, 2, 3] as const) {
			setDecodeChannelMode(channel, 'dsb');
			setDecodeError(channel, null);
			setFt8DecoderState(null, channel);
			clearFt8Spots(channel);
			clearCwLog(channel);
			clearDrm(channel);
			clearSstv(channel);
		}
		shell();
		vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
	});

	afterEach(() => {
		setDecodeChannel(1);
		for (const channel of [1, 2, 3] as const) setDecodeChannelMode(channel, 'dsb');
		vi.restoreAllMocks();
	});

	it('selects the confirmed mode and retained output for each receiver', () => {
		setDecodeChannelMode(1, 'ft8');
		setDecodeChannelMode(2, 'cw');
		addFt8Spot(report({ text: 'CQ JO1WKO PM95' }), 1000, 1);
		addCwText('CHANNEL TWO', true, 0.8, 2);
		setCwLevel(0.2, true, { wpm: 19, pitchHz: 701, afcHz: -2.5 }, 2);
		toggleDecodeWindow();
		const selected = vi.fn();
		document.addEventListener('websa:decode-channel', selected);
		try {
			expect(document.getElementById('decode-channel')).toBeNull();
			setDecodeChannel(2);
			expect(decodeChannel()).toBe(2);
			expect(decodeWindowMode()).toBe('cw');
			expect(document.getElementById('decode-window-cw')?.textContent).toContain('CHANNEL TWO');
			expect(cwLevel()).toMatchObject({ rms: 0.2, keyed: true, wpm: 19, pitchHz: 701 });
			expect(document.getElementById('cw-telemetry')?.textContent).toBe('19.0 WPM · 701 Hz · AFC -2.5 Hz');
			expect((selected.mock.calls[0][0] as CustomEvent).detail).toEqual({ channel: 2 });
			setDecodeChannelMode(1, 'wfm');
			expect(decodeWindowMode()).toBe('cw');
			setDecodeChannelMode(1, 'ft8');
			setDecodeChannel(1);
			expect(rows()[0].textContent).toContain('CQ JO1WKO PM95');
			expect(cwLog().lines).toEqual([]);
		} finally {
			document.removeEventListener('websa:decode-channel', selected);
		}
	});

	it('deduplicates a source window while retaining later FT8 transmissions and other channels', () => {
		const first = { ...report(), sourceKey: 'epoch:stream', windowStart: '9007199254740993' };
		addFt8Spot(first, 1000, 1);
		addFt8Spot(first, 2000, 1);
		addFt8Spot({ ...first, windowStart: '9007199254740994' }, 3000, 1);
		addFt8Spot(first, 4000, 2);
		expect(ft8Spots().map((spot) => spot.at)).toEqual([3000, 1000]);
		setDecodeChannel(2);
		expect(ft8Spots().map((spot) => spot.at)).toEqual([4000]);
	});

	it('keeps exact source timing and decoder errors isolated across channels', () => {
		setDecodeChannelMode(1, 'ft8');
		setDecodeChannelMode(2, 'cw');
		setDecodeTiming(1, {
			inputIndex: '9007199254740993123', fractionNum: '17', fractionDen: '31',
			inputRate: 48000, delaySeconds: 0, delayKnown: false, causalPresentation: true,
		});
		setDecodeTiming(2, {
			inputIndex: '21', fractionNum: '0', fractionDen: '1',
			inputRate: 48000, delaySeconds: 0.125, delayKnown: true, causalPresentation: false,
		});
		setDecodeError(2, 'Decoder unavailable');
		toggleDecodeWindow();
		const status = document.getElementById('decode-source-timing')!;
		const title = document.getElementById('decode-window-title')!;
		expect(status.style.display).toBe('none');
		expect(status.textContent).toBe('');
		expect(title.title).toContain('9007199254740993123 + 17/31');
		setDecodeChannel(2);
		expect(status.style.display).not.toBe('none');
		expect(status.textContent).toContain('Decoder unavailable');
		expect(status.getAttribute('role')).toBe('alert');
		expect(title.title).toContain('0.125000');
		setDecodeError(2, null);
		expect(status.style.display).toBe('none');
		expect(status.textContent).toBe('');
		setDecodeChannel(1);
		expect(title.title).toContain('9007199254740993123 + 17/31');
		setDecodeTiming(1, {
			inputIndex: '9007199254740993999', fractionNum: '3', fractionDen: '7',
			inputRate: 48000, delaySeconds: 0.02, delayKnown: true, causalPresentation: true,
		});
		expect(title.title).toContain('9007199254740993999 + 3/7');
		expect(title.title).toContain('0.020000');
	});

	it('keeps FT8 receive counters with their channel while preserving the decode history', () => {
		setDecodeChannelMode(1, 'ft8');
		setDecodeChannelMode(2, 'ft8');
		addFt8Spot(report(), 1000, 1);
		setFt8DecoderState({ bufferedSamples: 24000, bufferedSeconds: 0.5, attempts: 3, count: 7, level: 0.2, epoch: 4 }, 1);
		setFt8DecoderState({ bufferedSamples: 48000, bufferedSeconds: 1, attempts: 5, count: 9, level: 0.4, epoch: 6 }, 2);
		toggleDecodeWindow();
		expect(document.getElementById('decode-ft8-state')?.textContent).toContain('Buffer 0.5 s · Attempts 3 · Decodes 7 · Level 20%');
		setDecodeChannel(2);
		expect(document.getElementById('decode-ft8-state')?.textContent).toContain('Buffer 1.0 s · Attempts 5 · Decodes 9 · Level 40%');
		setDecodeChannel(1);
		setFt8DecoderState(null, 1);
		expect(document.getElementById('decode-ft8-state')?.style.display).toBe('none');
		expect(rows()[0].textContent).toContain('CQ JO1WKO PM95');
	});

	it('renders structured DRM services for either DRM mode', () => {
		setDecodeChannelMode(2, 'drmplus');
		setDrmStatus({
			locked: true, mode: 'E', stationLabel: 'VHF RADIO', carrierOffsetHz: -2.5,
			facOk: 11, facErrors: 2, sdcOk: 3,
			services: [{ serviceId: 42, label: 'News', audioCoding: 0, sampleRate: 48000, channels: 2 }],
		}, 2);
		toggleDecodeWindow();
		setDecodeChannel(2);
		expect(document.getElementById('decode-window-title')?.textContent).toBe('DRM+');
		expect(document.getElementById('decode-drm-station')?.textContent).toBe('VHF RADIO');
		expect(document.getElementById('decode-drm-fac-err')?.textContent).toBe('2');
		expect(document.getElementById('decode-drm-services')?.textContent).toContain('News · ID 42 · AAC · 48.0 kHz · 2 ch');
		setDecodeChannel(1);
		expect(drmStatus().receiver).toBeNull();
	});

	it('gates inactive decode without changing wanted visibility or clearing DRM output', () => {
		setDecodeChannelMode(1, 'drm');
		setDrmStatus({ locked: true, mode: 'A', stationLabel: 'RETAINED DRM', carrierOffsetHz: 0,
			facOk: 1, facErrors: 0, sdcOk: 1, services: [] });
		toggleDecodeWindow();
		expect(decodeWindowOpen()).toBe(true);
		setDecodeActive(false);
		expect(decodeWindowOpen()).toBe(false);
		expect((document.getElementById('btn-decode-window') as HTMLButtonElement).disabled).toBe(true);
		toggleDecodeWindow();
		document.getElementById('btn-decode-close')!.click();
		expect(decodeWindowMode()).toBe('drm');
		expect(drmStatus().receiver?.stationLabel).toBe('RETAINED DRM');
		setDecodeActive(true);
		expect(decodeWindowOpen()).toBe(true);
		expect(document.getElementById('decode-drm-station')?.textContent).toBe('RETAINED DRM');
	});
});
describe('the FT8 decode window', () => {
	beforeEach(() => {
		shell();
		clearFt8Spots();
		clearCwLog();
		clearDrm();
		vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
		localStorage.clear();
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
	});

	it('is disabled until FT8 is the demodulator, and hides when it stops being', () => {
		const button = document.getElementById('btn-decode-window') as HTMLButtonElement;
		expect(button.disabled).toBe(true);
		setDecodeWindowAvailable('ft8');
		expect(button.disabled).toBe(false);
		toggleDecodeWindow();
		expect(decodeWindowOpen()).toBe(true);
		expect(shown()).toBe('flex');
		// Switching to an analog demodulator closes it (the table would be meaningless) but the log
		// and the user's chosen position stay.
		setDecodeWindowAvailable('dsb');
		expect(decodeWindowOpen()).toBe(false);
		expect(shown()).toBe('none');
		expect(button.disabled).toBe(true);
	});

	it('shows a waiting row, then one row per decode, newest first', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		expect(document.querySelector('.ft8-empty')).not.toBeNull();
		expect((document.querySelector('.ft8-empty') as HTMLTableCellElement).colSpan).toBe(7);

		const prevTz = process.env.TZ;
		try {
			process.env.TZ = 'UTC';   // pin the zone so the default local clock equals UTC
			addFt8Spot(report({ text: 'CQ JO1WKO PM95', frequencyHz: 1000 }), Date.UTC(2026, 8, 21, 1, 2, 3));
			addFt8Spot(report({ text: 'K1ABC W9XYZ EN37', frequencyHz: 1800, snrDb: -5 }), Date.UTC(2026, 8, 21, 1, 2, 18));
			const table = rows();
			expect(table.length).toBe(2);
			const first = Array.from(table[0].querySelectorAll('td')).map((cell) => cell.textContent);
			expect(first[0]).toBe('01:02:18');
			expect(first[1]).toBe('-5');
			expect(first[2]).toBe('1800');
			expect(first[3]).toBe('21.0758');
			expect(first[4]).toBe('K1ABC W9XYZ EN37');
			expect(document.getElementById('decode-window-count')?.textContent).toBe('2');
		} finally {
			process.env.TZ = prevTz;
		}
	});

	it('derives sender → receiver countries and the sender’s grid for each decode', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		setLang('zh');
		try {
			addFt8Spot(report({ text: 'JA0TXU BG7BVP -73' }), 1_000);   // the real 40m slot fixture
			addFt8Spot(report({ text: 'CQ BG7BVP OL68' }), 2_000);
			const cells = (row: Element) => Array.from(row.querySelectorAll('td')).map((c) => c.textContent);
			const [cqRow, realRow] = rows();
			expect(cells(cqRow)[4]).toBe('CQ BG7BVP OL68');
			expect(cells(cqRow)[5]).toBe('中国');               // CQ: sender only
			expect(cells(cqRow)[6]).toBe('28.5°N 113.0°E');
			expect(cells(realRow)[5]).toBe('中国 → 日本');      // directed: sender (transmitting) → receiver
			expect(cells(realRow)[6]).toBe('—');
		} finally {
			setLang('en');
		}
	});
	it('filters rows by a regex over the message text; an invalid pattern keeps every row', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		addFt8Spot(report({ text: 'JA0TXU BG7BVP -73' }), 1_000);
		addFt8Spot(report({ text: 'CQ JO1WKO PM95' }), 2_000);
		addFt8Spot(report({ text: 'K1ABC W9XYZ EN37' }), 3_000);
		const input = document.getElementById('decode-ft8-search') as HTMLInputElement;
		const searchRow = document.getElementById('decode-ft8-search-row') as HTMLElement;
		expect(searchRow.style.display).toBe('');   // the search row belongs to the FT8 pane
		input.value = 'BG7BVP';
		input.dispatchEvent(new Event('input', { bubbles: true }));
		expect(rows().length).toBe(1);
		expect(document.getElementById('decode-window-count')?.textContent).toBe('1/3');
		input.value = '(';
		input.dispatchEvent(new Event('input', { bubbles: true }));
		expect(input.classList.contains('invalid')).toBe(true);   // filter off, rows stay
		expect(rows().length).toBe(3);
		expect(document.getElementById('decode-window-count')?.textContent).toBe('3');
		input.value = '';
		input.dispatchEvent(new Event('input', { bubbles: true }));
		expect(rows().length).toBe(3);
	});

	it('sorts by a clicked column and restores the newest-first order on the second click', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		addFt8Spot(report({ text: 'K1ABC W9XYZ EN37', snrDb: -5, frequencyHz: 1800 }), 1_000);
		addFt8Spot(report({ text: 'CQ JO1WKO PM95', snrDb: 2, frequencyHz: 1000 }), 2_000);
		const firstText = () => (rows()[0].querySelectorAll('td')[4] as HTMLElement).textContent;
		const th = (col: number) =>
			document.querySelector(`#decode-window-table th[data-col="${col}"]`) as HTMLElement;
		th(1).click();                                    // SNR: -5 first
		expect(th(1).classList.contains('sorted')).toBe(true);
		expect((rows()[0].querySelectorAll('td')[1] as HTMLElement).textContent).toBe('-5');
		th(4).click();                                    // message text: CQ… first alphabetically
		expect(th(1).classList.contains('sorted')).toBe(false);
		expect(th(4).classList.contains('sorted')).toBe(true);
		expect(firstText()).toBe('CQ JO1WKO PM95');
		th(4).click();                                    // again: back to arrival order
		expect(th(4).classList.contains('sorted')).toBe(false);
		expect(firstText()).toBe('CQ JO1WKO PM95');       // newest first = the 2_000 decode
	});

	it('marks the hovered decode and its QSO pair; a CQ marks every decode of that caller', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		addFt8Spot(report({ text: 'JA0TXU BG7BVP -73' }), 1_000);
		addFt8Spot(report({ text: 'CQ JO1WKO PM95' }), 2_000);
		addFt8Spot(report({ text: 'BG7BVP JA0TXU RR73' }), 3_000);
		const body = document.getElementById('decode-window-rows') as HTMLElement;
		const hovered = body.querySelectorAll('tr.ft8-row')[0];      // BG7BVP↔JA0TXU pair
		hovered.dispatchEvent(new MouseEvent('pointerover', { bubbles: true }));
		const marked = Array.from(body.querySelectorAll('tr.ft8-row'));
		expect(hovered.classList.contains('qso-focus')).toBe(true);
		expect(marked[2].classList.contains('qso-peer')).toBe(true);  // same pair
		expect(marked[1].classList.contains('qso-peer')).toBe(false); // unrelated CQ
		marked[1].dispatchEvent(new MouseEvent('pointerover', { bubbles: true }));
		expect(marked[1].classList.contains('qso-focus')).toBe(true);
		expect(marked[0].classList.contains('qso-peer')).toBe(false); // pair ≠ CQ key
	});


	it('shows local time by default and switches to UTC from the toggle', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		const prevTz = process.env.TZ;
		try {
			process.env.TZ = 'Etc/GMT+5';
			addFt8Spot(report({ text: 'CQ BG7BVP OL68' }), Date.UTC(2026, 8, 21, 1, 2, 3));
			const first = () => Array.from(rows()[0].querySelectorAll('td')).map((c) => c.textContent);
			const button = document.getElementById('btn-decode-utc') as HTMLButtonElement;
			const timeTh = document.getElementById('decode-window-time-th') as HTMLTableCellElement;
			expect(button.classList.contains('active')).toBe(false);
			expect(first()[0]).toBe('20:02:03');   // local (the default)
			expect(timeTh.title).toBe('LT');       // header tooltip follows the same flag
			button.click();
			expect(button.classList.contains('active')).toBe(true);
			expect(first()[0]).toBe('01:02:03');   // UTC after the toggle
			expect(timeTh.title).toBe('UTC');
			expect(JSON.parse(localStorage.getItem('websa-decode-window') || '{}').utc).toBe(true);
		} finally {
			process.env.TZ = prevTz;
		}
	});

	it('applies the UTC toggle to CW line timestamps too', () => {
		setDecodeWindowAvailable('cw');
		toggleDecodeWindow();
		addCwText('CQ DE', false);
		addCwText(' N0CALL', true);
		const at = cwLog().lines[0].at;
		const stamp = () => document.querySelector('.cw-time')?.textContent;
		expect(stamp()).toBe(clock(at, false));   // local by default
		(document.getElementById('btn-decode-utc') as HTMLButtonElement).click();
		expect(stamp()).toBe(clock(at, true));    // UTC after the toggle
	});

	it('tunes to the signal a row was clicked on', () => {
		const onTune = vi.fn();
		initDecodeWindow({ onTune, onSstvMode: () => {} });
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		addFt8Spot(report({ frequencyHz: 1450 }), 1_000);
		rows()[0].dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(onTune).toHaveBeenCalledWith(21_075_450);
	});

	it('clears the table from its own button', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		addFt8Spot(report(), 1_000);
		expect(rows().length).toBe(1);
		(document.getElementById('btn-decode-clear') as HTMLButtonElement).click();
		expect(document.querySelector('.ft8-empty')).not.toBeNull();
	});

	it('remembers where and how big the operator left it', () => {
		setDecodeWindowAvailable('ft8');
		const win = document.getElementById('decode-window') as HTMLElement;
		const head = document.getElementById('decode-window-head') as HTMLElement;
		head.dispatchEvent(new PointerEvent('pointerdown', { clientX: 100, clientY: 100, bubbles: true }));
		head.dispatchEvent(new PointerEvent('pointermove', { clientX: 160, clientY: 140, bubbles: true }));
		head.dispatchEvent(new PointerEvent('pointerup', { clientX: 160, clientY: 140, bubbles: true }));
		expect(win.style.left).not.toBe('');
		const stored = JSON.parse(localStorage.getItem('websa-decode-window') || '{}');
		expect(typeof stored.x).toBe('number');
		expect(stored.w).toBeGreaterThan(0);
	});

	it('dims the window from its head, remembers the level, and stays grabbable', () => {
		setDecodeWindowAvailable('ft8');
		const win = document.getElementById('decode-window') as HTMLElement;
		const slider = document.getElementById('decode-window-opacity') as HTMLInputElement;
		expect(win.style.opacity).toBe('1');

		slider.value = '60';
		slider.dispatchEvent(new Event('input', { bubbles: true }));
		expect(win.style.opacity).toBe('0.6');
		expect(JSON.parse(localStorage.getItem('websa-decode-window') || '{}').opacity).toBeCloseTo(0.6);

		// The floor is a guard, not a preference: an invisible window cannot be dragged back, which is
		// the same reason the position is clamped into the viewport instead of restored blindly.
		slider.value = '0';
		slider.dispatchEvent(new Event('input', { bubbles: true }));
		expect(win.style.opacity).toBe('0.35');

		// The slider is a control in the drag handle: grabbing it must move the handle, not the window.
		const before = win.style.left;
		slider.dispatchEvent(new PointerEvent('pointerdown', { clientX: 100, clientY: 100, bubbles: true }));
		(document.getElementById('decode-window-head') as HTMLElement)
			.dispatchEvent(new PointerEvent('pointermove', { clientX: 260, clientY: 220, bubbles: true }));
		expect(win.style.left).toBe(before);
	});

	it('drags in viewport pixels while carrying the UI scale (the zoom fix)', () => {		setUiScale(2);   // jsdom lays nothing out, so this pins the conversion convention only
		try {
			setDecodeWindowAvailable('ft8');
			const win = document.getElementById('decode-window') as HTMLElement;
			const head = document.getElementById('decode-window-head') as HTMLElement;
			expect(win.style.zoom).toBe('2');

			// Grab the head at the cursor and drag: the styles store local px (half the viewport
			// distance at scale 2), while the persisted geometry stays in viewport px - that split is
			// what keeps the cursor anchored to the head on a scaled UI.
			head.dispatchEvent(new PointerEvent('pointerdown', { clientX: 300, clientY: 200, bubbles: true }));
			head.dispatchEvent(new PointerEvent('pointermove', { clientX: 500, clientY: 300, bubbles: true }));
			head.dispatchEvent(new PointerEvent('pointerup', { clientX: 500, clientY: 300, bubbles: true }));
			// jsdom reports a 0x0 rect, so the grab offset is the full cursor position (300, 200) and
			// the move lands the window's origin at (200, 100) viewport px.
			expect(win.style.left).toBe('100px');   // 200 viewport px / scale 2
			expect(win.style.top).toBe('50px');
			const stored = JSON.parse(localStorage.getItem('websa-decode-window') || '{}');
			expect(stored.x).toBe(200);             // persisted in viewport px
			expect(stored.y).toBe(100);

			// A later init at the same scale restores the same spot: the stored viewport px go back
			// through the same division, not in as raw style values.
			initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
			expect(win.style.left).toBe('100px');
			expect(win.style.top).toBe('50px');
		} finally {
			setUiScale(1);
		}
	});

	it('resizes from all four edges and four corners', () => {
		setDecodeWindowAvailable('ft8');
		const win = document.getElementById('decode-window') as HTMLElement;
		const box = (el: string) => document.querySelector(`[data-decode-rz="${el}"]`) as HTMLElement;
		const drag = (el: string, dx: number, dy: number) => {
			box(el).dispatchEvent(new PointerEvent('pointerdown', { clientX: 100, clientY: 100, bubbles: true }));
			box(el).dispatchEvent(new PointerEvent('pointermove', { clientX: 100 + dx, clientY: 100 + dy, bubbles: true }));
			box(el).dispatchEvent(new PointerEvent('pointerup', { clientX: 100 + dx, clientY: 100 + dy, bubbles: true }));
		};
		// jsdom has no layout: geometry starts from the inline styles the init wrote (the scale-1
		// defaults) and each drag is a delta on the captured start box.
		const w0 = Number.parseFloat(win.style.width);
		const h0 = Number.parseFloat(win.style.height);
		expect(box('n') && box('s') && box('e') && box('w')).toBeTruthy();
		drag('e', 40, 0);
		expect(Number.parseFloat(win.style.width)).toBe(w0 + 40);
		drag('s', 0, 30);
		expect(Number.parseFloat(win.style.height)).toBe(h0 + 30);
		// The west edge moves the origin with it; the height is untouched by a horizontal drag.
		const left0 = Number.parseFloat(win.style.left);
		drag('w', -20, 0);
		expect(Number.parseFloat(win.style.width)).toBe(w0 + 60);
		expect(Number.parseFloat(win.style.left)).toBe(left0 - 20);
		// The north edge moves the top and shrinks the box from that side.
		const top0 = Number.parseFloat(win.style.top);
		drag('n', 0, -10);
		expect(Number.parseFloat(win.style.top)).toBe(top0 - 10);
		expect(Number.parseFloat(win.style.height)).toBe(h0 + 40);
		// A corner changes both axes at once.
		drag('se', 25, 15);
		expect(Number.parseFloat(win.style.width)).toBe(w0 + 85);
		expect(Number.parseFloat(win.style.height)).toBe(h0 + 55);
	});

	it('keeps the opposite edge fixed when a resize hits the minimum width', () => {
		setDecodeWindowAvailable('ft8');
		const win = document.getElementById('decode-window') as HTMLElement;
		const east = document.querySelector('[data-decode-rz="e"]') as HTMLElement;
		const left0 = Number.parseFloat(win.style.left);
		// Pull the west edge far past the 260 px floor: the east edge (left0 + width) must not move.
		const right0 = left0 + Number.parseFloat(win.style.width);
		const west = document.querySelector('[data-decode-rz="w"]') as HTMLElement;
		west.dispatchEvent(new PointerEvent('pointerdown', { clientX: 100, clientY: 100, bubbles: true }));
		west.dispatchEvent(new PointerEvent('pointermove', { clientX: 3000, clientY: 100, bubbles: true }));
		west.dispatchEvent(new PointerEvent('pointerup', { clientX: 3000, clientY: 100, bubbles: true }));
		expect(Number.parseFloat(win.style.width)).toBe(260);
		expect(Number.parseFloat(win.style.left) + 260).toBe(right0);
		expect(east).toBeTruthy();
	});

	it('shows the CW text when CW is the demodulator, and clears it from the same button', () => {
		setDecodeWindowAvailable('cw');
		toggleDecodeWindow();
		expect(decodeWindowOpen()).toBe(true);
		expect(decodeWindowMode()).toBe('cw');
		expect(document.getElementById('decode-window-title')?.textContent).toBe('CW');
		expect((document.getElementById('decode-window-cw') as HTMLElement).style.display).toBe('');
		expect((document.getElementById('decode-window-table') as HTMLElement).style.display).toBe('none');

		addCwText('CQ DE', false);
		addCwText(' N0CALL', true);
		const pane = document.getElementById('decode-window-cw') as HTMLElement;
		expect(pane.textContent).toContain('CQ DE N0CALL');
		expect(cwLog().lines.length).toBe(1);

		(document.getElementById('btn-decode-clear') as HTMLButtonElement).click();
		expect(cwLog().lines.length).toBe(0);
		// Clearing the CW pane must not touch the FT8 log behind it.
		addFt8Spot(report(), 1000);
		expect(document.querySelectorAll('#decode-window-rows tr').length).toBe(0);
	});

	it('keeps the window open when the operator switches between decoding modes', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		setDecodeWindowAvailable('cw');
		expect(decodeWindowOpen()).toBe(true);
		expect(decodeWindowMode()).toBe('cw');
		expect((document.getElementById('decode-window-title') as HTMLElement).textContent).toBe('CW');
	});

	it('keeps DRM service health separate from the FT8 table across mode switches', () => {
		setDecodeWindowAvailable('drm');
		toggleDecodeWindow();
		setDrmDecode([
			'locked: B, 10 kHz', 'station: CNR-1', 'FAC MER 18.7 dB',
			'14 MSC frames, 86 audio AUs, FAC ok 21 err 0', 'dbg au=86 fed=3',
		], null);
		setDrmConstellation([{ re: 0.7, im: -0.7 }]);
		expect(decodeWindowMode()).toBe('drm');
		expect((document.getElementById('decode-window-table') as HTMLElement).style.display).toBe('none');
		expect(document.getElementById('decode-drm-station')?.textContent).toBe('CNR-1');
		expect(document.getElementById('decode-drm-mode')?.textContent).toContain('B · 10 kHz');
		expect(document.getElementById('decode-drm-fac-ok')?.textContent).toBe('21');
		expect(document.getElementById('decode-drm-fac-err')?.textContent).toBe('0');
		expect(document.getElementById('decode-drm-msc')?.textContent).toBe('14');
		expect(document.getElementById('decode-drm-aus')?.textContent).toBe('86');
		expect(document.getElementById('decode-drm-mer')?.textContent).toBe('18.7');
		expect(document.getElementById('decode-drm-snr')?.textContent).toBe('—');
		expect(document.getElementById('decode-drm-snr')?.title).toContain('FAC frame');
		expect((document.getElementById('decode-drm-details') as HTMLElement).style.display).toBe('');
		addFt8Spot(report({ text: 'CQ JO1WKO PM95' }));
		expect(rows().length).toBe(0);
		setDecodeWindowAvailable('ft8');
		expect(rows().length).toBe(1);
		expect(rows()[0].textContent).not.toContain('CNR-1');
		expect(drmStatus().lines).toEqual([]);
		setDecodeWindowAvailable('drm');
		expect(document.getElementById('decode-drm-station')?.textContent).not.toBe('CNR-1');
		expect((document.getElementById('decode-drm-metrics') as HTMLElement).style.display).toBe('none');
	});

	it('shows the independent SNR estimate as well as FAC MER when available', () => {
		setDecodeWindowAvailable('drm');
		toggleDecodeWindow();
		setDrmDecode(['locked: B, 10 kHz', 'station: CNR-1', 'FAC MER 18.7 dB', 'FAC SNR 16 dB'], 16);
		expect(document.getElementById('decode-drm-mer')?.textContent).toBe('18.7');
		expect(document.getElementById('decode-drm-snr')?.textContent).toBe('16 dB');
		expect(document.getElementById('decode-drm-snr')?.title).toBe('');
	});

	it('retains the DRM snapshot across empty heartbeats and updates service rows in place', () => {
		setDecodeWindowAvailable('drm');
		toggleDecodeWindow();
		const receiver = { locked: true, mode: 'B', stationLabel: 'CNR-1', carrierOffsetHz: 0,
			facOk: 21, facErrors: 0, sdcOk: 3,
			services: [{ serviceId: 1, label: 'CNR-1', audioCoding: 0, sampleRate: 24000, channels: 1 }] };
		setDrmDecode(['locked: B, 10 kHz', 'FAC MER 18.7 dB', '14 MSC frames, 86 audio AUs, FAC ok 21 err 0'], 16, 1, receiver);
		const service = document.getElementById('decode-drm-services')?.children[1];
		setDrmDecode([], null, 1, { ...receiver, facOk: 22 });
		expect(document.getElementById('decode-drm-msc')?.textContent).toBe('14');
		expect(document.getElementById('decode-drm-aus')?.textContent).toBe('86');
		expect(document.getElementById('decode-drm-mer')?.textContent).toBe('18.7');
		expect(document.getElementById('decode-drm-snr')?.textContent).toBe('16 dB');
		expect(document.getElementById('decode-drm-fac-ok')?.textContent).toBe('22');
		expect(document.getElementById('decode-drm-services')?.children[1]).toBe(service);
	});

	it('consumes all decoder updates before painting the latest snapshot once', () => {
		setDecodeWindowAvailable('cw');
		toggleDecodeWindow();
		let paint: FrameRequestCallback | null = null;
		const schedule = vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => { paint = callback; return 1; });
		ingestDecodeEvents(() => {
			addCwText('CQ ', false);
			addCwText('DE N0CALL', true);
		});
		expect(cwLog().lines.map((line) => line.text).join('')).toContain('CQ DE N0CALL');
		expect(document.getElementById('decode-window-cw')?.textContent).not.toContain('CQ DE N0CALL');
		expect(schedule).toHaveBeenCalledTimes(1);
		(paint! as FrameRequestCallback)(0);
		expect(document.getElementById('decode-window-cw')?.textContent).toContain('CQ DE N0CALL');
	});

	it('retains decoded updates while closed and paints their latest state on opening', () => {
		setDecodeWindowAvailable('drm');
		const schedule = vi.spyOn(window, 'requestAnimationFrame');
		const canvas = vi.spyOn(HTMLCanvasElement.prototype, 'getContext');
		canvas.mockClear();
		ingestDecodeEvents(() => {
			setDecodeTiming(1, { inputIndex: '9000', fractionNum: '0', fractionDen: '1', inputRate: 48000,
				delaySeconds: 1, delayKnown: false, causalPresentation: true });
			setDrmDecode(['locked: B, 10 kHz', 'station: LATEST STATION'], 19);
			setDrmConstellation([{ re: 0.7, im: -0.7 }]);
		});
		expect(schedule).not.toHaveBeenCalled();
		expect(canvas).not.toHaveBeenCalled();
		expect(drmStatus().lines).toContain('station: LATEST STATION');
		toggleDecodeWindow();
		expect(document.getElementById('decode-drm-station')?.textContent).toBe('LATEST STATION');
		expect(document.getElementById('decode-window-title')?.title).toContain('9000');
	});

	it('shows DRM searching without old station or stale constellation', () => {
		setDecodeWindowAvailable('drm');
		toggleDecodeWindow();
		setDrmDecode(['DRM searching: 2.0s buffered, B 10 kHz, carrier +0.0 Hz, FAC errors 0'], null);
		expect(document.getElementById('decode-drm-status')?.textContent).toBe('Searching');
		expect(document.getElementById('decode-drm-mode')?.textContent).toContain('B · 10 kHz');
		expect(document.getElementById('decode-drm-lock')?.title).toContain('2.0s buffered');
		expect((document.getElementById('decode-drm-main') as HTMLElement).style.display).toBe('none');
	});

	it('shows the same structured pane when switching from DRM30 to DRM+', () => {
		setDecodeWindowAvailable('drm');
		toggleDecodeWindow();
		setDrmDecode(['station: CNR-1', 'locked: B, 10 kHz'], 15);
		setDecodeWindowAvailable('drmplus');
		expect(decodeWindowOpen()).toBe(true);
		expect(document.getElementById('decode-window-title')?.textContent).toBe('DRM+');
		expect(drmStatus().lines).toEqual([]);
		setDrmDecode(['station: VHF TEST', 'locked: E, 100 kHz'], 12);
		expect(document.getElementById('decode-drm-station')?.textContent).toBe('VHF TEST');
	});

	it('is closed and disabled for a mode that does not decode', () => {
		setDecodeWindowAvailable('ft8');
		toggleDecodeWindow();
		setDecodeWindowAvailable('dsb');
		expect(decodeWindowOpen()).toBe(false);
		expect(decodeWindowMode()).toBe(null);
		expect((document.getElementById('btn-decode-window') as HTMLButtonElement).disabled).toBe(true);
	});

	it('comes back when a decoding mode returns (the choice is not forgotten)', () => {
		// Reported: after leaving CW for another demodulator and coming back, the window stayed
		// hidden - the first version cleared the operator's choice on the way out - so the decode
		// list looked dead ("CW stops working after switching the demodulator away and back").
		setDecodeWindowAvailable('cw');
		toggleDecodeWindow();
		expect(decodeWindowOpen()).toBe(true);
		setDecodeWindowAvailable('usb');
		expect(decodeWindowOpen()).toBe(false);
		setDecodeWindowAvailable('cw');
		expect(decodeWindowOpen()).toBe(true);
		expect((document.getElementById('decode-window') as HTMLElement).style.display).toBe('flex');
	});
});

describe('the SSTV pane', () => {
	// The Rust mode table as the ABI reports it; the pane's buttons are built from this and nothing
	// else, so a fake table is what proves the buttons follow the decoder's list.
	const table = [
		{ index: 0, name: 'Scottie S1', vis: 60, width: 320, height: 256 },
		{ index: 5, name: 'Robot 36', vis: 8, width: 320, height: 240 },
		{ index: 17, name: 'PD-290', vis: 94, width: 800, height: 616 },
	];

	const picture = (marker: number) => ({ rgb: new Uint8Array([marker, marker, marker]), width: 2, height: 2 });
	const meta = (serial: number, over: Partial<Parameters<typeof updateSstvPicture>[1]> = {}) => ({
		name: 'Robot 36', modeIndex: 5, vis: 8, width: 320, height: 240,
		lines: 120, progress: 50, complete: false, serial, hz: 7_171_000, ...over,
	});
	const autoButton = () => document.getElementById('sstv-auto') as HTMLButtonElement;
	const modeButtons = () => Array.from(document.querySelectorAll<HTMLButtonElement>('#sstv-mode-group .btn'));
	const buttonText = (button: HTMLButtonElement) => button.dataset.sstvName ?? '';
	const activeMode = () => modeButtons().find((button) => button.classList.contains('active'))?.dataset.sstvName
		?? (autoButton().classList.contains('active') ? 'Auto' : undefined);

	beforeEach(() => {
		shell();
		clearSstv();
		setSstvModeTable(table);
		setSstvModeFilter(null);
		localStorage.clear();
		vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
	});

	afterEach(() => {
		setSstvModeTable(null);
		vi.restoreAllMocks();
	});

	it('shows its pane for the SSTV demodulator and keeps the FT8 table out of the way', () => {
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		expect(decodeWindowOpen()).toBe(true);
		expect(decodeWindowMode()).toBe('sstv');
		expect(document.getElementById('decode-window-title')?.textContent).toBe('SSTV');
		expect((document.getElementById('decode-window-sstv') as HTMLElement).style.display).toBe('');
		expect((document.getElementById('decode-window-table') as HTMLElement).style.display).toBe('none');
	});

	it('builds its mode buttons from the decoder table and highlights the detected mode', () => {
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		expect(modeButtons().map(buttonText)).toEqual(['Scottie S1', 'Robot 36', 'PD-290']);
		expect(autoButton().classList.contains('active')).toBe(true);
		expect(activeMode()).toBe('Auto');
		updateSstvPicture(picture(1), meta(1));
		expect(activeMode()).toBe('Robot 36');
		expect(document.getElementById('sstv-mode-name')?.textContent).toBe('Robot 36');
		expect(document.getElementById('sstv-vis')?.textContent).toBe('VIS 8');
		expect(document.getElementById('sstv-size')?.textContent).toBe('320×240');
		expect((document.getElementById('sstv-progress-fill') as HTMLElement).style.getPropertyValue('--pct')).toBe('50%');
	});

	it('shows the stored-picture count as a badge on the mode button', () => {
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		const robotBadge = () => modeButtons().find((button) => buttonText(button) === 'Robot 36')?.querySelector('.sstv-badge') as HTMLElement;
		expect(robotBadge().style.display).toBe('none');
		updateSstvPicture(picture(1), meta(1));
		expect(robotBadge().style.display).toBe('');
		expect(robotBadge().textContent).toBe('1');
		updateSstvPicture(picture(2), meta(2));
		expect(robotBadge().textContent).toBe('2');
	});







	it('waits in the title row and shows the decoder state while nothing has decoded', () => {
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		const hint = document.getElementById('sstv-head-hint') as HTMLElement;
		const state = document.getElementById('sstv-state') as HTMLElement;
		const canvas = document.getElementById('decode-sstv-canvas') as HTMLCanvasElement;
		expect(hint.style.display).toBe('');
		expect(canvas.style.display).toBe('none');
		expect(state.style.display).toBe('');
		expect(state.textContent).toBe(t('sstv_no_state'));
		setSstvDecoderState({ running: false, pushes: 0, attempts: 0, bufferedSamples: 0, bufferedSeconds: 0, hasPicture: false, level: 0, srcLevel: 0 });
		expect(state.textContent).toBe(t('sstv_decoder_off'));
		setSstvDecoderState({ running: true, pushes: 5, attempts: 3, bufferedSamples: 96_000, bufferedSeconds: 12, hasPicture: false, level: 0.3, srcLevel: 0.2 });
		expect(state.textContent).toBe(`${t('sstv_state', { s: '12', n: '3' })} · dec 30% · src 20%`);
		// A picture replaces the waiting line and the counters.
		updateSstvPicture(picture(1), meta(1));
		expect(hint.style.display).toBe('none');
		expect(state.style.display).toBe('none');
		expect(canvas.style.display).toBe('');
	});

	it('hides the waiting line for every other pane', () => {
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		const hint = document.getElementById('sstv-head-hint') as HTMLElement;
		expect(hint.style.display).toBe('');
		setDecodeWindowAvailable('ft8');
		expect(hint.style.display).toBe('none');
	});

	it('saves the picture at its true resolution as a PNG', () => {
		const sizes: Array<[number, number]> = [];
		const types: Array<string | undefined> = [];
		const downloads: string[] = [];
		Object.defineProperty(HTMLCanvasElement.prototype, 'toBlob', {
			configurable: true,
			value: function (this: HTMLCanvasElement, callback: BlobCallback, type?: string) {
				sizes.push([this.width, this.height]);
				types.push(type);
				callback(new Blob([new Uint8Array([0])], { type }));
			},
		});
		Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: () => 'blob:test' });
		Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: () => {} });
		const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
			downloads.push(this.download);
		});

		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		updateSstvPicture(
			{ rgb: new Uint8Array(320 * 240 * 3), width: 320, height: 240 },
			meta(1, { width: 320, height: 240 }),
		);
		document.getElementById('btn-sstv-save')!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(sizes).toEqual([[320, 240]]);
		expect(types).toEqual(['image/png']);
		expect(downloads).toHaveLength(1);
expect(downloads[0]).toMatch(/^sstv-Robot36-\d-\d{4}-\d{2}-\d{2}T/);
		click.mockRestore();
		delete (HTMLCanvasElement.prototype as { toBlob?: unknown }).toBlob;
	});
})

describe('SSTV pane interactions', () => {
	const table = [
		{ index: 0, name: 'Scottie S1', vis: 60, width: 320, height: 256 },
		{ index: 5, name: 'Robot 36', vis: 8, width: 320, height: 240 },
		{ index: 17, name: 'PD-290', vis: 94, width: 800, height: 616 },
	];
	const picture = (marker: number) => ({ rgb: new Uint8Array([marker, marker, marker]), width: 2, height: 2 });
	const meta = (serial: number, over: Partial<Parameters<typeof updateSstvPicture>[1]> = {}) => ({
		name: 'Robot 36', modeIndex: 5, vis: 8, width: 320, height: 240,
		lines: 120, progress: 50, complete: false, serial, hz: 7_171_000, ...over,
	});

	beforeEach(() => {
		shell();
		clearSstv();
		setSstvModeTable(table);
		setSstvModeFilter(null);
		localStorage.clear();
		vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
	});

	afterEach(() => {
		setSstvModeTable(null);
		vi.restoreAllMocks();
	});

	it('tick a thumbnail and delete it', () => {
		updateSstvPicture(picture(1), meta(1, { complete: true, progress: 100 }));
		updateSstvPicture(picture(2), meta(2));
		const thumbs = () => document.querySelectorAll('.decode-sstv-thumb');
		expect(thumbs()).toHaveLength(2);
		// View the older thumb, then tick it for deletion.
		(thumbs()[1] as HTMLElement).dispatchEvent(new MouseEvent('click', { bubbles: true }));
		const tick = thumbs()[1].querySelector('.sstv-tick') as HTMLElement;
		tick.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(sstvStatus().history[0].selected).toBe(true);
		const del = document.getElementById('btn-sstv-delete') as HTMLButtonElement;
		expect(del.disabled).toBe(false);
		del.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(sstvStatus().history).toHaveLength(0);
		// The live picture is untouched.
		expect(sstvStatus().current).not.toBeNull();
	});

	it('clear all removes everything without breaking the live picture', () => {
		updateSstvPicture(picture(1), meta(1));
		updateSstvPicture(picture(2), meta(2));
		expect(sstvCount()).toBe(2);
		const clearAll = document.getElementById('btn-decode-clear') as HTMLButtonElement;
		clearAll.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(sstvCount()).toBe(2);
		document.getElementById('btn-sstv-delete-all')!.click();
		expect(sstvCount()).toBe(0);
		// Reception recovers: the next picture starts fresh.
		updateSstvPicture(picture(3), meta(3));
		expect(sstvCount()).toBe(1);
	});

	


})

describe('SSTV auto toggle + multi-select delete + confirm', () => {
	const table = [
		{ index: 0, name: 'Scottie S1', vis: 60, width: 320, height: 256 },
		{ index: 5, name: 'Robot 36', vis: 8, width: 320, height: 240 },
		{ index: 17, name: 'PD-290', vis: 94, width: 800, height: 616 },
	];
	const picture = (marker: number) => ({ rgb: new Uint8Array([marker, marker, marker]), width: 2, height: 2 });
	const meta = (serial: number, over: Partial<Parameters<typeof updateSstvPicture>[1]> = {}) => ({
		name: 'Robot 36', modeIndex: 5, vis: 8, width: 320, height: 240,
		lines: 120, progress: 50, complete: false, serial, hz: 7_171_000, ...over,
	});
	const autoButton = () => document.getElementById('sstv-auto') as HTMLButtonElement;
	const modeButtons = () => Array.from(document.querySelectorAll<HTMLButtonElement>('#sstv-mode-group .btn'));
	const findMode = (name: string) => modeButtons().find((button) => button.dataset.sstvName === name)!;

	beforeEach(() => {
		shell();
		clearSstv();
		setSstvModeTable(table);
		setSstvModeFilter(null);
		localStorage.clear();
		vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
	});

	it('auto on: mode click views only (hooks not called); auto off: mode click pins', () => {
		const onSstvMode = vi.fn((mode: number | null) => sdrSstvMode.set(mode ?? -1));
		initDecodeWindow({ onTune: () => {}, onSstvMode });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		updateSstvPicture(picture(1), meta(1));
		// Auto on (default): clicking Robot 36 views only.
		findMode('Robot 36').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(onSstvMode).not.toHaveBeenCalled();
		// Toggle auto off.
		autoButton().dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(onSstvMode).toHaveBeenCalledWith(5); // pins to detected mode
		// Now clicking PD-290 pins to 17.
		findMode('PD-290').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(onSstvMode).toHaveBeenLastCalledWith(17);
		// Toggle auto back on: unpins.
		autoButton().dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(onSstvMode).toHaveBeenLastCalledWith(null);
	});

	it('projects API confirmation and pending pin or Auto requests without changing the history filter', () => {
		initDecodeWindow({ onTune: () => {}, onSstvMode: (mode) => sdrSstvMode.set(mode ?? -1) });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		findMode('Robot 36').click();
		expect(sstvStatus().activeMode).toBe(5);
		sdrSstvMode.confirm(17);
		refreshDecodeWindow();
		expect(autoButton().classList.contains('active')).toBe(false);
		expect(findMode('PD-290').classList.contains('active')).toBe(true);
		expect(sstvStatus().activeMode).toBe(5);
		findMode('Scottie S1').click();
		expect(sdrSstvMode.get()).toBe(0);
		expect(findMode('Scottie S1').classList.contains('pending')).toBe(true);
		sdrSstvMode.confirm(0);
		refreshDecodeWindow();
		expect(findMode('Scottie S1').classList.contains('pending')).toBe(false);
		autoButton().click();
		expect(sdrSstvMode.get()).toBe(-1);
		expect(autoButton().classList.contains('pending')).toBe(true);
		sdrSstvMode.confirm(-1);
		refreshDecodeWindow();
		expect(autoButton().classList.contains('active')).toBe(true);
		expect(autoButton().classList.contains('pending')).toBe(false);
		expect(sstvStatus().activeMode).toBe(0);
	});

	it('Preset immediately projects Auto, removes the persisted pin and waits for confirmation', () => {
		sdrSstvMode.confirm(17);
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		expect(findMode('PD-290').classList.contains('active')).toBe(true);
		resetSdrState();
		refreshDecodeWindow();
		expect(autoButton().classList.contains('active')).toBe(true);
		expect(autoButton().classList.contains('pending')).toBe(true);
		expect(localStorage.getItem('web-sa-sdr-sstv-mode')).toBeNull();
		sdrSstvMode.confirm(-1);
		refreshDecodeWindow();
		expect(autoButton().classList.contains('pending')).toBe(false);
	});

	it('Auto off without a received or stored pin keeps Auto and never sends mode zero', () => {
		const onSstvMode = vi.fn();
		initDecodeWindow({ onTune: () => {}, onSstvMode });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		expect(sstvStatus().activeMode).toBeNull();
		autoButton().click();
		expect(onSstvMode).not.toHaveBeenCalled();
		expect(sdrSstvMode.get()).toBe(-1);
		expect(autoButton().classList.contains('active')).toBe(true);
	});

	it('uses the viewed mode only when Auto off is explicitly requested without a received mode', () => {
		const onSstvMode = vi.fn((mode: number | null) => sdrSstvMode.set(mode ?? -1));
		initDecodeWindow({ onTune: () => {}, onSstvMode });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		findMode('PD-290').click();
		expect(onSstvMode).not.toHaveBeenCalled();
		expect(sdrSstvMode.get()).toBe(-1);
		autoButton().click();
		expect(onSstvMode).toHaveBeenCalledTimes(1);
		expect(onSstvMode).toHaveBeenLastCalledWith(17);
		expect(sdrSstvMode.get()).toBe(17);
		expect(findMode('PD-290').classList.contains('pending')).toBe(true);
	});

	it('ignores decoder commands while inactive and retains the requested visibility', () => {
		const onSstvMode = vi.fn((mode: number | null) => sdrSstvMode.set(mode ?? -1));
		initDecodeWindow({ onTune: () => {}, onSstvMode });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		sdrSstvMode.confirm(5);
		setDecodeActive(false);
		autoButton().click();
		findMode('PD-290').click();
		expect(onSstvMode).not.toHaveBeenCalled();
		expect(sdrSstvMode.get()).toBe(5);
		toggleDecodeWindow();
		setDecodeActive(true);
		expect(decodeWindowOpen()).toBe(true);
	});

	it.each([
		{ auto: null, view: null, pin: null, expected: -1, filter: null },
		{ auto: null, view: '17', pin: null, expected: -1, filter: 17 },
		{ auto: '0', view: null, pin: null, expected: -1, filter: null },
		{ auto: '0', view: '', pin: null, expected: -1, filter: null },
		{ auto: '1', view: '17', pin: null, expected: -1, filter: 17 },
		{ auto: '0', view: '5', pin: null, expected: 5, filter: 5 },
		{ auto: null, view: null, pin: '0', expected: 0, filter: null },
		{ auto: null, view: null, pin: '', expected: -1, filter: null },
		{ auto: null, view: '5', pin: '17', expected: 17, filter: 5 },
	])('reload restores config $expected and history filter $filter without sending ($auto/$view/$pin)', async ({ auto, view, pin, expected, filter }) => {
		if (auto !== null) localStorage.setItem('web-sa-sstv-auto', auto);
		if (view !== null) localStorage.setItem('web-sa-sstv-mode', view);
		if (pin !== null) localStorage.setItem('web-sa-sdr-sstv-mode', pin);
		vi.resetModules();
		try {
			// Fresh module loading is the reload boundary: static imports cannot restore storage again.
			const state = await import('../ui/sdrState');
			const decode = await import('../ui/decodeWindow');
			const registry = await import('../sdr/registry');
			const log = await import('../sdr/sstvLog');
			registry.setSstvModeTable(table);
			const onSstvMode = vi.fn();
			decode.initDecodeWindow({ onTune: () => {}, onSstvMode });
			decode.setDecodeWindowAvailable('sstv');
			decode.toggleDecodeWindow();
			expect(decode.decodeWindowOpen()).toBe(false);
			expect(state.sdrSstvMode.get()).toBe(expected);
			expect(log.sstvStatus().activeMode).toBe(filter);
			expect(onSstvMode).not.toHaveBeenCalled();
			decode.setDecodeActive(true);
			decode.toggleDecodeWindow();
			expect(decode.decodeWindowOpen()).toBe(true);
			expect(autoButton().classList.contains('active')).toBe(expected < 0);
			if (expected >= 0) {
				expect(document.querySelector(`[data-sstv-mode="${expected}"]`)?.classList.contains('active')).toBe(true);
			}
			expect(onSstvMode).not.toHaveBeenCalled();
			decode.setDecodeActive(false);
		} finally {
			vi.resetModules();
		}
	});

	it('tick 2 thumbnails and delete both', () => {
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		updateSstvPicture(picture(1), meta(1, { complete: true, progress: 100 }));
		updateSstvPicture(picture(2), meta(2, { complete: true, progress: 100 }));
		updateSstvPicture(picture(3), meta(3));
		// Tick the 2 history entries.
		setSstvSelected(sstvStatus().history[0].id, true);
		setSstvSelected(sstvStatus().history[1].id, true);
		const del = document.getElementById('btn-sstv-delete') as HTMLButtonElement;
		expect(del.disabled).toBe(false);
		del.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		expect(sstvStatus().history).toHaveLength(0);
		expect(sstvCount()).toBe(1); // only the live picture remains
	});

	it('keeps receiving while an inline decision is pending, and Cancel preserves every picture', () => {
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		setDecodeWindowAvailable('sstv');
		toggleDecodeWindow();
		updateSstvPicture(picture(1), meta(1));
		updateSstvPicture(picture(2), meta(2));
		const clearAll = document.getElementById('btn-decode-clear') as HTMLButtonElement;
		const confirmation = document.getElementById('sstv-clear-confirm') as HTMLElement;
		const confirm = document.getElementById('btn-sstv-delete-all') as HTMLButtonElement;
		// The affirmative button cannot delete without an outstanding request.
		confirm.click();
		expect(sstvCount()).toBe(2);
		clearAll.click();
		clearAll.click();
		expect(confirmation.style.display).toBe('');
		expect(document.querySelectorAll('#sstv-clear-confirm')).toHaveLength(1);
		expect(sstvCount()).toBe(2);
		// A growing live image and a new transmission still reach the store and pane.
		updateSstvPicture(picture(3), meta(2, { progress: 75, lines: 180 }));
		expect(sstvStatus().current?.picture.rgb[0]).toBe(3);
		expect(document.getElementById('sstv-progress-text')?.textContent).toBe('75%');
		updateSstvPicture(picture(4), meta(3));
		expect(sstvCount()).toBe(3);
		expect(document.getElementById('sstv-count')?.textContent).toBe('3');
		expect(confirmation.style.display).toBe('');
		document.getElementById('btn-sstv-clear-cancel')!.click();
		expect(confirmation.style.display).toBe('none');
		confirm.click();
		expect(sstvCount()).toBe(3);
		clearAll.click();
		confirm.click();
		expect(sstvCount()).toBe(0);
		expect(confirmation.style.display).toBe('none');
		updateSstvPicture(picture(5), meta(4));
		confirm.click();
		expect(sstvCount()).toBe(1);
	});

	it.each(['close', 'toggle', 'demodulator', 'channel', 'inactive/disconnect', 'SSTV mode', 'Auto', 're-init'])(
		'discards pending deletion after %s so a stale button cannot delete a later session', (transition) => {
			initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
			setDecodeWindowAvailable('sstv');
			toggleDecodeWindow();
			updateSstvPicture(picture(1), meta(1));
			document.getElementById('btn-decode-clear')!.click();
			const confirmation = document.getElementById('sstv-clear-confirm') as HTMLElement;
			expect(confirmation.style.display).toBe('');
			switch (transition) {
				case 'close': document.getElementById('btn-decode-close')!.click(); break;
				case 'toggle': toggleDecodeWindow(); break;
				case 'demodulator': setDecodeWindowAvailable('ft8'); break;
				case 'channel':
					setDecodeChannelMode(2, 'sstv');
					setDecodeChannel(2);
					break;
				case 'inactive/disconnect': setDecodeActive(false); break;
				case 'SSTV mode': findMode('PD-290').click(); break;
				case 'Auto': autoButton().click(); break;
				case 're-init': initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} }); break;
			}
			expect(confirmation.style.display).toBe('none');
			setDecodeChannel(1);
			setDecodeWindowAvailable('sstv');
			setDecodeActive(true);
			if (!decodeWindowOpen()) toggleDecodeWindow();
			updateSstvPicture(picture(2), meta(2));
			document.getElementById('btn-sstv-delete-all')!.click();
			expect(sstvCount()).toBe(2);
			expect(sstvStatus().current?.picture.rgb[0]).toBe(2);
			expect(confirmation.style.display).toBe('none');
		},
	);
})

describe('the AM pane', () => {
	const metricIds = ['am-carrier-offset', 'am-modulation', 'am-input', 'am-rms', 'am-peak'];
	const metricText = () => metricIds.map((id) => document.getElementById(id)!.textContent);
	const badge = () => document.getElementById('am-title-badge')!;
	const toggle = () => document.getElementById('am-sync-toggle') as HTMLButtonElement;
	const info = (over: Partial<AmInfo> = {}): AmInfo => ({
		syncEnabled: true, syncLocked: true, syncGeneration: 0, carrierHz: 7_100_012, carrierOffsetHz: 12, modulationPct: 65,
		inputDbFs: -20, rmsDbFs: -12, peakDbFs: -3, scope: new Float32Array([0, 0.5, -0.5, 0]), ...over,
	});
	const lineTo = vi.fn();
	const clearRect = vi.fn();
	const onAmSync = vi.fn((enabled: boolean) => {
		sdrAmSync.set(enabled);
		setAmInfo(null, 1);
	});

	beforeEach(() => {
		localStorage.clear();
		setLang('en');
		sdrAmSync.reset();
		sdrAmSync.confirm(false);
		for (const channel of [1, 2, 3] as const) {
			setDecodeError(channel, null);
			setAmInfo(null, channel);
		}
		shell();
		lineTo.mockClear();
		clearRect.mockClear();
		onAmSync.mockClear();
		vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({
			clearRect, lineTo, moveTo: vi.fn(), beginPath: vi.fn(), stroke: vi.fn(),
		} as unknown as CanvasRenderingContext2D);
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {}, onAmSync });
		setDecodeChannelMode(1, 'am');
		confirmAmSyncGeneration(0);
		if (!decodeWindowOpen()) toggleDecodeWindow();
	});

	afterEach(() => {
		vi.useRealTimers();
		sdrAmSync.reset();
		sdrAmSync.confirm(false);
		vi.restoreAllMocks();
	});

	it('makes AM available and preserves wanted visibility across an unsupported mode', () => {
		expect((document.getElementById('btn-decode-window') as HTMLButtonElement).disabled).toBe(false);
		expect(decodeWindowOpen()).toBe(true);
		expect(document.getElementById('decode-am')!.style.display).not.toBe('none');
		expect(document.getElementById('decode-window-table')!.style.display).toBe('none');
		setDecodeChannelMode(1, 'dsb');
		expect(decodeWindowOpen()).toBe(false);
		expect((document.getElementById('btn-decode-window') as HTMLButtonElement).disabled).toBe(true);
		setDecodeChannelMode(1, 'am');
		expect(decodeWindowOpen()).toBe(true);
		expect(shown()).toBe('flex');
	});

	it('keeps AM telemetry isolated per channel and makes auxiliary sync read only', () => {
		setDecodeChannelMode(2, 'am');
		setAmInfo(info(), 1);
		setAmInfo(info({ carrierOffsetHz: -7, inputDbFs: -40 }), 2);
		expect(metricText()[0]).toBe('12.0 Hz');
		setDecodeChannel(2);
		expect(metricText()[0]).toBe('-7.0 Hz');
		expect(metricText()[2]).toBe('-40.0 dBFS');
		expect(toggle().disabled).toBe(true);
		toggle().click();
		expect(onAmSync).not.toHaveBeenCalled();
		setDecodeChannel(1);
		expect(metricText()[0]).toBe('12.0 Hz');
		expect(metricText()[2]).toBe('-20.0 dBFS');
	});

	it('keeps the confirmed sync badge read only', () => {
		expect(badge().tagName).toBe('SPAN');
		badge().click();
		expect(onAmSync).not.toHaveBeenCalled();
		toggle().click();
		expect(onAmSync).toHaveBeenCalledExactlyOnceWith(true);
	});

	it('keeps AM timing and errors in its badge and hides routine timing in other modes', () => {
		setDecodeTiming(1, { inputIndex: '123456', fractionNum: '3', fractionDen: '7',
			inputRate: 48000, delaySeconds: 0.125, delayKnown: true, causalPresentation: false });
		const timing = document.getElementById('decode-source-timing')!;
		const title = document.getElementById('decode-window-title')!;
		expect(timing.style.display).toBe('none');
		expect(badge().title).toContain('123456 + 3/7');
		expect(badge().title).toContain('0.125000');
		setDecodeError(1, 'test decoder failure');
		expect(badge().title).toContain('test decoder failure');
		expect(badge().title).toContain('123456 + 3/7');
		expect(badge().getAttribute('role')).toBe('alert');
		expect(badge().getAttribute('aria-label')).toContain('test decoder failure');
		setDecodeError(1, null);
		expect(badge().getAttribute('role')).toBe('status');
		expect(badge().title).not.toContain('test decoder failure');
		setDecodeChannelMode(1, 'cw');
		expect(timing.style.display).toBe('none');
		expect(title.title).toContain('123456 + 3/7');
		expect(badge().style.display).toBe('none');
		setDecodeError(1, 'CW failure');
		expect(timing.style.display).not.toBe('none');
		expect(timing.textContent).toContain('CW failure');
		expect(timing.getAttribute('role')).toBe('alert');
		setDecodeError(1, null);
		expect(timing.style.display).toBe('none');
	});

	it('displays absolute carrier frequency only when available and valid', () => {
		const carrier = () => document.getElementById('am-carrier')!.textContent;
		expect(carrier()).toBe('—');
		setAmInfo(info());
		expect(carrier()).toBe('7.100012 MHz');
		for (const carrierHz of [null, 0, -1, NaN, Infinity]) {
			setAmInfo(info({ carrierHz }));
			expect(carrier()).toBe('—');
		}
	});

	it('distinguishes unknown measurements from measured zero', () => {
		expect(metricText()).toEqual(['—', '—', '—', '—', '—']);
		setAmInfo(info({ carrierOffsetHz: 0, modulationPct: 0, inputDbFs: 0, rmsDbFs: 0, peakDbFs: 0 }));
		expect(metricText()).toEqual(['0.0 Hz', '0.0 %', '0.0 dBFS', '0.0 dBFS', '0.0 dBFS']);
		setAmInfo(info({ carrierOffsetHz: null, modulationPct: null, inputDbFs: null, rmsDbFs: null, peakDbFs: null }));
		expect(metricText()).toEqual(['—', '—', '—', '—', '—']);
	});

	it('integrates audio RMS power, releases peaks and clears held levels on telemetry reset', () => {
		const clock = vi.spyOn(performance, 'now').mockReturnValue(0);
		setAmInfo(info({ rmsDbFs: -10, peakDbFs: -3 }));
		clock.mockReturnValue(50);
		setAmInfo(info({ rmsDbFs: -40, peakDbFs: -40 }));
		expect(document.getElementById('am-rms')!.textContent).toBe('-10.7 dBFS');
		expect(document.getElementById('am-peak')!.textContent).toBe('-4.0 dBFS');
		clock.mockReturnValue(100);
		setAmInfo(info({ rmsDbFs: -40, peakDbFs: -1 }));
		expect(document.getElementById('am-peak')!.textContent).toBe('-1.0 dBFS');
		setAmInfo(null);
		expect(document.getElementById('am-rms')!.textContent).toBe('—');
		expect(document.getElementById('am-peak')!.textContent).toBe('—');
		setAmInfo(info({ rmsDbFs: -160, peakDbFs: -160 }));
		expect(document.getElementById('am-rms')!.textContent).toBe('-160.0 dBFS');
		expect(document.getElementById('am-peak')!.textContent).toBe('-160.0 dBFS');
	});

	it('draws different waveform paths when actual audio samples change', () => {
		setAmInfo(info({ scope: new Float32Array([0, 0.5, -0.5, 0]) }));
		const first = lineTo.mock.calls.map((call) => [...call]);
		lineTo.mockClear();
		setAmInfo(info({ scope: new Float32Array([0, -0.25, 0.25, 0]) }));
		const second = lineTo.mock.calls.map((call) => [...call]);
		expect(first).toContainEqual([319 / 3, 26]);
		expect(second).toContainEqual([319 / 3, 62]);
		expect(second).not.toEqual(first);
	});

	it.each(['mode leave', 'decoder error', 'inactive'] as const)('clears old lock and waveform on %s', (transition) => {
		sdrAmSync.confirm(true);
		setAmInfo(info());
		expect(badge().classList.contains('on')).toBe(true);
		lineTo.mockClear();
		clearRect.mockClear();
		if (transition === 'mode leave') {
			setDecodeChannelMode(1, 'dsb');
			expect(toggle().disabled).toBe(true);
			setDecodeChannelMode(1, 'am');
		} else if (transition === 'decoder error') {
			setDecodeError(1, 'AM decoder failed');
			expect(toggle().disabled).toBe(true);
			setAmInfo(info());
			setDecodeError(1, null);
		} else {
			setDecodeActive(false);
			expect(toggle().disabled).toBe(true);
			setDecodeActive(true);
			confirmAmSyncGeneration(0);
		}
		expect(badge().classList.contains('on')).toBe(false);
		expect(document.getElementById('am-carrier')!.textContent).toBe('—');
		expect(metricText()).toEqual(['—', '—', '—', '—', '—']);
		expect(clearRect).toHaveBeenCalled();
		expect(lineTo).not.toHaveBeenCalled();
	});

	it('waits for matching STATUS and actual lock telemetry after a sync request', () => {
		expect(toggle().getAttribute('aria-pressed')).toBe('false');
		expect(badge().classList.contains('on')).toBe(false);
		toggle().click();
		expect(onAmSync).toHaveBeenCalledExactlyOnceWith(true);
		expect(sdrAmSync.pending()).toBe(true);
		expect(toggle().getAttribute('aria-pressed')).toBe('false');
		expect(toggle().getAttribute('aria-busy')).toBe('true');
		expect(toggle().classList.contains('pending')).toBe(true);
		expect(toggle().disabled).toBe(true);
		setAmInfo(info());
		expect(badge().classList.contains('on')).toBe(false);
		sdrAmSync.confirm(false);
		refreshDecodeWindow();
		expect(toggle().getAttribute('aria-pressed')).toBe('false');
		expect(toggle().classList.contains('pending')).toBe(true);
		sdrAmSync.confirm(true);
		setAmInfo(info({ syncLocked: false }));
		refreshDecodeWindow();
		expect(toggle().getAttribute('aria-pressed')).toBe('true');
		expect(toggle().getAttribute('aria-busy')).toBe('false');
		expect(toggle().disabled).toBe(false);
		expect(toggle().getAttribute('aria-pressed')).toBe('true');
		expect(badge().classList.contains('on')).toBe(false);
		setAmInfo(info());
		expect(badge().classList.contains('on')).toBe(true);
		setAmInfo(info({ syncEnabled: false }));
		expect(badge().classList.contains('on')).toBe(false);
		expect(toggle().getAttribute('aria-pressed')).toBe('true');
		setAmInfo(info({ syncLocked: false }));
		expect(badge().classList.contains('on')).toBe(false);
		setDecodeError(1, 'AM decoder failed');
		expect(toggle().disabled).toBe(true);
		toggle().click();
		expect(onAmSync).toHaveBeenCalledTimes(1);
	});

	it.each(['rejection', 'expiration'] as const)('restores confirmed sync state after pending %s', (outcome) => {
		vi.useFakeTimers();
		vi.setSystemTime(10_000);
		sdrAmSync.confirm(true);
		refreshDecodeWindow();
		toggle().click();
		expect(onAmSync).toHaveBeenCalledExactlyOnceWith(false);
		expect(toggle().getAttribute('aria-pressed')).toBe('true');
		expect(toggle().classList.contains('pending')).toBe(true);
		if (outcome === 'rejection') sdrAmSync.reset();
		else vi.setSystemTime(13_001);
		refreshDecodeWindow();
		expect(toggle().getAttribute('aria-pressed')).toBe('true');
		expect(toggle().getAttribute('aria-busy')).toBe('false');
		expect(toggle().classList.contains('pending')).toBe(false);
		expect(badge().classList.contains('on')).toBe(false);
		expect(toggle().getAttribute('aria-pressed')).toBe('true');
		expect(toggle().disabled).toBe(false);
	});

	it('rejects queued telemetry from an earlier enable after off and re-enable', () => {
		sdrAmSync.confirm(true);
		confirmAmSyncGeneration(1);
		setAmInfo(info({ syncGeneration: 1 }));
		expect(badge().classList.contains('on')).toBe(true);
		sdrAmSync.confirm(false);
		confirmAmSyncGeneration(2);
		refreshDecodeWindow();
		sdrAmSync.confirm(true);
		confirmAmSyncGeneration(3);
		setAmInfo(info({ syncGeneration: 1 }));
		expect(toggle().getAttribute('aria-pressed')).toBe('true');
		expect(toggle().getAttribute('aria-pressed')).toBe('true');
		expect(badge().classList.contains('on')).toBe(false);
		expect(document.getElementById('am-carrier')!.textContent).toBe('—');
		expect(metricText()).toEqual(['—', '—', '—', '—', '—']);
		setAmInfo(info({ syncGeneration: 3 }));
		expect(badge().classList.contains('on')).toBe(true);
	});
});

describe('the WFM pane', () => {
	const info = (over: Partial<WfmInfo> = {}): WfmInfo => ({
		stereo: false, peakDevHz: 0, pilotPct: 0, peakDbFs: '', peakHoldDb: '',
		rdsLocked: false, rdsPi: 0, rdsGroups: 0, ps: '', rt: '',
		carrierHz: 98_100_000, pilotDevHz: 19_000, pilotSnrDb: 30,
		scopeL: new Float32Array([0, 0.5, -0.5, 0]), scopeR: null, ...over,
	});
	const carrier = () => document.getElementById('wfm-carrier')!.textContent;
	const label = () => document.getElementById('wfm-carrier-label')!.textContent;

	beforeEach(() => {
		localStorage.clear();
		setLang('en');
		for (const channel of [1, 2, 3] as const) {
			setDecodeError(channel, null);
			setWfmInfo(null, channel);
		}
		shell();
		vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
		initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		setDecodeWindowAvailable('wfm');
		if (!decodeWindowOpen()) toggleDecodeWindow();
	});

	afterEach(() => {
		setLang('en');
		vi.restoreAllMocks();
	});

	it('shows the receive stream channel centre in MHz under its translated label', () => {
		expect(decodeWindowMode()).toBe('wfm');
		expect(label()).toBe(t('wfm_carrier_label'));
		setLang('zh');
		setWfmInfo(info());
		expect(label()).toBe('频率');
		expect(carrier()).toBe('98.10000 MHz');
		setLang('en');
		setWfmInfo(info({ carrierHz: 101_100_000 }));
		expect(label()).toBe('Freq');
		expect(carrier()).toBe('101.10000 MHz');
	});

	it('keeps the placeholder while no positive channel centre arrives', () => {
		expect(carrier()).toBe('—');
		for (const carrierHz of [0, -1, NaN]) {
			setWfmInfo(info({ carrierHz }));
			expect(carrier()).toBe('—');
		}
	});

	it("ignores another receiver's channel centre", () => {
		setWfmInfo(info({ carrierHz: 98_100_000 }), 2);
		expect(carrier()).toBe('—');
		setWfmInfo(info({ carrierHz: 98_100_000 }));
		expect(carrier()).toBe('98.10000 MHz');
	});

	it("shows the selected receiver's channel centre", () => {
		setWfmInfo(info({ carrierHz: 98_100_000 }), 1);
		setDecodeChannelMode(2, 'wfm');
		setDecodeChannel(2);
		expect(carrier()).toBe('—');
		setWfmInfo(info({ carrierHz: 87_500_000 }), 2);
		expect(carrier()).toBe('87.50000 MHz');
	});

	it('clears the shown centre when the stream resets', () => {
		setWfmInfo(info());
		expect(carrier()).toBe('98.10000 MHz');
		setWfmInfo(null);
		expect(carrier()).toBe('—');
		expect(document.getElementById('wfm-station')!.textContent).toBe('—');
	});

	it('drops the shown centre across real stream mode switches', async () => {
		// The honest mode switch is the wire's own stream boundary: a start envelope for
		// another demodulator resets the pane, exactly as the worker delivers it. The fresh
		// modules come from vi.resetModules(): a static import would re-share the old state.
		class Receiver {
			static current: Receiver;
			onmessage: ((event: { data: unknown }) => void) | null = null;
			constructor() { Receiver.current = this; }
			postMessage(): void {}
			terminate(): void {}
		}
		const frame = (over: Record<string, unknown> = {}): ArrayBuffer => {
			const value = { version: 1, channel: 1, channel_id: 17, epoch: 9, seq: 0,
				input_index: '9007199254740993', input_rate: 62500000, input_fraction_num: '0', input_fraction_den: '1',
				delay_seconds: 0.001, delay_known: true, causal_presentation: false, mode: 'wfm', event: { type: 'start' }, ...over };
			const text = new TextEncoder().encode(JSON.stringify(value));
			const data = new ArrayBuffer(text.length + 8);
			new Uint8Array(data).set([83, 68, 69, 50]);
			new DataView(data).setUint32(4, text.length, true);
			new Uint8Array(data, 8).set(text);
			return data;
		};
		const wfmEvent = { type: 'wfm-info', stereo: false, peakDevHz: 0, pilotPct: 0, peakDbFs: '-20', peakHoldDb: '-20',
			rdsLocked: false, rdsPi: 0, rdsGroups: 0, ps: '', rt: '', carrierHz: 98_100_000, pilotDevHz: 19_000, pilotSnrDb: 30 };
		vi.resetModules();
		vi.stubGlobal('Worker', Receiver);
		try {
			const ui = await import('../ui/decodeWindow');
			const { setLang } = await import('../core/i18n');
			const stream = await import('../sdr/iqStream');
			const { DecodeSequence } = await import('../sdr/decodeProtocol');
			shell();
			setLang('en');
			ui.initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
			ui.setDecodeActive(true);
			ui.setDecodeWindowAvailable('wfm');
			if (!ui.decodeWindowOpen()) ui.toggleDecodeWindow();
			const sequence = new DecodeSequence();
			const deliver = (...envelopes: DecodeEnvelope[]) => {
				Receiver.current.onmessage!({ data: { type: 'frames', envelopes } });
				ui.refreshDecodeWindow();
			};
		const start = (mode: string, channel_id: number) => sequence.accept(frame({ mode, channel_id }));
		const wfmInfo = (seq: number, event: Record<string, unknown> = {}, channel_id = 17): DecodeEnvelope =>
			sequence.accept(frame({ channel_id, mode: 'wfm', seq, event: { ...wfmEvent, ...event } }));
		stream.setSdrIqEnabled(true);
		deliver(start('wfm', 17), wfmInfo(1));
		expect(carrier()).toBe('98.10000 MHz');
		deliver(wfmInfo(2, { carrierHz: null }));
		expect(carrier()).toBe('—');
		deliver(start('am', 18));
		expect(document.getElementById('decode-window-title')!.textContent).toBe('AM');
		deliver(start('wfm', 19));
		expect(document.getElementById('decode-window-title')!.textContent).toBe('FM');
		expect(carrier()).toBe('—');
		expect(document.getElementById('wfm-station')!.textContent).toBe('—');
		deliver(wfmInfo(1, { carrierHz: 87_500_000 }, 19));
			expect(carrier()).toBe('87.50000 MHz');
			stream.setSdrIqEnabled(false);
		} finally {
			vi.unstubAllGlobals();
			document.body.innerHTML = '';
		}
	});
});

describe('radiofax logical page browser', () => {
	let ui: typeof DecodeUi;
	let fax: typeof RadiofaxStore;
	let language: typeof I18nModule;
	let originalToBlob: PropertyDescriptor | undefined;
	const transmissions = new Map<string, string>();
	const transmission = (label: string): string => {
		if (!transmissions.has(label)) transmissions.set(label, String(transmissions.size + 1));
		return transmissions.get(label)!;
	};
	const select = () => document.getElementById('radiofax-history') as HTMLSelectElement;
	const click = (id: string) => document.getElementById(id)!.click();
	const text = (id: string) => document.getElementById(id)!.textContent!;
	const image = (transmissionId: string, segmentIndex = 0, values: number[] | Uint8Array = [10, 128, 30], overrides: Partial<RadiofaxPageMeta> = {}, revision = 0, width = 1) => {
		fax.appendRadiofaxRows({ width, row: 0, rows: values.length / width, gray: Uint8Array.from(values), revision }, {
			serial: `${transmissionId}-${segmentIndex}`, epoch: 'metadata-epoch', hz: 9_110_000, ioc: 576, lpm: 120,
			lines: values.length / width, complete: false, partial: false, segmentIndex, transmissionId: transmission(transmissionId),
			missingRows: [], reason: null, ...overrides,
		}, 'stream-epoch');
		ui.refreshDecodeWindow();
	};
	beforeEach(async () => {
		transmissions.clear();
		originalToBlob = Object.getOwnPropertyDescriptor(HTMLCanvasElement.prototype, 'toBlob');
		Object.defineProperty(HTMLCanvasElement.prototype, 'toBlob', {
			configurable: true, writable: true,
			value: (_callback: BlobCallback) => { throw new Error('PNG tests must install their export callback'); },
		});
		// A fresh module graph isolates channel stores and UI subscriptions from the other pane tests.
		vi.resetModules();
		ui = await import('../ui/decodeWindow');
		fax = await import('../sdr/radiofaxLog');
		language = await import('../core/i18n');
		language.setLang('en');
		localStorage.clear();
		shell();
		const markup = new DOMParser().parseFromString(appHtml, 'text/html');
		document.getElementById('decode-window-body')!.append(markup.getElementById('decode-window-radiofax')!);
		vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
		ui.initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		ui.setDecodeActive(true);
		ui.setDecodeWindowAvailable('radiofax');
		if (!ui.decodeWindowOpen()) ui.toggleDecodeWindow();
	});
	afterEach(() => {
		vi.restoreAllMocks();
		if (originalToBlob) Object.defineProperty(HTMLCanvasElement.prototype, 'toBlob', originalToBlob);
		else Reflect.deleteProperty(HTMLCanvasElement.prototype, 'toBlob');
		vi.unstubAllGlobals();
		document.body.innerHTML = '';
	});

	it('can render another mode before the Radiofax panels become native popovers', () => {
		const descriptor = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'hidePopover');
		const hide = vi.fn(function (this: HTMLElement) {
			if (!this.hasAttribute('popover')) throw new DOMException('Not a popover', 'NotSupportedError');
		});
		Object.defineProperty(HTMLElement.prototype, 'hidePopover', { configurable: true, writable: true, value: hide });
		try {
			const markup = new DOMParser().parseFromString(appHtml, 'text/html');
			document.getElementById('decode-window-radiofax')!.replaceWith(markup.getElementById('decode-window-radiofax')!);
			expect(() => ui.setDecodeWindowAvailable('wfm')).not.toThrow();
			expect(() => ui.initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} })).not.toThrow();
			expect(document.getElementById('decode-window-wfm')!.style.display).not.toBe('none');
		} finally {
			if (descriptor) Object.defineProperty(HTMLElement.prototype, 'hidePopover', descriptor);
			else Reflect.deleteProperty(HTMLElement.prototype, 'hidePopover');
		}
	});

	it('keeps native settings open and focused through ingress, then closes on Escape or outside pointer', () => {
		const settings = document.getElementById('radiofax-settings') as HTMLDetailsElement;
		const summary = settings.querySelector('summary')!;
		const ioc = document.getElementById('radiofax-ioc') as HTMLSelectElement;
		summary.click();
		ioc.focus();
		ioc.value = '288';
		image('paper');
		expect(settings.open).toBe(true);
		expect(document.activeElement).toBe(ioc);
		expect(ioc.value).toBe('288');
		ioc.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
		expect(settings.open).toBe(false);
		expect(document.activeElement).toBe(summary);
		summary.click();
		document.getElementById('radiofax-more')!.querySelector('summary')!.click();
		expect(settings.open).toBe(false);
		expect((document.getElementById('radiofax-more') as HTMLDetailsElement).open).toBe(true);
		document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }));
		expect((document.getElementById('radiofax-more') as HTMLDetailsElement).open).toBe(false);
	});

	it('uses a direct single-segment save and a native multi-segment save choice', () => {
		const exportPng = vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(null));
		const menu = document.getElementById('radiofax-save-menu') as HTMLDetailsElement;
		image('paper');
		click('radiofax-save');
		expect(exportPng).toHaveBeenCalledTimes(1);
		expect(menu.open).toBe(false);
		image('paper', 1, [20]);
		click('radiofax-save');
		expect(exportPng).toHaveBeenCalledTimes(1);
		expect(menu.open).toBe(true);
		click('radiofax-save-segment');
		expect(exportPng).toHaveBeenCalledTimes(2);
		expect(menu.open).toBe(false);
	});

	it('keeps one set of Radiofax handlers after reinitialization and applies the selected zoom', () => {
		const exportPng = vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(null));
		image('paper');
		ui.initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		click('radiofax-save');
		expect(exportPng).toHaveBeenCalledTimes(1);
		const zoom = document.getElementById('radiofax-zoom') as HTMLSelectElement;
		zoom.value = '200';
		zoom.dispatchEvent(new Event('change'));
		expect((document.getElementById('decode-radiofax-canvas') as HTMLCanvasElement).style.width).toBe('2px');
		click('radiofax-follow');
		expect(document.getElementById('radiofax-follow')!.getAttribute('aria-pressed')).toBe('false');
	});

	it('closes an open native menu when the window or receiver mode changes', () => {
		const settings = document.getElementById('radiofax-settings') as HTMLDetailsElement;
		settings.querySelector('summary')!.click();
		ui.setDecodeWindowAvailable('wfm');
		expect(settings.open).toBe(false);
		ui.setDecodeWindowAvailable('radiofax');
		settings.querySelector('summary')!.click();
		ui.toggleDecodeWindow();
		expect(settings.open).toBe(false);
	});

	it('keeps viewed completion separate from live RX and hides diagnostics in Details', () => {
		expect(text('radiofax-state')).toBe('RX: Inactive');
		image('complete', 0, [10, 20], { complete: true });
		const completed = fax.radiofaxVisiblePage()!.id;
		image('live');
		select().value = completed;
		select().dispatchEvent(new Event('change'));
		fax.setRadiofaxDecoderState({ running: true, stage: 'partial', hasPicture: true,
			signalLevel: 0.004, carrierCoherence: 0.75, ioc: 576, lpm: 120, modulation: 'fm', polarity: 'normal',
			width: 1, lines: 3, elapsedSeconds: 1.5, clockStatus: 'tracking', clockSource: 'image_content',
			phasePixels: 1.25, clockPpm: -0.5, clockConfidence: 0.8, complete: false, partial: true, reason: 'signal_lost' });
		ui.refreshDecodeWindow();
		expect(text('radiofax-state')).toContain('RX: Signal lost');
		expect(text('radiofax-page-info')).toBe('Page: Complete · 1 × 2');
		expect(text('radiofax-state')).not.toContain('signal_lost');
		for (const [reason, label] of [['discontinuity', 'Interrupted'], ['new_page', 'Ended'], [null, 'Ended']] as const) {
			fax.setRadiofaxDecoderState({ ...fax.radiofaxStatus().decoder!, reason });
			ui.refreshDecodeWindow();
			expect(text('radiofax-state')).toContain(`RX: ${label}`);
			expect(text('radiofax-page-info')).toBe('Page: Complete · 1 × 2');
		}
		expect(text('radiofax-state')).not.toContain('1.5 s');
		expect(text('radiofax-signal')).toContain('linear amplitude');
		expect(text('radiofax-carrier')).toContain('ratio');
		expect((document.getElementById('radiofax-details') as HTMLDetailsElement).open).toBe(false);
		for (const id of ['radiofax-clock-state', 'radiofax-signal', 'radiofax-carrier', 'radiofax-received', 'radiofax-resource', 'radiofax-reason']) {
			expect(document.getElementById(id)!.closest('details')!.id).toBe('radiofax-details');
		}
		expect(document.getElementById('radiofax-prefix-missing')!.closest('details')).toBeNull();
		expect(document.getElementById('radiofax-save-error')!.closest('details')).toBeNull();
		expect(select().options[0].textContent).toBe('Follow');
		expect(select().options[1].textContent).not.toMatch(/rows|segments/);
	});

	it('pauses Follow at the bottom without pausing ingress and resumes at the newest segment', () => {
		image('paper', 0, [10]);
		const first = fax.radiofaxVisiblePage()!.id;
		const view = document.getElementById('radiofax-view')!;
		Object.defineProperties(view, { scrollHeight: { value: 1000 }, clientHeight: { value: 100 } });
		view.scrollTop = 900;
		click('radiofax-follow');
		expect(document.getElementById('radiofax-follow')!.getAttribute('aria-pressed')).toBe('false');
		expect(fax.radiofaxStatus().viewId).toBe(first);
		image('paper', 1, [20, 21]);
		image('next', 0, [30, 31, 32]);
		view.dispatchEvent(new Event('scroll'));
		expect(fax.radiofaxStatus().pages).toHaveLength(2);
		expect(fax.radiofaxStatus().viewId).toBe(first);
		expect((document.getElementById('decode-radiofax-canvas') as HTMLCanvasElement).height).toBe(1);
		expect(view.scrollTop).toBe(900);
		expect(document.getElementById('radiofax-follow')!.getAttribute('aria-pressed')).toBe('false');
		click('radiofax-follow');
		expect(document.getElementById('radiofax-follow')!.getAttribute('aria-pressed')).toBe('true');
		expect(fax.radiofaxStatus().viewId).toBeNull();
		expect(fax.radiofaxVisiblePage()!.id).not.toBe(first);
		expect((document.getElementById('decode-radiofax-canvas') as HTMLCanvasElement).height).toBe(3);
		expect(view.scrollTop).toBe(1000);
	});

	it('keeps option identity across revisions and freezes mouse or keyboard interaction until blur', () => {
		image('older');
		image('live');
		const older = fax.radiofaxStatus().pages.find((page) => page.segments[0].meta.transmissionId === transmission('older'))!;
		const option = Array.from(select().options).find((item) => item.value === older.id)!;
		select().focus();
		select().dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown' }));
		select().value = older.id;
		const frozen = select().innerHTML;
		select().dispatchEvent(new Event('pointerdown'));
		image('live', 0, [99, 128, 33], { partial: true, reason: 'signal_lost' }, 1);
		image('newest');
		expect(select().innerHTML).toBe(frozen);
		expect(select().value).toBe(older.id);
		expect(document.activeElement).toBe(select());
		select().dispatchEvent(new Event('change'));
		expect(fax.radiofaxStatus().viewId).toBe(older.id);
		select().blur();
		expect(select().options).toHaveLength(4);
		expect(Array.from(select().options).find((item) => item.value === older.id)).toBe(option);
		expect(select().value).toBe(older.id);
		image('next');
		expect(select().value).toBe(older.id);
		expect(fax.radiofaxVisiblePage()!.id).toBe(older.id);
	});

	it('pins the viewed page when scrolling up and never lets a new page steal it', () => {
		image('first');
		const first = fax.radiofaxVisiblePage()!.id;
		const view = document.getElementById('radiofax-view')!;
		Object.defineProperties(view, { scrollHeight: { value: 1000 }, clientHeight: { value: 100 } });
		view.scrollTop = 100;
		view.dispatchEvent(new Event('scroll'));
		expect(fax.radiofaxStatus().viewId).toBe(first);
		expect(document.getElementById('radiofax-follow')!.getAttribute('aria-pressed')).toBe('false');
		image('second');
		expect(fax.radiofaxVisiblePage()!.id).toBe(first);
		expect(view.scrollTop).toBe(100);
		click('radiofax-follow');
		expect(fax.radiofaxStatus().viewId).toBeNull();
		expect(fax.radiofaxVisiblePage()!.segments[0].meta.transmissionId).toBe(transmission('second'));
	});

	it('browses ordered storage segments inside one logical page using 1-based labels', () => {
		image('paper', 0, [10]);
		image('paper', 1, [20, 21]);
		expect(select().options).toHaveLength(2);
		expect(text('radiofax-segment')).toBe('Segment 2 · 2 retained');
		click('radiofax-segment-prev');
		expect(text('radiofax-segment')).toBe('Segment 1 · 2 retained');
		expect((document.getElementById('decode-radiofax-canvas') as HTMLCanvasElement).height).toBe(1);
		image('paper', 2, [30, 31, 32]);
		expect(text('radiofax-segment')).toBe('Segment 1 · 3 retained');
		click('radiofax-segment-next');
		expect(text('radiofax-segment')).toBe('Segment 2 · 3 retained');
	});

	it('shows honest dated local receipt time, switches to UTC and restores the persisted preference', () => {
		const at = Date.parse('2026-10-26T00:00:01Z');
		vi.spyOn(Date, 'now').mockReturnValue(at);
		image('dated');
		expect(document.getElementById('btn-decode-utc')!.style.display).toBe('');
		expect(text('radiofax-received')).toContain('First browser image receipt');
		expect(text('radiofax-received')).toMatch(/2026-10-\d{2} .* LT/);
		expect(document.getElementById('radiofax-received')!.title).toContain("not the station's transmission time");
		click('btn-decode-utc');
		expect(text('radiofax-received')).toContain('2026-10-26 00:00:01 UTC');
		expect(JSON.parse(localStorage.getItem('websa-decode-window')!).utc).toBe(true);
		ui.initDecodeWindow({ onTune: () => {}, onSstvMode: () => {} });
		ui.refreshDecodeWindow();
		expect(document.getElementById('btn-decode-utc')!.classList.contains('active')).toBe(true);
		expect(text('radiofax-received')).toContain('2026-10-26 00:00:01 UTC');
		vi.mocked(Date.now).mockReturnValue(Date.parse('2026-10-25T23:59:59Z'));
		image('previous-day');
		expect(Array.from(select().options).map((option) => option.textContent)).toEqual(expect.arrayContaining([
			expect.stringContaining('2026-10-25 23:59:59 UTC'), expect.stringContaining('2026-10-26 00:00:01 UTC'),
		]));
	});

	it('preserves neutral missing bands in canvas and PNG and shows partial reason metadata', () => {
		const pixels: Uint8ClampedArray[] = [];
		const context = {
			createImageData: (width: number, height: number) => ({ data: new Uint8ClampedArray(width * height * 4) }),
			putImageData: (image: ImageData) => pixels.push(image.data),
		} as unknown as CanvasRenderingContext2D;
		vi.mocked(HTMLCanvasElement.prototype.getContext).mockReturnValue(context);
		const blob = vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(null));
		image('partial', 0, [10, 128, 30], { missingRows: [1], partial: true, reason: 'signal_lost' });
		expect(text('radiofax-missing')).toContain('1 missing rows');
		expect(text('radiofax-page-info')).toContain('Partial');
		expect(text('radiofax-page-info')).not.toContain('signal_lost');
		expect(text('radiofax-reason')).toContain('signal_lost');
		expect(document.getElementById('radiofax-missing')!.title).toContain('neutral gray bands');
		expect(Array.from(pixels.at(-1)!)).toEqual([10, 10, 10, 255, 128, 128, 128, 255, 30, 30, 30, 255]);
		click('radiofax-save');
		expect(blob).toHaveBeenCalled();
		expect(Array.from(pixels.at(-1)!)).toEqual([10, 10, 10, 255, 128, 128, 128, 255, 30, 30, 30, 255]);
		expect(text('radiofax-save-error')).toBe('PNG export failed');
	});

	it('keeps pixels current through growth, overlaps, revisions and page switches without repainting status updates', () => {
		const pixels: number[][] = [];
		const allocate = vi.fn((width: number, height: number) => ({ data: new Uint8ClampedArray(width * height * 4) }));
		vi.mocked(HTMLCanvasElement.prototype.getContext).mockReturnValue({
			createImageData: allocate,
			putImageData: (image: ImageData) => pixels.push(Array.from(image.data)),
		} as unknown as CanvasRenderingContext2D);
		const canvas = document.getElementById('decode-radiofax-canvas') as HTMLCanvasElement;
		const rgba = (values: number[]) => values.flatMap((value) => [value, value, value, 255]);
		const append = (row: number, values: number[]) => {
			const entry = fax.radiofaxVisible()!;
			fax.appendRadiofaxRows({ width: 1, row, rows: values.length, gray: Uint8Array.from(values), revision: entry.revision },
				{ ...entry.meta, lines: row + values.length }, 'stream-epoch');
			ui.refreshDecodeWindow();
		};
		image('paper', 0, [10, 128, 30]);
		const paper = fax.radiofaxVisiblePage()!.id;
		expect(pixels.at(-1)).toEqual(rgba([10, 128, 30]));
		const paints = allocate.mock.calls.length;
		fax.setRadiofaxDecoderState(null);
		ui.refreshDecodeWindow();
		expect(allocate).toHaveBeenCalledTimes(paints);
		expect(pixels.at(-1)).toEqual(rgba([10, 128, 30]));
		append(3, [40, 50]);
		expect([canvas.width, canvas.height]).toEqual([1, 5]);
		expect(pixels.at(-1)).toEqual(rgba([10, 128, 30, 40, 50]));
		append(1, [99]);
		expect(pixels.at(-1)).toEqual(rgba([10, 99, 30, 40, 50]));
		image('paper', 0, [70, 80], {}, 1);
		expect([canvas.width, canvas.height]).toEqual([1, 5]);
		expect(pixels.at(-1)).toEqual(rgba([70, 80, 30, 40, 50]));
		append(2, [90, 100, 110]);
		expect(pixels.at(-1)).toEqual(rgba([70, 80, 90, 100, 110]));
		image('next', 0, [21]);
		expect(pixels.at(-1)).toEqual(rgba([21]));
		select().value = paper;
		select().dispatchEvent(new Event('change'));
		expect(pixels.at(-1)).toEqual(rgba([70, 80, 90, 100, 110]));
		canvas.width = 2;
		ui.refreshDecodeWindow();
		expect([canvas.width, canvas.height]).toEqual([1, 5]);
		expect(pixels.at(-1)).toEqual(rgba([70, 80, 90, 100, 110]));
		const sizes: number[][] = [];
		vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation(function (this: HTMLCanvasElement, callback) {
			sizes.push([this.width, this.height]); callback(null);
		});
		click('radiofax-save');
		expect(sizes).toEqual([[1, 5]]);
		expect(pixels.at(-1)).toEqual(rgba([70, 80, 90, 100, 110]));
	});

	it('exports one safe whole page and keeps UTC segment filenames 1-based', () => {
		const downloads: string[] = [];
		vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({
			createImageData: (width: number, height: number) => ({ data: new Uint8ClampedArray(width * height * 4) }),
			putImageData: vi.fn(), drawImage: vi.fn(),
		} as unknown as CanvasRenderingContext2D);
		const sizes: number[][] = [];
		vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation(function (this: HTMLCanvasElement, callback) {
			sizes.push([this.width, this.height]); callback(new Blob(['png'], { type: 'image/png' }));
		});
		vi.stubGlobal('URL', { createObjectURL: () => 'blob:fax', revokeObjectURL: vi.fn() });
		vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) { downloads.push(this.download); });
		vi.spyOn(Date, 'now').mockReturnValue(Date.parse('2026-10-26T12:00:00Z'));
		image('paper', 0, [10]); image('paper', 1, [20, 21]);
		click('radiofax-save'); click('radiofax-save-segment'); click('radiofax-save'); click('radiofax-save-page');
		expect(sizes).toEqual([[1, 2], [1, 3]]);
		expect(downloads[0]).toBe(`radiofax-20261026T120000Z-9110000Hz-IOC576-120LPM-p${transmission('paper')}-s2.png`);
		expect(downloads[1]).toContain('-page.png');
	});

	it('refuses oversized whole-page export before canvas allocation and leaves segment export available', () => {
		image('wide', 0, Array(32_768).fill(128), {}, 0, 32_768);
		const exportPng = vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(null));
		click('radiofax-save-page');
		expect(exportPng).not.toHaveBeenCalled();
		expect((document.getElementById('radiofax-save-page') as HTMLButtonElement).disabled).toBe(true);
		expect(text('radiofax-export-limit')).toContain('Save each segment PNG instead');
		expect(document.getElementById('radiofax-save')!.getAttribute('aria-disabled')).toBe('false');
	});

	it('rejects whole-page RGBA allocation above the pixel budget even when each dimension is safe', () => {
		const pixels = new Uint8Array(4096 * 2049).fill(128);
		image('large', 0, pixels, {}, 0, 4096);
		image('large', 1, pixels, {}, 0, 4096);
		const exportPng = vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(null));
		click('radiofax-save-page');
		expect(exportPng).not.toHaveBeenCalled();
		expect(text('radiofax-export-limit')).toContain('16,777,216 pixels');
		expect((document.getElementById('radiofax-save-segment') as HTMLButtonElement).disabled).toBe(false);
	});

	it('warns when a late snapshot has lost earlier segments and keeps whole-page export unavailable', () => {
		image('snapshot', 4, [10, 128, 30], { partial: true, reason: 'snapshot_prefix_missing', missingRows: [1] });
		expect(text('radiofax-prefix-missing')).toContain('Earlier segments are unavailable');
		expect(text('radiofax-page-info')).toContain('Partial');
		expect(text('radiofax-segment')).toBe('Segment 5 · 1 retained');
		expect(document.getElementById('radiofax-segments')!.classList.contains('shown')).toBe(false);
		const exportPng = vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((callback) => callback(null));
		click('radiofax-save-page');
		expect(exportPng).not.toHaveBeenCalled();
		language.setLang('zh');
		ui.refreshDecodeWindow();
		expect(text('radiofax-prefix-missing')).toContain('不含较早的分段');
		expect(text('radiofax-missing')).toContain('缺失 1 行');
		expect(text('radiofax-segment')).toContain('第 5 段');
	});

	it('exposes eviction and refused retention while preserving the viewed page and save/delete controls', () => {
		const retain = (transmissionId: string, segmentIndex = 0, rows = 2400) => {
			fax.appendRadiofaxRows({ width: 4096, row: 0, rows, gray: new Uint8Array(4096 * rows), revision: 0 }, {
				serial: `${transmissionId}-${segmentIndex}`, epoch: 'metadata-epoch', hz: 9_110_000, ioc: 576, lpm: 120,
				lines: rows, complete: false, partial: false, segmentIndex, transmissionId: transmission(transmissionId), missingRows: [], reason: null,
			}, 'stream-epoch');
			ui.refreshDecodeWindow();
		};
		retain('first'); retain('second'); retain('third');
		expect(text('radiofax-eviction')).toContain('1 evicted');
		const second = fax.radiofaxStatus().pages.find((page) => page.segments[0].meta.transmissionId === transmission('second'))!;
		select().value = second.id;
		select().dispatchEvent(new Event('change'));
		retain('third', 1, 400);
		expect(fax.radiofaxStatus().retention).toBe('blocked');
		expect(fax.radiofaxVisiblePage()!.id).toBe(second.id);
		expect(text('radiofax-budget')).toContain('Storage full');
		expect(document.getElementById('radiofax-budget')!.closest('details')).toBeNull();
		expect(document.getElementById('radiofax-save')!.getAttribute('aria-disabled')).toBe('false');
		expect((document.getElementById('radiofax-delete') as HTMLButtonElement).disabled).toBe(false);
	});

	it('deletes a pinned historical page after confirmation and safely falls back to the receiving page', () => {
		image('older');
		const older = fax.radiofaxVisiblePage()!.id;
		image('live');
		select().value = older;
		select().dispatchEvent(new Event('change'));
		click('radiofax-delete');
		expect(fax.radiofaxStatus().viewId).toBe(older);
		click('radiofax-confirm-delete');
		expect(fax.radiofaxStatus().viewId).toBeNull();
		expect(fax.radiofaxVisiblePage()!.segments[0].meta.transmissionId).toBe(transmission('live'));
		expect(select().value).toBe('');
	});

	it('requires confirmation for selected or receiving page deletion and channel clear', () => {
		image('older'); image('receiving');
		const receiving = fax.radiofaxVisiblePage()!.id;
		click('radiofax-delete');
		expect(fax.radiofaxStatus().pages).toHaveLength(2);
		expect(text('radiofax-confirm-prompt')).toContain('receiving page');
		click('radiofax-confirm-cancel');
		expect(fax.radiofaxStatus().pages).toHaveLength(2);
		select().focus();
		click('radiofax-delete'); click('radiofax-confirm-delete');
		expect(fax.radiofaxStatus().pages.some((page) => page.id === receiving)).toBe(false);
		expect(fax.radiofaxStatus().currentSuppressed).toBe(true);
		expect(select().selectedIndex).toBeGreaterThanOrEqual(0);
		expect(text('radiofax-suppressed')).toContain('later rows stay hidden');
		click('radiofax-clear');
		expect(text('radiofax-confirm-prompt')).toContain('selected and receiving pages');
		click('radiofax-confirm-delete');
		expect(fax.radiofaxStatus().pages).toHaveLength(0);
		expect(text('radiofax-activity')).toBe('Channel history cleared');
	});
});
