import './sdrScan.css';
import { html, render } from 'lit-html';
import { repeat } from 'lit-html/directives/repeat.js';
import { unsafeHTML } from 'lit-html/directives/unsafe-html.js';
import { workbenchIcon } from './workbenchIcons';
import { ScanAudition, isScanPreview } from './sdrScanAudition';
import { defaultScanFilters, scanResults, type ScanSort } from './sdrScanResults';
import { getLang, onLangChange } from '../core/i18n';
import { CHANNEL_COLORS } from '../core/channelColors';
import { getUiScale } from '../core/uiScale';
import { defaultTransitionHz, occupiedBandwidth } from '../core/receiverBandwidth';
import type { ReceiverConfig, ReceiverState } from '../core/multichannel';
import { createFloatingWindow, type FloatingWindow } from './floatingWindow';

import type { WorkbenchSession } from './workbenchSession';
import { beginIdentifySelection, highlightIdentifyBand, identifySelecting, initIdentifySelection, setIdentifySource, type IdentifyRequest } from './sdrIdentifySelection';
import { currentGraphMode } from './graphMode';
import type { ScanSignal, ScanSnapshot } from '../core/sdrScan';
import { ScanUpdates } from './sdrScanUpdates';

const l = (en: string, zh: string) => getLang() === 'zh' ? zh : en;
const mhz = (hz: number) => (hz / 1e6).toFixed(6);
const names: Record<string, [string, string]> = {
  unknown: ['Unknown', '未知'], carrier: ['Carrier', '载波'], 'AM-like': ['AM-like', 'AM 类'], 'FM-like': ['FM-like', 'FM 类'], 'FM stereo pilot': ['FM · stereo pilot', 'FM · 立体声导频'],
  '2FSK-like': ['2FSK-like', '2FSK 类'], '4FSK-like': ['4FSK-like', '4FSK 类'], 'PSK-like': ['PSK-like', 'PSK 类'],
  tentative: ['Tentative', '待确认'], credible: ['Credible', '较可信'], confirmed: ['Confirmed', '已确认'],
  idle: ['Not identified', '待识别'], queued: ['Queued', '排队中'], sampling: ['Sampling', '采样中'], analyzing: ['Analyzing', '分析中'], detecting: ['Sampling region', '采样选区'],
  waiting: ['Waiting for a free receiver slot', '等待空闲接收资源'], unavailable: ['Cannot probe this range', '此范围暂无法探测'],
  discontinuous: ['IQ gap; retry', 'IQ 不连续，请重试'], insufficient_iq: ['Not enough contiguous IQ', '连续 IQ 不足'], cancelled: ['Cancelled', '已取消'],
  throttled: ['Scan paused under receiver load', '接收压力较高，扫描暂缓'],
  budget_skipped: ['Analysis budget reached', '达到分析预算'], running: ['Scanning', '扫描中'], warming: ['Estimating background', '正在估计本底'], paused: ['Paused', '已暂停'], stopped: ['Stopped', '未扫描'],
  not_detected: ['No signal in the current observation', '当前观察未发现信号'], stale: ['Source changed; repeat identification', '源已变化，请重新识别'],
  snapshot: ['Snapshot', '静态快照'],
  capacity: ['Discovery continues; identification waits for a free slot', '发现继续，识别等待空闲资源'],
  source_changed: ['Capture changed; previous results expired', '采集已变化，旧结果已失效'], capture_pressure: ['Scan slowed to protect reception', '已降低扫描频率，优先保证接收'],
  live_iq_spectrum: ['Current IQ · spectrum selection', '当前 IQ · 频谱选区'], live_iq_waterfall: ['Current IQ · waterfall frequency band', '当前 IQ · 瀑布频段'], live_iq_manual: ['Current IQ · selected region', '当前 IQ · 选定频段'],
  analysis_budget: ['Analysis duty cycle reduced', '已降低分析占空比'], unseen: ['Not seen recently', '近期未见'], short: ['Short activity', '短时活动'], active: ['Active', '活跃'],
  analysis_passband_limit: ['Candidate exceeds one analysis passband; select a narrower signal', '候选超出单路分析通带，请缩小选区到具体信号'],
  no_signal_detected: ['No signal detected in this selection', '选区内未发现信号'],
};
const text = (key: string) => names[key] ? l(...names[key]) : key;
function describeEvidence(ev: Record<string, unknown>): string {
  const parts: string[] = [];
  const metric = (key: string, label: string, scale = 1, unit = '', digits = 2) => { const value = ev[key]; if (typeof value === 'number' && Number.isFinite(value)) parts.push(`${label} ${(value/scale).toFixed(digits)}${unit}`); };
  metric('duration_seconds', l('Observed', '已观察'), 1, ' s'); metric('sample_rate_hz',l('IQ rate', 'IQ 速率'),1000,' kS/s',1);
  metric('envelope_cv',l('Envelope variation', '包络变化')); metric('frequency_deviation_rms_hz',l('Discriminator RMS', '鉴频 RMS'),1000,' kHz');
  if (ev.fm_19khz_pilot === true) parts.push(l('19 kHz pilot verified in three windows', '三段观测验证了 19 kHz 导频'));
  if (typeof ev.discriminator_clusters === 'number' && ev.discriminator_clusters >= 2) parts.push(l(`${ev.discriminator_clusters} discriminator plateaus`, `${ev.discriminator_clusters} 个鉴频平台`));
  const protocol = ev.protocol_evidence as {protocol?: string;rds_groups?: number;drm_fac_ok?: number;ft8_text?: string}|undefined;
  if (protocol?.protocol) parts.push(`${protocol.protocol}: ${protocol.rds_groups ? l(`${protocol.rds_groups} valid groups`, `${protocol.rds_groups} 个有效组`) : protocol.drm_fac_ok ? l(`${protocol.drm_fac_ok} valid FAC`, `${protocol.drm_fac_ok} 个有效 FAC`) : protocol.ft8_text ?? ''}`);
  if (!parts.length && ev.reason) parts.push(text(String(ev.reason)));
  return parts.join(' · ') || l('Waiting for enough contiguous IQ', '正在等待足够的连续 IQ');
}

let summary: { running: boolean; state: string; service_id?: string; capture?: ScanSnapshot['capture'] } | null = null;
let mounted: ReturnType<typeof mount> | null = null, opening: Promise<ReturnType<typeof mount> | null> | null = null;
function updateToggle(): void {
  const button = document.querySelector<HTMLButtonElement>('#sdr-scan-toggle'); if (!button) return;
  const visible = mounted?.visible() ?? false;
  button.setAttribute('aria-label', l('Scan / Identify', '扫描／识别')); button.setAttribute('aria-pressed', String(visible)); button.setAttribute('aria-expanded', String(visible));
  button.dataset.running = String(summary?.running === true);
  button.title = `${l('Scan / Identify', '扫描 / 识别')} · ${summary ? text(summary.state) : l('Status unknown', '状态待核实')}`;
}
export function acceptScanStatus(status: { connected?: boolean; mode?: string; sdr?: { scan?: typeof summary } }): void {
  summary = status.connected && status.mode === 'sdr' ? status.sdr?.scan ?? null : null;
  if (!summary) setIdentifySource('disconnected', '', 1000, []);
  else if (summary.capture) { const c = summary.capture; setIdentifySource(JSON.stringify([summary.service_id,c.epoch,c.center_hz,c.usable_bandwidth_hz,c.state]),c.epoch,c.input_rate_hz ? c.input_rate_hz/65536 : 1000); }
  updateToggle();
}
async function ensure(): Promise<ReturnType<typeof mount> | null> {
  if (mounted) return mounted; if (opening) return opening;
  opening = (async () => { const session = await (await import('./nativeWorkbench')).nativeWorkbenchSession(); return session ? mounted = mount(session) : null; })();
  try { return await opening; } finally { opening = null; }
}
export function initSdrScan(): void {
  initIdentifySelection();
  document.querySelector('#sdr-scan-toggle')?.addEventListener('click', () => { void ensure().then(view => view?.toggle()).catch(showError); });
  document.addEventListener('websa:identify-region', event => {
    const request = (event as CustomEvent<IdentifyRequest>).detail;
    void ensure().then(async view => { view?.show(); await view?.identifyRegion(request); }).catch(showError);
  });
  onLangChange(updateToggle); updateToggle();
}
function showError(error: unknown): void { console.error('Scan / Identify:', error); }

function mount(session: WorkbenchSession) {
  let snapshot: ScanSnapshot | null = null, selected = '', details = false, more = false, tableSettings = false, error = '', connectionError = '', notice = '', busy = false;
  const audition = new ScanAudition(session), filters = defaultScanFilters(), modes = new Map<string, string>();
  let exclusive = localStorage.getItem('web-sa-scan-exclusive') !== 'false', measurement = 'discovery', frozenOrder: string[] | null = null, renderedRows: ScanSignal[] = [];
  let pointerInRows = false;
  const chosenMode = (s: ScanSignal) => modes.get(s.id) ?? 'auto';
  const icon = (name: string) => unsafeHTML(workbenchIcon(name));
  let win: FloatingWindow | undefined, visible = false, polling = false, refreshAgain = false, version = 0, disposed = false, focusResults = false;
  let refreshPending: Promise<void> = Promise.resolve(), commandPending: Promise<void> = Promise.resolve(), regionVersion = 0;
  const lifetime = new AbortController();
  const button = document.querySelector<HTMLButtonElement>('#sdr-scan-toggle');
  const evidence = new Map<string, Record<string, unknown>>();
  const updates = new ScanUpdates(() => { void refresh(); });
  async function readEvidence(s: ScanSignal): Promise<void> { const service = snapshot?.service_id, epoch = snapshot?.capture.epoch; try { const r = await fetch(`/api/sdr/scan/signals/${s.id}`, { signal: lifetime.signal }); if (!r.ok) return; const full = await r.json() as ScanSignal; if (full.service_id === service && full.current_capture_epoch === epoch && service === snapshot?.service_id && epoch === snapshot?.capture.epoch && full.generation === s.generation) { evidence.set(s.id, full.evidence ?? {}); paint(); } } catch { /* The next explicit request can retry. */ } }
  const selectedSignal = () => snapshot?.signals.find(s => s.id === selected);
  const valid = (s?: ScanSignal) => !!s && s.activity !== 'stale' && snapshot?.capture.epoch === session.snapshot.capture.epoch && snapshot.service_id === session.snapshot.service_id;
  const statusChanged = () => {
    if (!win) return;
    visible = !win.root.hidden;
    updates.setVisible(visible);
    if (!visible) { version++; void audition.stop().catch(reason => { error = String(reason); }); button?.focus({ preventScroll: true }); }
    updateToggle(); if (visible) void refresh();
  };
  const closeDetails = (event: KeyboardEvent) => {
    if (event.key === 'Escape' && details) { details = false; event.preventDefault(); event.stopPropagation(); paint(); return true; } return false;
  };
  const scale = getUiScale();
  const plotRight = document.querySelector('.spectrum-area')?.getBoundingClientRect().right ?? innerWidth;
  win = createFloatingWindow('sdr-scan', () => l('Scan / Identify', '扫描 / 识别'),
    { x: Math.max(16, plotRight - 740 * scale), y: Math.max(20, innerHeight - 510 * scale), w: 720 * scale, h: 460 * scale }, statusChanged,
    { minWidth: 520, minHeight: 300, closeTip: () => l('Hide window; scanning continues', '隐藏窗口，扫描继续'), escape: closeDetails });
  win.root.id = 'sdr-scan-window'; win.root.classList.add('sdr-scan-window'); win.hide();
  function accept(value: ScanSnapshot): void {
    if (value.service_id !== session.snapshot.service_id) return;
    if (snapshot?.service_id === value.service_id && BigInt(value.revision) < BigInt(snapshot.revision)) return;
    for (const id of evidence.keys()) if (!value.signals.some(s => s.id === id)) evidence.delete(id);
    for (const id of modes.keys()) if (!value.signals.some(s => s.id === id)) modes.delete(id);
    if (!tableSettings && value.signals.some(s => s.source === 'channel')) measurement = 'table';
    snapshot = value; summary = { running: value.running, state: value.state, service_id: value.service_id }; updateToggle();
    if (audition.playing && value.signals.some(s => s.id === audition.playing!.signal && s.activity === 'stale')) void audition.stop().catch(reason => { error = String(reason); });
    const capture = value.capture;
    setIdentifySource(JSON.stringify([value.service_id, capture.epoch, capture.center_hz, capture.usable_bandwidth_hz, capture.state]), capture.epoch, value.bin_spacing_hz ?? 1000, value.signals);
    if (selected && !visibleRows().some(s => s.id === selected)) { selected = ''; details = false; }
    const row = selectedSignal(); highlightIdentifyBand(valid(row) ? row! : null); paint();
    if (focusResults && value.signals.some(s => s.source === 'region' && s.identify_state !== 'detecting')) {
      selected = value.signals.find(s => s.source === 'region')!.id; focusResults = false; paint();
      win?.root.querySelector<HTMLElement>(`[data-signal="${selected}"]`)?.focus({ preventScroll: true });
    }
    if (focusResults && value.reason === 'no_signal_detected') { focusResults = false; win?.root.querySelector<HTMLElement>('[data-scan-select]')?.focus({ preventScroll: true }); }
  }
  async function refresh(): Promise<void> {
    if (busy) { refreshAgain = true; return; }
    if (polling) { refreshAgain = true; return refreshPending; }
    if (!visible || disposed || document.hidden && !win?.portal.isPoppedOut()) return;
    let finish = () => {}; refreshPending = new Promise<void>(resolve => { finish = resolve; });
    polling = true; const generation = version;
    try {
      const response = await fetch('/api/sdr/scan', { signal: AbortSignal.any([lifetime.signal, AbortSignal.timeout(5000)]) });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const value = await response.json() as ScanSnapshot;
      connectionError = '';
      if (generation === version && visible) { if (busy) refreshAgain = true; else accept(value); }
    } catch (reason) { if (!disposed) { connectionError = String(reason); summary = null; updateToggle(); paint(); } }
    finally { polling = false; finish(); if (refreshAgain && !busy) { refreshAgain = false; void refresh(); } }
  }
  async function command(type: string, fields: Record<string, unknown> = {}): Promise<void> {
    if (!snapshot || busy) return;
    const service = session.snapshot.service_id;
    const request = { type, service_id: service, request_id: crypto.randomUUID(), scan_id: snapshot.scan_id,
      expected_revision: snapshot.revision, capture_epoch: snapshot.capture.epoch, ...fields };
    let finishCommand = () => {}; commandPending = new Promise<void>(resolve => { finishCommand = resolve; });
    busy = true; error = ''; paint();
    try {
      const response = await fetch('/api/sdr/scan', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(request), signal: AbortSignal.any([lifetime.signal, AbortSignal.timeout(8000)]) });
      const value = await response.json(); if (!response.ok) throw new Error(value.error ?? `HTTP ${response.status}`);
      if (value.request_id !== request.request_id) throw new Error(l('Scan response identity mismatch', '扫描回包身份不匹配'));
      if (service === session.snapshot.service_id) accept(value as ScanSnapshot);
    } catch (reason) { error = String(reason instanceof Error ? reason.message : reason); }
    finally { busy = false; finishCommand(); paint(); if (refreshAgain) { refreshAgain = false; void refresh(); } }
  }
  function configFor(s: ScanSignal): ReceiverConfig {
    const mode = chosenMode(s), chosen = mode === 'auto' ? s.suggested_mode ?? 'iq' : mode;
    const descriptor = session.snapshot.capabilities.mode_descriptors?.find(d => d.id === chosen);
    const bandwidth = Math.max(chosen === 'wfm' ? 180000 : chosen === 'cw' ? 1000 : chosen === 'am' ? 12000 : 2500, s.bandwidth_hz);
    const passband = descriptor ? occupiedBandwidth(descriptor, bandwidth, 700) : bandwidth, transition = defaultTransitionHz(passband);
    return { name: `${mhz(s.center_hz)} MHz · ${chosen.toUpperCase()}`, color: CHANNEL_COLORS[session.snapshot.receivers.length % CHANNEL_COLORS.length], tags: [], enabled: true,
      purpose: chosen === 'iq' ? 'iq' : descriptor?.kind === 'digital' ? 'digital' : 'listen', baseband: { frequency_hz: s.center_hz, passband_hz: passband, transition_hz: transition,
        minimum_rate_hz: Math.max(descriptor?.minimumRateHz ?? 0, passband + 2 * transition) },
      processors: chosen === 'iq' ? [] : [{ id: `scan-${crypto.randomUUID()}`, kind: descriptor?.kind === 'digital' ? 'decoder' : 'demodulator', mode: chosen, bandwidth_hz: bandwidth,
        minimum_rate_hz: descriptor?.minimumRateHz ?? 0, pcm_gain: 0.7, agc: true, squelch_dbm: -120, nr: false, nr_algorithm: 'wiener', nr_strength: 0.5, nr_atten_db: 20 }] };
  }
  function duplicate(s: ScanSignal): ReceiverState | undefined { return audition.reusable(configFor(s)); }
  function canListen(s: ScanSignal): boolean {
    const mode = chosenMode(s), chosen = mode === 'auto' ? s.suggested_mode : mode;
    return !!chosen && session.snapshot.capabilities.mode_descriptors?.some(d => d.id === chosen && d.available && (d.kind === 'analog' || d.outputs.includes('audio') || d.audioCompanion !== null)) === true;
  }
  async function stopAudition(): Promise<void> { try { await audition.stop(); } catch (reason) { error = String(reason); } paint(); }
  async function receive(s: ScanSignal, listen: boolean): Promise<void> {
    if (!valid(s) || busy) return;
    selected = s.id;
    if (listen && audition.playing?.signal === s.id && audition.playing.mode === configFor(s).processors[0]?.mode) { await stopAudition(); return; }
    if (s.flags.includes('analysis_passband_limit')) { notice = text('analysis_passband_limit'); paint(); return; }
    let finishReception = () => {}; commandPending = new Promise<void>(resolve => { finishReception = resolve; });
    busy = true; error = ''; notice = ''; paint(); const service = session.snapshot.service_id, epoch = snapshot!.capture.epoch;
    try {
      const freshResponse = await fetch(`/api/sdr/scan/signals/${s.id}`, { signal: lifetime.signal });
      if (!freshResponse.ok) throw new Error(l('Result expired', '结果已失效'));
      const fresh = await freshResponse.json() as ScanSignal;
      if (fresh.service_id !== service || fresh.current_capture_epoch !== epoch || fresh.generation !== s.generation || fresh.activity === 'stale' || service !== session.snapshot.service_id || epoch !== session.snapshot.capture.epoch) throw new Error(l('Result or capture changed; retry', '结果或采集状态已变化，请重试'));
      const guard = { capture_epoch: epoch, signal_id: fresh.id, signal_generation: fresh.generation };
      if (listen) await audition.start(fresh.id, configFor(fresh), guard, exclusive);
      else {
        const receiver = await audition.add(configFor(fresh), guard);
        if (receiver) { s.receiver_uid = receiver.receiver_uid; notice = l(`Added C${receiver.display_number}`, `已加入 C${receiver.display_number}`); }
      }
    } catch (reason) { error = String(reason instanceof Error ? reason.message : reason); }
    finally { busy = false; finishReception(); paint(); if (refreshAgain) { refreshAgain = false; void refresh(); } }
  }
  function visibleRows(): ScanSignal[] {
    const rows = scanResults(snapshot?.signals ?? [], filters, s => !!duplicate(s), s => [text(s.label), text(s.activity), text(s.identify_state), text(s.verdict)].join(' '));
    if (frozenOrder) { const rank = new Map(frozenOrder.map((id, i) => [id, i])); rows.sort((a, b) => (rank.get(a.id) ?? Infinity) - (rank.get(b.id) ?? Infinity)); }
    return rows;
  }
  function filterChanged(): void {
    frozenOrder = null;
    if (selected && !visibleRows().some(s => s.id === selected)) { selected = ''; details = false; highlightIdentifyBand(null); }
    if (pointerInRows) frozenOrder = visibleRows().map(s => s.id);
    paint();
  }
  function sortBy(sort: ScanSort): void {
    filters.descending = filters.sort === sort ? !filters.descending : sort !== 'frequency' && sort !== 'type'; filters.sort = sort; filterChanged();
  }
  async function clearResults(scope = 'all'): Promise<void> {
    if (busy) return; busy = true; paint();
    try { await audition.stop(); } catch (reason) { error = String(reason); busy = false; paint(); if (refreshAgain) { refreshAgain = false; void refresh(); } return; }
    busy = false; await command('clear', { scope }); if (!error) { selected = ''; details = false; focusResults = false; evidence.clear(); modes.clear(); frozenOrder = null; highlightIdentifyBand(null); notice = snapshot?.running ? l('Cleared. New detections will appear while scanning.', '记录已清除；扫描继续，新命中会重新出现。') : l('Results cleared', '记录已清除'); paint(); }
  }
  async function measureChannels(channels: { low_hz: number; high_hz: number }[]): Promise<void> {
    if (!channels.length) { error = l('No saved channels to measure', '尚无可测量的已保存信道'); paint(); return; }
    await command('configure', { range: null, channels }); if (error) return;
    measurement = 'table'; tableSettings = false; if (!snapshot?.running) await command('start'); paint();
  }
  const table = { start: String((session.snapshot.capture.center_hz - 100000) / 1e6), stop: String((session.snapshot.capture.center_hz + 100000) / 1e6), step: '12.5', width: '12.5' };
  function selectRow(s: ScanSignal): void { selected = s.id; details = false; highlightIdentifyBand(s); paint(); }
  function row(s: ScanSignal) {
    const saved = duplicate(s), playing = audition.playing?.signal === s.id, limited = s.flags.includes('analysis_passband_limit'), capable = valid(s) && (!limited || playing);
    const state = s.protocol ? `${s.protocol} · ${text('confirmed')}` : s.label === 'unknown' ? text('unknown') : text(s.label);
    const status = s.activity === 'stale' || s.activity === 'not_detected' ? text(s.activity) : limited ? text('analysis_passband_limit') : s.identify_state === 'complete' ? text(s.verdict) : text(s.identify_state);
    const capacity = !saved && !audition.canAllocate();
    return html`<tr role="row" data-signal=${s.id} data-identify-state=${s.identify_state} data-stale=${s.activity === 'stale'} aria-selected=${s.id === selected} tabindex=${s.id === selected || !selected && s.id === renderedRows[0]?.id ? 0 : -1}
      @mouseenter=${() => highlightIdentifyBand(s)} @mouseleave=${() => highlightIdentifyBand(selectedSignal() ?? null)} @click=${() => selectRow(s)}
      @keydown=${(e: KeyboardEvent) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === 'Enter') { e.preventDefault(); selected = s.id; details = !details; if (details) void readEvidence(s); paint(); }
        if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); const rows = renderedRows, i = rows.indexOf(s), target = rows[Math.max(0, Math.min(rows.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))]; if (target) { selectRow(target); win?.root.querySelector<HTMLElement>(`[data-signal="${target.id}"]`)?.focus(); } }
      }}>
      <td><span class="sdr-scan-frequency">${mhz(s.center_hz)}</span><small>${saved ? `C${saved.display_number}` : s.source === 'region' ? l('Selection', '选区') : s.source === 'channel' ? l('Fixed frequency', '固定频点') : l('Discovered', '发现')}${s.activity === 'unseen' ? ` · ${text('unseen')}` : ''}</small></td>
      <td><span>${state}</span><small title=${status}>${status}</small></td>
      <td title=${s.source === 'channel' || s.activity === 'not_detected' || s.flags.includes('requested_window') ? l('Measurement window', '测量窗口') : ''}>${(s.bandwidth_hz / 1000).toFixed(1)}<small>kHz${s.source === 'channel' || s.activity === 'not_detected' ? l(' window', ' 窗口') : ''}</small></td>
      <td title=${`${l('Observed hit ratio', '观测命中率')} ${(s.observed_hit_ratio * 100).toFixed(0)}%`}>${s.excess_db.toFixed(1)}<small>dB ${l('above floor', '高于本底')}</small></td>
      <td class="sdr-scan-row-actions"><div><button data-scan-listen aria-pressed=${playing} aria-label=${l(`${playing ? 'Stop preview' : 'Preview'} ${mhz(s.center_hz)} MHz`, `${playing ? '停止试听' : '试听'} ${mhz(s.center_hz)} MHz`)} title=${!canListen(s) ? l('Select this row and choose an audio mode below', '选中此行，在下方选择音频模式即可试听') : capacity ? l('Receiver slots full', '接收资源已满') : l('Temporary preview; stop to restore the previous mix', '临时试听，停止后恢复原混音')} ?disabled=${busy || !capable || !canListen(s) || capacity} @click=${(e: Event) => { e.stopPropagation(); void receive(s, true); }}>${icon(playing ? 'stop' : 'headphones')}<span>${playing ? l('Stop', '停止') : l('Hear', '试听')}</span></button>
        <button data-scan-add aria-label=${l(`Add ${mhz(s.center_hz)} MHz`, `加入 ${mhz(s.center_hz)} MHz`)} title=${saved ? l(`Open C${saved.display_number}`, `打开已加入的 C${saved.display_number}`) : l('Keep a channel for reception or recording', '保留为正式信道，供接收或录制')} ?disabled=${busy || !capable || capacity} @click=${(e: Event) => { e.stopPropagation(); if (saved) { session.select(saved.receiver_uid); void import('./nativeWorkbench').then(m => m.openNativeChannelOverview()); } else void receive(s, false); }}>${icon(saved ? 'check' : 'plus')}<span>${saved ? `C${saved.display_number}` : configFor(s).purpose === 'iq' ? l('IQ', '加入 IQ') : l('Add', '加入')}</span></button></div></td>
    </tr>`;
  }
  function paint(): void {
    if (!win || !visible) return;
    const s = selectedSignal(), rows = visibleRows(), total = snapshot?.signals.filter(r => r.activity !== 'ignored').length ?? 0;
    renderedRows = rows;
    const mode = s ? chosenMode(s) : 'auto', playing = audition.playing;
    const title = win.root.querySelector('.mc-head strong'); if (title) title.textContent = `${l('Scan / Identify', '扫描 / 识别')} · ${text(snapshot?.state ?? 'stopped')}`;
    const filtered = !!filters.query || filters.type !== 'all' || filters.status !== 'all' || filters.source !== 'all' || !!filters.minPower;
    const setFilter = (key: 'type' | 'status' | 'source' | 'query' | 'minPower', e: Event) => { filters[key] = (e.target as HTMLInputElement).value; filterChanged(); };
    const heading = (key: ScanSort, label: string) => html`<th aria-sort=${filters.sort === key ? filters.descending ? 'descending' : 'ascending' : 'none'}><button data-sort=${key} @click=${() => sortBy(key)}>${label}${filters.sort === key ? filters.descending ? ' ↓' : ' ↑' : ''}</button></th>`;
    render(html`<div class="sdr-scan-toolbar">
      <button data-scan-run class="sdr-scan-primary" ?disabled=${busy || !snapshot} @click=${() => command(snapshot?.running ? 'pause' : 'start')}>${icon(snapshot?.running ? 'stop' : 'play')}${snapshot?.running ? l('Pause', '暂停扫描') : l('Start scan', '开始扫描')}</button>
      <button data-scan-select @click=${() => beginIdentifySelection()}>${icon('crosshair')}${l('Select region', '选区识别')}</button>
      <select data-measurement aria-label=${l('Scan mode', '扫描方式')} .value=${tableSettings ? 'table' : snapshot?.range ? 'range' : measurement} @change=${(e: Event) => { const value = (e.target as HTMLSelectElement).value; if (value === 'table') { tableSettings = !tableSettings; paint(); } else { tableSettings = false; measurement = 'discovery'; void command('configure', { range: value === 'range' && s ? { low_hz: s.low_hz, high_hz: s.high_hz } : null, channels: [] }); } }}><option value="discovery">${l('Find signals', '发现信号')}</option><option value="table">${l('Fixed frequencies…', '固定频点测量…')}</option><option value="range" ?disabled=${!valid(s)}>${l('Selected range', '扫描所选范围')}</option></select>
      <button class="sdr-scan-more" aria-label=${l('More settings', '更多设置')} aria-expanded=${more} @click=${() => { more = !more; paint(); }}>${icon('settings')}</button></div>
      ${tableSettings ? html`<fieldset class="sdr-scan-settings"><legend>${l('Measure fixed frequencies', '固定频点测量')}</legend><p>${l('Check power and activity at known frequencies. This does not create receiver channels.', '按已知频率检查功率和活跃程度，不会创建接收信道。')}</p>
        <div class="sdr-scan-table-inputs">${(['start', 'stop', 'step', 'width'] as const).map((key, i) => html`<label>${[l('Start MHz', '起点 MHz'), l('Stop MHz', '终点 MHz'), l('Spacing kHz', '间隔 kHz'), l('Width kHz', '带宽 kHz')][i]}<input data-table=${key} type="number" step="any" .value=${table[key]} @input=${(e: Event) => { table[key] = (e.target as HTMLInputElement).value; }}></label>`)}</div>
        <button ?disabled=${busy} @click=${() => { const start = Number(table.start) * 1e6, stop = Number(table.stop) * 1e6, step = Number(table.step) * 1e3, width = Number(table.width) * 1e3; if (![start, stop, step, width].every(Number.isFinite) || !(step > 0 && stop >= start && width > 0) || (stop - start) / step >= 256) { error = l('Use positive widths, at most 256 frequencies', '请输入正带宽，最多 256 个频点'); paint(); return; } const channels = []; for (let hz = start; hz <= stop; hz += step) channels.push({ low_hz: hz - width / 2, high_hz: hz + width / 2 }); void measureChannels(channels); }}>${l('Start measurement', '开始测量')}</button>
        <button ?disabled=${busy} @click=${() => measureChannels(session.snapshot.receivers.filter(r => r.display_number !== 1 && !isScanPreview(r)).map(r => ({ low_hz: r.confirmed.baseband.frequency_hz - r.confirmed.baseband.passband_hz / 2, high_hz: r.confirmed.baseband.frequency_hz + r.confirmed.baseband.passband_hz / 2 })))}>${l('Use saved channels', '使用已保存信道')}</button></fieldset>` : ''}
      ${more ? html`<div class="sdr-scan-settings"><label><input type="checkbox" .checked=${snapshot?.auto_identify ?? false} @change=${(e: Event) => command('configure', { auto_identify: (e.target as HTMLInputElement).checked })}>${l('Identify new signals', '自动识别新信号')}</label>
        <label>${l('Threshold', '门限')}<input data-threshold type="number" min="5" max="20" value="8">dB</label><button @click=${() => command('configure', { threshold_db: Number(win!.root.querySelector<HTMLInputElement>('[data-threshold]')!.value) })}>${l('Apply', '应用')}</button>
        <label>${l('Source', '结果来源')}<select .value=${filters.source} @change=${(e: Event) => setFilter('source', e)}><option value="all">${l('All', '全部')}</option><option value="region">${l('Selection', '本次选区')}</option><option value="scan">${l('Discovery', '信号发现')}</option><option value="channel">${l('Fixed frequencies', '固定频点')}</option></select></label><label>${l('Min above floor', '最低门限裕量')}<input data-min-power type="number" placeholder="dB" .value=${filters.minPower} @input=${(e: Event) => setFilter('minPower', e)}></label>
        <button data-clear-inactive ?disabled=${busy} @click=${() => clearResults('inactive')}>${l('Clear inactive', '清除非活跃记录')}</button><button @click=${() => { const blob = new Blob([JSON.stringify(snapshot, null, 2)], { type: 'application/json' }); const url = URL.createObjectURL(blob); const a = document.createElement('a'); a.href = url; a.download = 'signal-scan.json'; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); }}>${l('Export JSON', '导出 JSON')}</button>
        <p>${l('Observed intermittently', '间歇观察')} · ${snapshot?.statistics.observations ?? 0} ${l('observations', '次')} · ${l('Skipped', '跳过')} ${snapshot?.statistics.skipped ?? 0}</p><p>${l('Modulation hints: AM, FM, carrier, FSK/PSK families. Protocol evidence requires valid decoder output.', '调制提示：AM、FM、载波、FSK／PSK 类。具体协议需有效解码证据。')}</p></div>` : ''}
      <div class="sdr-scan-filters"><input type="search" data-scan-search aria-label=${l('Search signals', '搜索信号')} placeholder=${l('Frequency / type…', '频率 / 类型…')} .value=${filters.query} @input=${(e: Event) => setFilter('query', e)}>
        <select data-scan-type aria-label=${l('Signal type', '信号类型')} .value=${filters.type} @change=${(e: Event) => setFilter('type', e)}>${[['all', l('All types', '全部类型')], ['am', 'AM'], ['fm', 'FM'], ['digital', l('Digital hints', '数字类')], ['carrier', l('Carrier', '载波')], ['unknown', l('Unknown', '未知')]].map(([value, label]) => html`<option value=${value}>${label}</option>`)}</select>
        <select data-scan-status aria-label=${l('Result status', '结果状态')} .value=${filters.status} @change=${(e: Event) => setFilter('status', e)}>${[['all', l('All states', '全部状态')], ['active', l('Active', '活跃')], ['complete', l('Analyzed', '已分析')], ['pending', l('Pending', '待识别')], ['stale', l('Inactive / stale', '非活跃／失效')], ['saved', l('Added', '已加入')]].map(([value, label]) => html`<option value=${value}>${label}</option>`)}</select>
        <button data-scan-clear ?disabled=${busy || !total} title=${l('Clear results. Scanning continues; new detections may reappear.', '清除结果；扫描继续时，新命中会重新出现。')} @click=${() => clearResults()}>${icon('trash')}${l('Clear', '清除')}</button></div>
      <div class="sdr-scan-summary"><span>${filtered ? `${rows.length} / ${total}` : total} ${l('results', '个结果')}${filtered ? html` <button data-clear-filters @click=${() => { Object.assign(filters, defaultScanFilters()); filterChanged(); }}>${l('Reset filters', '重置筛选')}</button>` : ''}</span><span title=${snapshot ? `${mhz(snapshot.range?.low_hz ?? snapshot.capture.center_hz - (snapshot.capture.usable_bandwidth_hz ?? 0) / 2)}–${mhz(snapshot.range?.high_hz ?? snapshot.capture.center_hz + (snapshot.capture.usable_bandwidth_hz ?? 0) / 2)} MHz` : ''}>${snapshot?.reason ? text(snapshot.reason) : snapshot?.running ? l('Live observations', '当前采样') : l('Current IQ, including waterfall selections', '选区均使用当前 IQ')}${snapshot?.truncated ? ` · ${snapshot.truncated} ${l('omitted', '项省略')}` : ''}</span></div>
      <div class="sdr-scan-scroll" @pointerenter=${() => { pointerInRows = true; frozenOrder = visibleRows().map(s => s.id); }} @pointerleave=${() => { pointerInRows = false; if (!win!.body.querySelector('.sdr-scan-table')?.contains(win!.root.ownerDocument.activeElement)) frozenOrder = null; }} @focusin=${() => { frozenOrder ??= visibleRows().map(s => s.id); }} @focusout=${(e: FocusEvent) => { if (!(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node)) frozenOrder = null; }}><table class="sdr-scan-table" role="grid" aria-label=${l('Signal results', '信号结果')}><thead><tr>${heading('frequency', 'MHz')}${heading('type', l('Signal / state', '信号 / 状态'))}${heading('width', l('Width', '带宽'))}${heading('power', l('Above floor', '高于本底'))}<th>${l('Actions', '操作')}</th></tr></thead><tbody>${repeat(rows, s => s.id, row)}</tbody></table>
        ${!rows.length ? html`<div class="sdr-scan-empty">${icon('search')}<p>${filtered ? l('No matching results', '没有符合筛选的结果') : snapshot?.reason === 'no_signal_detected' ? text('no_signal_detected') : snapshot?.running ? l('Waiting for stable signals…', '正在等待稳定信号…') : l('Start scanning or select a frequency band.', '开始扫描，或在图上选择一个频段。')}</p>${filtered ? '' : html`<small>${l('Preview to check a signal. Add it to keep receiving.', '先试听确认，加入后保留接收。')}</small>`}</div>` : ''}</div>
      <div class="sdr-scan-listening"><label><input data-scan-exclusive type="checkbox" .checked=${exclusive} @change=${(e: Event) => { exclusive = (e.target as HTMLInputElement).checked; localStorage.setItem('web-sa-scan-exclusive', String(exclusive)); audition.setExclusive(exclusive); paint(); }}>${l('Mute other channels during preview', '试听时静音其他频道')}</label>${playing ? html`<span>${mhz(playing.frequency)} · ${playing.mode.toUpperCase()}</span><button data-scan-stop @click=${() => stopAudition()}>${icon('stop')}${l('Stop preview', '停止试听')}</button>` : ''}</div>
      ${s ? html`<div class="sdr-scan-footer"><span data-selected-frequency>${mhz(s.center_hz)} MHz</span><select aria-label=${l('Preview / add mode', '试听／加入模式')} title=${l('How to preview or receive this row; does not change the identification', '只决定此行的试听和接收方式，不改变识别结论')} .value=${mode} ?disabled=${busy} @change=${(e: Event) => { modes.set(s.id, (e.target as HTMLSelectElement).value); if (audition.playing?.signal === s.id) { if (canListen(s)) void receive(s, true); else void stopAudition(); } paint(); }}><option value="auto">${l('Auto', '自动')} · ${s.suggested_mode?.toUpperCase() ?? 'IQ'}</option><option value="iq">${l('IQ only (record / analyze)', '仅 IQ（录制／分析）')}</option>${['am', 'nfm', 'wfm', 'cw', 'usb', 'lsb'].filter(id => session.snapshot.capabilities.mode_descriptors?.some(d => d.id === id && d.available)).map(id => html`<option value=${id}>${id.toUpperCase()}</option>`)}</select>
        <button data-scan-identify ?disabled=${!valid(s) || busy} @click=${() => command('identify', { signal_id: s.id, signal_generation: s.generation })}>${l('Identify', '重新识别')}</button><button data-scan-evidence aria-expanded=${details} @click=${() => { details = !details; if (details) void readEvidence(s); paint(); }}>${details ? l('Less', '收起依据') : l('Evidence', '识别依据')}</button></div>
        ${mode === 'iq' || mode === 'auto' && !s.suggested_mode ? html`<small class="sdr-scan-mode-hint">${l('IQ has no audio. Add it for recording and analysis, or choose an audio mode to preview.', 'IQ 不播放音频；加入后可录制和分析，试听请选音频模式。')}</small>` : ''}
        ${details ? html`<div class="sdr-scan-evidence"><p>${text(s.label)} · ${text(s.verdict)} · ${l('Protocol', '协议')}：${s.protocol ?? l('No validated evidence', '尚无有效证据')}${s.protocol ? ` · ${l('Decoder', '解码器')}：${s.decoder_available ? l('Available', '可用') : l('Unavailable', '不可用')}` : ''}</p><p>${describeEvidence(evidence.get(s.id) ?? s.evidence ?? {})}</p><small>${s.sampled_at_ms ? new Date(Number(s.sampled_at_ms)).toLocaleString() : ''} · ${l('Current IQ', '当前 IQ')}${s.flags.includes('capture_edge') ? l(' · Capture edge', ' · 采集边缘') : ''}</small></div>` : ''}` : ''}
      ${error || connectionError || notice ? html`<p class="sdr-scan-error" role=${error || connectionError ? 'alert' : 'status'}>${error || connectionError || notice}</p>` : ''}`, win.body);
  }
  const stop = session.subscribe(() => {
    if (snapshot && (snapshot.service_id !== session.snapshot.service_id || snapshot.capture.epoch !== session.snapshot.capture.epoch || currentGraphMode() !== 'sdr')) {
      void audition.stop().catch(reason => { error = String(reason); });
      version++; regionVersion++; setIdentifySource(JSON.stringify(session.snapshot.capture), session.snapshot.capture.epoch, 1000, []);
      snapshot = null; evidence.clear(); selected = ''; summary = null; updateToggle(); if (visible) void refresh();
    }
    paint();
  });
  const selection = () => win?.setTemporaryCollapsed(identifySelecting());
  document.addEventListener('websa:identify-selection', selection);
  const stopLang = onLangChange(() => { win?.sync(); paint(); });
  document.addEventListener('visibilitychange', () => { if (!document.hidden) void refresh(); }, { signal: lifetime.signal });
  const timer = setInterval(() => { updates.poll(Boolean(connectionError)); }, 500);
  window.addEventListener('pagehide', () => { audition.unload(); disposed = true; version++; lifetime.abort(); updates.close(); clearInterval(timer); stop(); stopLang(); document.removeEventListener('websa:identify-selection', selection); win?.dispose(); }, { once: true });
  return { visible: () => visible, toggle: () => visible ? win!.hide() : win!.show(), show: () => win!.show(),
    async identifyRegion(request: IdentifyRequest): Promise<void> { const current = ++regionVersion; Object.assign(filters, defaultScanFilters(), { sort: filters.sort, descending: filters.descending }); frozenOrder = null; await audition.stop(); if (!snapshot) await refresh(); await commandPending; if (current !== regionVersion || disposed) return; focusResults = false; await command('identify_region', { ...request, capture_epoch: request.capture_epoch || snapshot?.capture.epoch }); if (current === regionVersion && !error) focusResults = true; },
  };
}
