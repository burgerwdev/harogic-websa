import './sdrScan.css';
import { html, render } from 'lit-html';
import { repeat } from 'lit-html/directives/repeat.js';
import { getLang, onLangChange } from '../core/i18n';
import { CHANNEL_COLORS } from '../core/channelColors';
import { getUiScale } from '../core/uiScale';
import { defaultTransitionHz, occupiedBandwidth } from '../core/receiverBandwidth';
import type { ReceiverConfig, ReceiverState } from '../core/multichannel';
import { createFloatingWindow, type FloatingWindow } from './floatingWindow';

import type { WorkbenchSession } from './workbenchSession';
import { beginIdentifySelection, highlightIdentifyBand, identifySelecting, initIdentifySelection, setIdentifySource, type IdentifyRequest } from './sdrIdentifySelection';
import { currentGraphMode } from './graphMode';

export interface ScanSignal { service_id?: string; current_capture_epoch?: string; id: string; generation: string; center_hz: number; bandwidth_hz: number; low_hz: number; high_hz: number;
  excess_db: number; activity: string; label: string; verdict: string; identify_state: string; suggested_mode: string | null;
  protocol: string | null; receiver_available: boolean; decoder_available: boolean; receiver_uid: string | null; source: string; sampled_at_ms: string | null; evidence?: Record<string, unknown>; flags: string[]; observed_hit_ratio: number }
export interface ScanSnapshot { service_id: string; scan_id: string; revision: string; capture: { epoch: string; center_hz: number; usable_bandwidth_hz: number | null; input_rate_hz: number | null; state: string };
  running: boolean; state: string; range: {low_hz:number;high_hz:number}|null; signals: ScanSignal[]; reason: string | null; probe_slots_used: number; truncated: number; auto_identify: boolean;
  bin_spacing_hz: number | null; background_reliable: boolean; statistics: { observations: number; skipped: number; observed_seconds: number; rate_hz: number; analysis_cpu_seconds: number; feature_cpu_seconds: number; elapsed_seconds: number | null } }
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
  button.textContent = l('Identify', '识别'); button.setAttribute('aria-pressed', String(visible)); button.setAttribute('aria-expanded', String(visible));
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
  let snapshot: ScanSnapshot | null = null, selected = '', details = false, more = false, error = '', busy = false, mode = 'auto';
  let win: FloatingWindow | undefined, visible = false, polling = false, version = 0, disposed = false, focusResults = false;
  let refreshPending: Promise<void> = Promise.resolve(), commandPending: Promise<void> = Promise.resolve(), regionVersion = 0;
  const lifetime = new AbortController();
  const button = document.querySelector<HTMLButtonElement>('#sdr-scan-toggle');
  const evidence = new Map<string, Record<string, unknown>>();
  async function readEvidence(s: ScanSignal): Promise<void> { const service = snapshot?.service_id, epoch = snapshot?.capture.epoch; try { const r = await fetch(`/api/sdr/scan/signals/${s.id}`, { signal: lifetime.signal }); if (!r.ok) return; const full = await r.json() as ScanSignal; if (full.service_id === service && full.current_capture_epoch === epoch && service === snapshot?.service_id && epoch === snapshot?.capture.epoch && full.generation === s.generation) { evidence.set(s.id, full.evidence ?? {}); paint(); } } catch { /* The next explicit request can retry. */ } }
  const selectedSignal = () => snapshot?.signals.find(s => s.id === selected);
  const valid = (s?: ScanSignal) => !!s && s.activity !== 'stale' && snapshot?.capture.epoch === session.snapshot.capture.epoch && snapshot.service_id === session.snapshot.service_id;
  const statusChanged = () => {
    if (!win) return;
    visible = !win.root.hidden;
    if (!visible) { version++; button?.focus({ preventScroll: true }); }
    updateToggle(); if (visible) void refresh();
  };
  const closeDetails = (event: KeyboardEvent) => {
    if (event.key === 'Escape' && details) { details = false; event.preventDefault(); event.stopPropagation(); paint(); return true; } return false;
  };
  const scale = getUiScale();
  const plotRight = document.querySelector('.spectrum-area')?.getBoundingClientRect().right ?? innerWidth;
  win = createFloatingWindow('sdr-scan', () => l('Scan / Identify', '扫描 / 识别'),
    { x: Math.max(16, plotRight - 700 * scale), y: Math.max(20, innerHeight - 470 * scale), w: 680 * scale, h: 420 * scale }, statusChanged,
    { minWidth: 520, minHeight: 300, closeTip: () => l('Hide window; scanning continues', '隐藏窗口，扫描继续'), escape: closeDetails });
  win.root.id = 'sdr-scan-window'; win.root.classList.add('sdr-scan-window'); win.hide();
  function accept(value: ScanSnapshot): void {
    if (value.service_id !== session.snapshot.service_id) return;
    if (snapshot?.service_id === value.service_id && BigInt(value.revision) < BigInt(snapshot.revision)) return;
    for (const id of evidence.keys()) if (!value.signals.some(s => s.id === id)) evidence.delete(id);
    snapshot = value; summary = { running: value.running, state: value.state, service_id: value.service_id }; updateToggle();
    const capture = value.capture;
    setIdentifySource(JSON.stringify([value.service_id, capture.epoch, capture.center_hz, capture.usable_bandwidth_hz, capture.state]), capture.epoch, value.bin_spacing_hz ?? 1000, value.signals);
    if (!value.signals.some(s => s.id === selected)) selected = value.signals.find(s => s.source === 'region')?.id ?? '';
    const row = selectedSignal(); highlightIdentifyBand(valid(row) ? row! : null); paint();
    if (focusResults && value.signals.some(s => s.source === 'region' && s.identify_state !== 'detecting')) {
      selected = value.signals.find(s => s.source === 'region')!.id; focusResults = false; paint();
      win?.root.querySelector<HTMLElement>(`[data-signal="${selected}"]`)?.focus({ preventScroll: true });
    }
  }
  async function refresh(): Promise<void> {
    if (polling) return refreshPending;
    if (!visible || disposed || document.hidden && !win?.portal.isPoppedOut()) return;
    let finish = () => {}; refreshPending = new Promise<void>(resolve => { finish = resolve; });
    polling = true; const generation = version;
    try {
      const response = await fetch('/api/sdr/scan', { signal: AbortSignal.any([lifetime.signal, AbortSignal.timeout(5000)]) });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const value = await response.json() as ScanSnapshot;
      if (generation === version && visible) accept(value);
    } catch (reason) { if (!disposed) { error = String(reason); summary = null; updateToggle(); paint(); } }
    finally { polling = false; finish(); }
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
    finally { busy = false; finishCommand(); paint(); }
  }
  function configFor(s: ScanSignal): ReceiverConfig {
    const chosen = mode === 'auto' ? s.suggested_mode ?? 'iq' : mode;
    const descriptor = session.snapshot.capabilities.mode_descriptors?.find(d => d.id === chosen);
    const bandwidth = Math.max(chosen === 'wfm' ? 180000 : chosen === 'cw' ? 1000 : chosen === 'am' ? 12000 : 2500, s.bandwidth_hz);
    const passband = descriptor ? occupiedBandwidth(descriptor, bandwidth, 700) : bandwidth, transition = defaultTransitionHz(passband);
    return { name: `${mhz(s.center_hz)} MHz · ${chosen.toUpperCase()}`, color: CHANNEL_COLORS[session.snapshot.receivers.length % CHANNEL_COLORS.length], tags: [], enabled: true,
      purpose: chosen === 'iq' ? 'iq' : descriptor?.kind === 'digital' ? 'digital' : 'listen', baseband: { frequency_hz: s.center_hz, passband_hz: passband, transition_hz: transition,
        minimum_rate_hz: Math.max(descriptor?.minimumRateHz ?? 0, passband + 2 * transition) },
      processors: chosen === 'iq' ? [] : [{ id: `scan-${crypto.randomUUID()}`, kind: descriptor?.kind === 'digital' ? 'decoder' : 'demodulator', mode: chosen, bandwidth_hz: bandwidth,
        minimum_rate_hz: descriptor?.minimumRateHz ?? 0, pcm_gain: 0.7, agc: true, squelch_dbm: -120, nr: false, nr_algorithm: 'wiener', nr_strength: 0.5, nr_atten_db: 20 }] };
  }
  function duplicate(s: ScanSignal): ReceiverState | undefined {
    const c = configFor(s);
    return session.snapshot.receivers.find(r => Math.abs(r.confirmed.baseband.frequency_hz - c.baseband.frequency_hz) < .5
      && r.confirmed.processors[0]?.mode === c.processors[0]?.mode
      && (['passband_hz', 'transition_hz', 'minimum_rate_hz'] as const).every(k => Math.abs(r.confirmed.baseband[k] - c.baseband[k]) < .5));
  }
  async function receive(listen: boolean): Promise<void> {
    const s = selectedSignal(); if (!valid(s) || busy) return;
    let finishReception = () => {}; commandPending = new Promise<void>(resolve => { finishReception = resolve; });
    busy = true; error = ''; paint(); const service = session.snapshot.service_id, epoch = snapshot!.capture.epoch;
    try {
      const freshResponse = await fetch(`/api/sdr/scan/signals/${s!.id}`, { signal: lifetime.signal });
      if (!freshResponse.ok) throw new Error(l('Result expired', '结果已失效'));
      const fresh = await freshResponse.json() as ScanSignal;
      if (fresh.service_id !== service || fresh.current_capture_epoch !== epoch || fresh.generation !== s!.generation || fresh.activity === 'stale' || service !== session.snapshot.service_id || epoch !== session.snapshot.capture.epoch) throw new Error(l('Result or capture changed; refresh and retry', '结果或采集状态已变化，请刷新后重试'));
      let receiver = duplicate(s!);
      if (!receiver) {
        const reply = await session.create(configFor(fresh), { capture_epoch: epoch, signal_id: fresh.id, signal_generation: fresh.generation });
        if (reply.fault) throw new Error(reply.fault.message);
        receiver = session.snapshot.receivers.find(r => r.receiver_uid === reply.receiver_uid);
      }
      if (!receiver || service !== session.snapshot.service_id) return;
      if (listen) {
        if (!receiver.confirmed.enabled) {
          const reply = await session.send({ type: 'configure', request_id: session.requestId(), receiver_uid: receiver.receiver_uid, expected_revision: receiver.revision,
            config: { ...receiver.confirmed, enabled: true } }); if (reply.fault) throw new Error(reply.fault.message);
        }
        session.masterMuted = false; session.monitor(receiver.receiver_uid, { enabled: true });
      } else { session.select(receiver.receiver_uid); await (await import('./nativeWorkbench')).openNativeChannelOverview(); }
      s!.receiver_uid = receiver.receiver_uid;
    } catch (reason) { error = String(reason instanceof Error ? reason.message : reason); }
    finally { busy = false; finishReception(); paint(); }
  }
  function row(s: ScanSignal) {
    const state = s.protocol ? `${s.protocol} · ${text('confirmed')}` : s.identify_state === 'complete' ? s.label === 'unknown' ? l('Unknown · insufficient evidence', '未知 · 证据不足') : `${text(s.label)} · ${text(s.verdict)}` : `${text(s.label)} · ${text(s.identify_state)}`;
    return html`<tr role="row" data-signal=${s.id} data-stale=${s.activity === 'stale'} aria-selected=${s.id === selected} tabindex=${s.id === selected || !selected && s.id === snapshot?.signals[0]?.id ? 0 : -1}
      @mouseenter=${() => highlightIdentifyBand(s)} @mouseleave=${() => highlightIdentifyBand(selectedSignal() ?? null)}
      @click=${() => { selected = s.id; details = false; highlightIdentifyBand(s); paint(); }}
      @keydown=${(e: KeyboardEvent) => {
        if (e.key === 'Enter') { e.preventDefault(); selected = s.id; highlightIdentifyBand(s); details = !details; if (details) void readEvidence(s); paint(); }
        if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); const rows = snapshot!.signals.filter(r => r.activity !== 'ignored').sort((a,b) => Number(b.source === 'region') - Number(a.source === 'region') || a.center_hz - b.center_hz); const i = rows.indexOf(s); selected = rows[Math.max(0, Math.min(rows.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))].id; highlightIdentifyBand(selectedSignal() ?? null); paint(); win?.root.querySelector<HTMLElement>(`[data-signal="${selected}"]`)?.focus(); }
      }}>
      <td>${mhz(s.center_hz)}</td><td title=${s.activity === 'not_detected' || s.source === 'channel' || s.flags.includes('requested_window') ? l('Requested measurement window, not measured occupancy', '待测窗口，非已测占用带宽') : ''}>${s.activity === 'not_detected' || s.source === 'channel' || s.flags.includes('requested_window') ? l('Window ' , '窗口 ') : ''}${(s.bandwidth_hz / 1000).toFixed(1)} kHz</td><td title=${`${state} · ${text(s.activity)}`}>${state}${s.activity !== 'active' ? ` · ${text(s.activity)}` : ''}${s.receiver_uid ? ` C${session.snapshot.receivers.find(r => r.receiver_uid === s.receiver_uid)?.display_number ?? ''}` : ''}</td>
      <td title=${`${l('Observed hit ratio', '观测命中率')} ${(s.observed_hit_ratio * 100).toFixed(0)}%`}>+${s.excess_db.toFixed(1)} dB</td></tr>`;
  }
  function paint(): void {
    if (!win || !visible) return;
    const s = selectedSignal(), saved = s ? duplicate(s) : undefined, capable = valid(s);
    const chosen = s ? mode === 'auto' ? s.suggested_mode : mode : null;
    const canListen = !!chosen && chosen !== 'iq' && session.snapshot.capabilities.mode_descriptors?.some(d => d.id === chosen && d.available && d.kind === 'analog');
    const title = win.root.querySelector('.mc-head strong'); if (title) title.textContent = `${l('Scan / Identify', '扫描 / 识别')} · ${text(snapshot?.state ?? 'stopped')}`;
    const signals = snapshot?.signals.filter(s => s.activity !== 'ignored') ?? [];
    const groups = ['region', 'scan'];
    render(html`<div class="sdr-scan-toolbar"><select aria-label=${l('Scan range', '扫描范围')} @change=${(e: Event) => {
      const value = (e.target as HTMLSelectElement).value;
      if (value === 'capture') void command('configure', { range: null, channels: [] });
      if (value === 'saved') { const channels = session.snapshot.receivers.filter(r => r.display_number !== 1).map(r => ({ low_hz: r.confirmed.baseband.frequency_hz - r.confirmed.baseband.passband_hz / 2, high_hz: r.confirmed.baseband.frequency_hz + r.confirmed.baseband.passband_hz / 2 })); if (!channels.length) { error = l('No saved channels yet', '尚无已保存信道'); paint(); } else void command('configure', { range: null, channels }); }
      if (value === 'custom') { more = true; paint(); }
      if (value === 'selected' && s) void command('configure', { range: { low_hz: s.low_hz, high_hz: s.high_hz }, channels: [] }).then(() => { if (!error && !snapshot?.running) return command('start'); });
    }}><option value="capture">${l('Capture bandwidth', '采集带宽')}</option><option value="saved">${l('Saved channels', '已保存信道')}</option><option value="custom">${l('Custom channel spacing…', '自定义频道间隔…')}</option><option value="selected" ?disabled=${!s}>${l('Scan selected range', '扫描此范围')}</option></select>
      <button data-scan-select @click=${() => beginIdentifySelection()}>${l('Select region', '选区识别')}</button>
      <button data-scan-run ?disabled=${busy || !snapshot} @click=${() => command(snapshot?.running ? 'pause' : 'start')}>${snapshot?.running ? l('Pause', '暂停扫描') : l('Start scan', '开始扫描')}</button>
      <button aria-label=${l('More settings', '更多设置')} aria-expanded=${more} @click=${() => { more = !more; paint(); }}>···</button></div>
      ${more ? html`<div class="sdr-scan-settings"><label><input type="checkbox" .checked=${snapshot?.auto_identify ?? false} @change=${(e: Event) => command('configure', { auto_identify: (e.target as HTMLInputElement).checked })}>${l('Identify new signals', '识别新信号')}</label>
        <label>${l('Threshold', '门限')}<input data-threshold type="number" min="5" max="20" value="8">dB</label><button @click=${() => command('configure', { threshold_db: Number(win!.root.querySelector<HTMLInputElement>('[data-threshold]')!.value) })}>${l('Apply', '应用')}</button>
        <div>${(['start', 'stop', 'step', 'width'] as const).map((key, i) => html`<label>${[l('Start MHz', '起点 MHz'), l('Stop MHz', '终点 MHz'), l('Spacing kHz', '间隔 kHz'), l('Width kHz', '带宽 kHz')][i]}<input data-table=${key} type="number" step="any" value=${i < 2 ? String((session.snapshot.capture.center_hz + (i ? 100000 : -100000)) / 1e6) : '12.5'}></label>`)}
          <button @click=${() => { const v = (key: string) => Number(win!.root.querySelector<HTMLInputElement>(`[data-table=${key}]`)!.value); const channels = []; const start = v('start') * 1e6, stop = v('stop') * 1e6, step = v('step') * 1e3, width = v('width') * 1e3;
            if (!(step > 0 && stop >= start && width > 0) || (stop - start) / step >= 256) { error = l('Use positive widths and at most 256 channels', '请输入正带宽，最多 256 个频道'); paint(); return; }
            for (let hz = start; hz <= stop; hz += step) channels.push({ low_hz: hz - width / 2, high_hz: hz + width / 2 }); void command('configure', { range: null, channels }); }}>${l('Measure table', '测量频道表')}</button></div>
        <p>${l('Intermittent observation', '间歇观察')} · ${snapshot?.statistics.observations ?? 0} ${l('observations', '次')} · ${snapshot?.statistics.observed_seconds.toFixed(3) ?? 0} s · ${l('Skipped', '跳过')} ${snapshot?.statistics.skipped ?? 0} · ${snapshot?.statistics.rate_hz ?? 5} Hz</p>
        <button @click=${() => command('stop')}>${l('Stop', '停止')}</button><button @click=${() => { const blob = new Blob([JSON.stringify(snapshot, null, 2)], { type: 'application/json' }); const url = URL.createObjectURL(blob); const a = document.createElement('a'); a.href = url; a.download = 'signal-scan.json'; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); }}>${l('Export JSON', '导出 JSON')}</button></div>` : ''}
      <div class="sdr-scan-summary">${snapshot ? `${mhz(snapshot.range?.low_hz ?? snapshot.capture.center_hz - (snapshot.capture.usable_bandwidth_hz ?? 0) / 2)}–${mhz(snapshot.range?.high_hz ?? snapshot.capture.center_hz + (snapshot.capture.usable_bandwidth_hz ?? 0) / 2)} MHz · ${signals.length} ${l('results', '个结果')}` : l('Reading capture…', '读取采集状态…')}
      ${snapshot?.reason ? text(snapshot.reason) : ''}${snapshot && snapshot.statistics.observations > 0 && !snapshot.background_reliable ? l(' · Background uncertain', ' · 背景不可靠') : ''}${snapshot?.truncated ? ` · ${snapshot.truncated} ${l('omitted; narrow the range', '项省略，请缩小范围')}` : ''}</div>
      <div class="sdr-scan-scroll"><table class="sdr-scan-table" role="grid" aria-label=${l('Signal results', '信号结果')}><thead><tr><th>MHz</th><th>${l('Width', '带宽')}</th><th>${l('Type / state', '类型与状态')}</th><th>${l('Above floor', '高于本底')}</th></tr></thead><tbody>
        ${groups.map(group => { const rows = signals.filter(s => group === 'region' ? s.source === 'region' : s.source !== 'region').sort((a, b) => a.center_hz - b.center_hz); return rows.length ? html`<tr class="sdr-scan-group"><td colspan="4">${group === 'region' ? l('This selection · current IQ', '本次选区 · 当前采样') : l('Continuous scan', '连续扫描')}</td></tr>${repeat(rows, s => s.id, row)}` : ''; })}
      </tbody></table>${!signals.length ? html`<p>${snapshot?.running ? l('No signal above threshold in current observations', '当前观察未发现高于门限的信号') : l('Scan the capture bandwidth, or identify a region independently.', '扫描当前采集范围，或独立选区识别。')}</p>` : ''}</div>
      ${s ? html`<div class="sdr-scan-evidence"><button aria-expanded=${details} @click=${() => { details = !details; if (details) void readEvidence(s); paint(); }}>${details ? l('Hide evidence', '收起依据') : l('Show evidence', '展开依据')}</button> ${text(s.activity)} · ${l('Current samples', '当前采样')}${s.flags.includes('capture_edge') ? l(' · Capture edge; width may be truncated', ' · 采集边缘，带宽可能截断') : ''}${s.flags.includes('possible_dc') ? l(' · Possible centre component', ' · 可能为采集中心分量') : ''}
        ${details ? html`<p>${l('Modulation', '调制')}：${text(s.label)} · ${text(s.verdict)} · ${l('Protocol', '协议')}：${s.protocol ?? l('No validated protocol evidence', '尚无有效协议证据')}</p><p>${l('Receive mode', '接收模式')}：${s.suggested_mode?.toUpperCase() ?? l('IQ / manual', 'IQ / 手动')} · ${s.receiver_available || !s.suggested_mode ? l('Available', '可用') : l('Unavailable', '不可用')} · ${l('Protocol decoder', '协议解码器')}：${s.protocol ? s.decoder_available ? l('Available', '可用') : l('Unavailable', '不可用') : l('No confirmed protocol', '尚无已确认协议')}</p><p>${describeEvidence(evidence.get(s.id) ?? s.evidence ?? {})}</p><p>${s.sampled_at_ms ? new Date(Number(s.sampled_at_ms)).toLocaleString() : ''} · ${l('Short shape analysis does not exclude slow protocols.', '短时形态分析不能排除长周期协议。')}</p>` : ''}</div>` : ''}
      <p class="sdr-scan-error" role="alert">${error}</p>
      <div class="sdr-scan-footer"><span data-selected-frequency>${s ? `${mhz(s.center_hz)} MHz` : l('Select a result', '请选择结果')}</span>
        <button ?disabled=${!capable || busy} @click=${() => s && command('identify', { signal_id: s.id, signal_generation: s.generation })}>${l('Identify', '识别')}</button>
        <select aria-label=${l('Reception mode', '接收模式')} .value=${mode} @change=${(e: Event) => { mode = (e.target as HTMLSelectElement).value; paint(); }}><option value="auto">${l('Suggested / IQ', '建议 / IQ')}</option><option value="iq">IQ</option>${['am', 'nfm', 'wfm', 'cw', 'usb', 'lsb'].filter(id => session.snapshot.capabilities.mode_descriptors?.some(d => d.id === id && d.available)).map(id => html`<option value=${id}>${id.toUpperCase()}</option>`)}</select>
        <button ?disabled=${!capable || !canListen || busy} title=${!canListen ? l('Choose an audio mode to listen', '请先选择可监听的模式') : ''} @click=${() => receive(true)}>${l('Listen', '监听')}</button>
        <button title=${!saved && session.snapshot.receivers.length >= session.snapshot.capabilities.max_channels ? l('Receiver capacity reached', '正式信道容量已满') : ''} ?disabled=${!capable || busy || !saved && session.snapshot.receivers.length >= session.snapshot.capabilities.max_channels} @click=${() => receive(false)}>${saved ? l(`Open C${saved.display_number}`, `打开 C${saved.display_number}`) : l('Add channel', '加入信道')}</button></div>`, win.body);
  }
  const stop = session.subscribe(() => {
    if (snapshot && (snapshot.service_id !== session.snapshot.service_id || snapshot.capture.epoch !== session.snapshot.capture.epoch || currentGraphMode() !== 'sdr')) {
      version++; regionVersion++; setIdentifySource(JSON.stringify(session.snapshot.capture), session.snapshot.capture.epoch, 1000, []);
      snapshot = null; evidence.clear(); selected = ''; summary = null; updateToggle(); if (visible) void refresh();
    }
    paint();
  });
  const selection = () => win?.setTemporaryCollapsed(identifySelecting());
  document.addEventListener('websa:identify-selection', selection);
  const stopLang = onLangChange(() => { win?.sync(); paint(); });
  const timer = setInterval(() => { void refresh(); }, 500);
  window.addEventListener('pagehide', () => { disposed = true; version++; lifetime.abort(); clearInterval(timer); stop(); stopLang(); document.removeEventListener('websa:identify-selection', selection); win?.dispose(); }, { once: true });
  return { visible: () => visible, toggle: () => visible ? win!.hide() : win!.show(), show: () => win!.show(),
    async identifyRegion(request: IdentifyRequest): Promise<void> { const current = ++regionVersion; if (!snapshot) await refresh(); await commandPending; if (current !== regionVersion || disposed) return; focusResults = true; await command('identify_region', { ...request, capture_epoch: request.capture_epoch || snapshot?.capture.epoch }); },
  };
}
