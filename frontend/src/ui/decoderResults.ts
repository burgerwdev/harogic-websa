import { t, getLang } from '../core/i18n';
import { clock } from '../sdr/ft8Log';
import { decoderLog, decoderRasterChanges, type DecoderRaster, type DecoderImage, type DecoderLogEntry } from '../sdr/decoderLog';
import { drawAudioScope } from './audioScope';
import { copyDetails, copyDetailsButton } from './copyDetails';
import { patchWorkbench } from './workbenchDom';

const rendered = new WeakMap<HTMLElement, { identity: string; presentation: string; nodes: Map<number, { entry: DecoderLogEntry; node: HTMLElement }> }>();
function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function readable(value: unknown): string {
  if (value === null || value === undefined) return '—';
  if (Array.isArray(value)) return value.map(readable).join(', ');
  if (record(value)) return Object.entries(value).map(([key, item]) => `${key}: ${readable(item)}`).join(' · ');
  return String(value);
}
const provenanceFields: Record<string, true> = { sourceFrequencyHz: true, onairFrequencyHz: true, frequencyOffsetHz: true, onairDialHz: true, onairRfHz: true, upstreamCompletionKnown: true, bench: true, center_hz: true, nominalCarrierHz: true, input_samples: true, input_rate_hz: true, gnss_version: true };
const summaryFields: Record<string, true> = { state: true, message: true, why: true, dial_hz: true, frequency_hz: true, frequencyHz: true, snr_db: true };
const fieldUnits: Record<string, string> = { hz: 'Hz', db: 'dB', dbfs: 'dBFS', dbm: 'dBm', snr: 'SNR', mmsi: 'MMSI', ioc: 'IOC', icao: 'ICAO', prn: 'PRN', pvt: 'PVT' };
function fieldLabel(key: string): string {
  if (key === 'cn0_db_hz') return 'C/N0 (dB-Hz)';
  return key.replace(/([a-z])([A-Z])/g, '$1 $2').replace(/_/g, ' ')
    .replace(/\b(hz|db|dbfs|dbm|snr|mmsi|ioc|icao|prn|pvt)\b/gi, unit => fieldUnits[unit.toLowerCase()])
    .replace(/\bC$/, '°C').replace(/\bF$/, '°F');
}
function fields(host: HTMLElement, data: Record<string, unknown>, status = false): void {
  const doc = host.ownerDocument;
  const list = doc.createElement('dl');
  list.className = status ? 'decoder-fields decoder-metrics' : 'decoder-fields';
  const hasRows = Object.values(data).some(Array.isArray);
  for (const [key, value] of Object.entries(data)) {
    if (['v', 'kind', 'type', 'png'].includes(key) || Object.prototype.hasOwnProperty.call(provenanceFields, key) || /^virtual/i.test(key) || Array.isArray(value)) continue;
    if (hasRows && !Object.prototype.hasOwnProperty.call(summaryFields, key)) continue;
    if (status && !['state', 'message', 'why', 'reason', 'lines', 'rows', 'snr_db'].includes(key)) continue;
    const term = doc.createElement('dt');
    term.textContent = fieldLabel(key);
    const description = doc.createElement('dd');
    description.textContent = readable(value);
    if (status) {
      const cell = doc.createElement('div'); cell.className = `decoder-metric${['message', 'why', 'reason'].includes(key) ? ' decoder-metric-wide' : ''}`;
      cell.append(term, description); list.append(cell);
    } else list.append(term, description);
  }
  if (list.childElementCount) host.appendChild(list);
}
const preferredColumns: Record<string, readonly string[]> = {
  vessels: ['name', 'mmsi', 'lat', 'lon', 'sog_kt', 'cog_deg', 'callsign', 'destination'],
  stations: ['call', 'lat', 'lon', 'comment', 'last_heard_s'],
  frames: ['from', 'to', 't', 'summary', 'path', 'baud', 'lat', 'lon'],
  decodes: ['slot_utc', 'snr_db', 'dt_s', 'audio_hz', 'rf_hz', 'msg', 'call', 'grid', 'dbm'],
  aircraft: ['icao', 'callsign', 'alt_ft', 'lat', 'lon', 'speed_kt', 'track_deg', 'vrate_fpm', 'squawk'],
  satellites: ['prn', 'channel', 'cn0_db_hz', 'doppler_hz', 'acquisition', 'tracking', 'nav_word_valid', 'tow_ms', 'pseudorange_m', 'pseudorange_valid', 'stale', 'age_ms'],
  pvt: ['valid', 'lat_deg', 'lon_deg', 'height_m', 'solution_status', 'valid_sats', 'utc_time', 'stale', 'age_ms'],
  navigation: ['prn', 'tow_ms', 'nav_message', 'system', 'signal'],
};
const tableViews = new WeakMap<HTMLElement, (rows: unknown[], utc: boolean) => void>();
function table(host: HTMLElement, rows: unknown[], label: string, utc: boolean): void {
  const existing = tableViews.get(host);
  if (existing) { existing(rows, utc); return; }
  const doc = host.ownerDocument;
  const heading = doc.createElement('strong');
  heading.textContent = ['aircraft', 'satellites', 'pvt', 'navigation'].includes(label) ? t(`decoder_table_${label}`) : label;
  host.appendChild(heading);
  let offset = 0;
  const wrapper = doc.createElement('div');
  wrapper.className = 'decoder-table-scroll';
  const grid = doc.createElement('table');
  const navigation = doc.createElement('div');
  const previous = doc.createElement('button');
  const next = doc.createElement('button');
  const page = doc.createElement('span');
  previous.className = next.className = 'btn';
  previous.textContent = '‹';
  next.textContent = '›';
  previous.title = t('decoder_previous');
  next.title = t('decoder_next');
  const draw = (): void => {
    const fresh = doc.createElement('table');
    if (!rows.length) { patchWorkbench(grid, ''); navigation.hidden = true; return; }
    offset = Math.min(offset, Math.max(0, Math.ceil(rows.length / 25) - 1) * 25);
    const visible = rows.slice(offset, offset + 25);
    const columns = [...new Set(visible.flatMap(row => record(row) ? Object.keys(row).filter(key => !Object.prototype.hasOwnProperty.call(provenanceFields, key) && !/^virtual/i.test(key)) : ['value']))];
    const preferred = preferredColumns[label] ?? [];
    columns.sort((a, b) => {
      const left = preferred.indexOf(a), right = preferred.indexOf(b);
      return (left < 0 ? preferred.length : left) - (right < 0 ? preferred.length : right);
    });
    // Unrelated sensor objects may have disjoint fields; keep each record readable
    // without multiplying thousands of empty cells across the page.
    if (columns.length > 32) {
      const body = fresh.createTBody();
      for (const value of visible) body.insertRow().insertCell().textContent = readable(value);
    } else {
      const header = fresh.createTHead().insertRow();
      for (const key of columns) {
        const th = doc.createElement('th');
        th.textContent = key === 'slot_utc' ? `${t('ft8_time')} (${utc ? 'UTC' : 'LT'})` : fieldLabel(key);
        header.appendChild(th);
      }
      const body = fresh.createTBody();
      for (const value of visible) {
        const row = body.insertRow();
        for (const key of columns) {
          const cell = row.insertCell();
          const field = record(value) ? value[key] : value;
          cell.textContent = key === 'slot_utc' && typeof field === 'number' && Number.isFinite(field)
            ? clock(field * 1000, utc) : readable(field);
          if (label === 'pvt' && record(value)) {
            const valid = value.valid === true && value.stale !== true;
            if (key === 'valid') cell.textContent = t(valid ? 'decoder_position_valid' : 'decoder_position_invalid');
            if (!valid && ['lat_deg', 'lon_deg', 'height_m'].includes(key)) cell.textContent = '—';
          }
          if (['msg', 'message', 'summary', 'comment', 'text', 'nav_message'].includes(key)) cell.className = 'decoder-message-cell';
        }
      }
    }
    patchWorkbench(grid, fresh.innerHTML);
    navigation.hidden = rows.length <= 25;
    previous.title = t('decoder_previous'); next.title = t('decoder_next');
    heading.textContent = ['aircraft', 'satellites', 'pvt', 'navigation'].includes(label) ? t(`decoder_table_${label}`) : label;
    page.textContent = ` ${rows.length ? offset + 1 : 0}–${Math.min(offset + 25, rows.length)} / ${rows.length} `;
    previous.disabled = offset === 0;
    next.disabled = offset + 25 >= rows.length;
  };
  previous.addEventListener('click', () => { offset = Math.max(0, offset - 25); draw(); });
  next.addEventListener('click', () => { offset += 25; draw(); });
  draw();
  navigation.append(previous, page, next);
  host.appendChild(navigation);
  tableViews.set(host, (nextRows, nextUtc) => { rows = nextRows; utc = nextUtc; draw(); });
  wrapper.appendChild(grid);
  host.appendChild(wrapper);
}
function fillResultBody(body: HTMLElement, entry: DecoderLogEntry, utc: boolean): void {
  const doc = body.ownerDocument;
  const target = body;
  body = doc.createElement('div');
  const audio: number[][] = [];
  const tables: { key: string; rows: unknown[]; label: string }[] = [];
  const appendTable = (rows: unknown[], label: string): void => {
    const section = doc.createElement('section');
    const key = `${tables.length}:${label}`; section.dataset.resultTable = key; section.dataset.key = key;
    body.append(section); tables.push({ key, rows, label });
  };
  const commit = (): void => {
    patchWorkbench(target, body.innerHTML, element => element.hasAttribute('data-result-table'));
    const canvas = target.querySelector<HTMLCanvasElement>('[data-audio-scope]');
    if (canvas && audio.length) drawAudioScope(canvas, audio, 1);
    for (const section of target.querySelectorAll<HTMLElement>('[data-result-table]')) {
      const model = tables.find(item => item.key === section.dataset.resultTable);
      if (model) table(section, model.rows, model.label, utc);
    }
  };
  const data = entry.data;
  if (entry.kind === 'status' && record(data) && data.type === 'rf-measurement' && record(data.measurement)) {
    const m = data.measurement;
    const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
    const heading = doc.createElement('strong'); heading.textContent = 'RF SNR'; body.append(heading);
    const stale = typeof m.max_age_seconds === 'number' && Date.now() - entry.at > m.max_age_seconds * 1000;
    const value = !stale && m.state !== 'unavailable' && typeof m.value === 'number' ? `${m.value.toFixed(1)} dB` : '—';
    fields(body, { value, bandwidth: `${Number(m.reference_bandwidth_hz).toFixed(0)} Hz`,
      state: stale ? l('Unavailable: stale', '不可用：数据过期') : m.state,
      reason: stale ? 'stale' : m.reason,
      method: l('Hann/Welch; sparse occupancy, flat background assumed', 'Hann/Welch；假定稀疏占用、平坦背景'),
      background: l('Includes unresolved interference; occupied power includes all signals in band', '背景包括无法区分的干扰；带内功率包括所有信号'),
      averaging: `${Number(m.averaging_seconds).toFixed(2)} s`,
      observation: `${Number(m.observation_seconds).toFixed(2)} s`,
      location: l('IQ before demodulation, AGC, squelch, NR and EQ', '解调、AGC、静噪、降噪和 EQ 前的 IQ') });
    commit(); return;
  }
  if (entry.kind === 'status' && record(data)
    && (typeof data.type === 'string' ? ['am-info', 'wfm-info', 'drm', 'ft8-state', 'sstv-state', 'radiofax-state', 'cw-level', 'rtty-state'].includes(data.type)
      : 'carrierHz' in data || 'rdsLocked' in data || record(data.status))
    && ['am', 'wfm', 'drm', 'drmplus', 'ft8', 'sstv', 'radiofax', 'cw', 'rtty', 'rtty-reverse'].includes(entry.mode)) {
    const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
    const metric = (label: string, value: unknown, unit = '', digits = 1): void => {
      const cell = doc.createElement('div'); cell.className = `decoder-metric${label === 'RadioText' ? ' decoder-metric-wide' : ''}`;
      const term = doc.createElement('dt'), description = doc.createElement('dd'); term.textContent = label;
      const numeric = typeof value === 'number' || typeof value === 'string' && (unit !== '' || digits === 0) && value.trim() !== '' && Number.isFinite(Number(value));
      description.textContent = numeric && Number.isFinite(Number(value)) ? Number(value).toFixed(digits) : typeof value === 'string' && value ? value : '—';
      if (unit && numeric && Number.isFinite(Number(value))) { const suffix = doc.createElement('small'); suffix.textContent = ` ${unit}`; description.append(suffix); }
      cell.append(term, description); metrics.append(cell);
    };
    const heading = doc.createElement('strong'), metrics = doc.createElement('dl'); metrics.className = 'decoder-fields decoder-metrics';
    heading.className = 'decoder-signal-heading';
    const extras = doc.createElement('div'); extras.className = 'decoder-signal-extras';
    if (entry.mode === 'am') {
      heading.textContent = data.syncEnabled === true ? data.syncLocked ? t('drm_locked') : l('Acquiring carrier', '正在捕获载波') : l('AM signal', 'AM 信号');
      metric(l('Carrier', '载波'), typeof data.carrierHz === 'number' ? data.carrierHz / 1e6 : undefined, 'MHz', 6);
      metric(l('Carrier offset', '载波偏移'), data.carrierOffsetHz, 'Hz');
      metric(l('Modulation', '调制度'), data.modulationPct, '%');
      metric(l('Input level', '输入电平'), data.inputDbFs, 'dBFS');
      metric(l('Audio RMS', '音频有效电平'), data.rmsDbFs, 'dBFS');
      metric(l('Audio peak', '音频峰值'), data.peakDbFs, 'dBFS');
      if (Array.isArray(data.scope)) audio.push(data.scope);
    } else if (entry.mode === 'wfm') {
      heading.textContent = data.rdsLocked ? String(data.ps || l('Receiving station name…', '正在接收台名…')) : l('Waiting for RDS', '等待 RDS');
      metric(l('Audio', '音频'), data.stereo === undefined ? undefined : data.stereo ? l('Stereo', '立体声') : l('Mono', '单声道'));
      metric('RadioText', data.rdsLocked ? data.rt : undefined);
      metric('PI', data.rdsLocked && typeof data.rdsPi === 'number' ? data.rdsPi.toString(16).toUpperCase().padStart(4, '0') : undefined);
      metric(l('Peak deviation', '峰值频偏'), typeof data.peakDevHz === 'number' ? data.peakDevHz / 1000 : undefined, 'kHz');
      metric(l('Pilot', '导频'), data.pilotPct, '%');
      metric(l('Pilot contrast (SNR estimate)', '导频对比度（SNR 估计）'), data.pilotSnrDb, 'dB');
      if (Array.isArray(data.measurements)) {
        const pilot = data.measurements.find(m => record(m) && m.metric_type === 'pilot_contrast');
        if (record(pilot)) metric(l('Pilot reference ENBW', '导频参考 ENBW'), pilot.reference_bandwidth_hz, 'Hz');
      }
      metric(l('Pilot deviation', '导频频偏'), data.pilotDevHz, 'Hz');
      metric(l('Carrier', '载波'), typeof data.carrierHz === 'number' ? data.carrierHz / 1e6 : undefined, 'MHz', 6);
      metric(l('Audio peak', '音频峰值'), data.peakDbFs, 'dBFS');
      metric(l('Peak hold', '峰值保持'), data.peakHoldDb, 'dBFS');
      metric(l('RDS groups', 'RDS 组数'), data.rdsGroups, '', 0);
      if (Array.isArray(data.scopeL)) audio.push(data.scopeL);
      if (data.stereo && Array.isArray(data.scopeR)) audio.push(data.scopeR);
    } else if (entry.mode === 'drm' || entry.mode === 'drmplus') {
      const status = record(data.status) ? data.status : {};
      const lines = Array.isArray(data.lines) ? data.lines.filter((line): line is string => typeof line === 'string') : [];
      const lockedLine = lines.find(line => line.startsWith('locked: '));
      const locked = status.locked ?? !!lockedLine;
      const counts = lines.find(line => line.includes(' MSC frames,'))?.match(/^(\d+) MSC frames, (\d+) audio AUs, FAC ok (\d+) err (\d+)/);
      const measured = (name: string): number | undefined => {
        const match = lines.find(line => line.startsWith(`FAC ${name} `))?.match(/ (-?\d+(?:\.\d+)?) dB/);
        return match ? Number(match[1]) : undefined;
      };
      heading.textContent = String(status.stationLabel || lines.find(line => line.startsWith('station: '))?.slice(9) || t('drm_station_waiting'));
      metric(l('Status', '状态'), t(locked ? 'drm_locked' : 'drm_searching'));
      metric(l('Mode', '模式'), status.mode || lockedLine?.slice(8) || lines.find(line => line.startsWith('DRM searching: '))?.match(/,\s*([A-E]\s+[\d.]+\s*kHz)/)?.[1]);
      const measurements = Array.isArray(status.measurements) ? status.measurements.filter(record) : [];
      const facSnr = measurements.find(m => m.metric_type === 'fac_decision_snr');
      metric(l('FAC SNR (AWGN estimate)', 'FAC SNR（白噪声估计）'), locked ? (facSnr ? facSnr.value : measured('SNR') ?? data.snrDb) : undefined, 'dB');
      if (facSnr) {
        metric(l('Reference bandwidth', '参考带宽'), facSnr.reference_bandwidth_hz, 'Hz', 0);
        metric(l('Method', '方法'), l('FAC decisions; MER ≥10 dB, flat AWGN channel only', 'FAC 判决；仅限 MER ≥10 dB 的平坦白噪声信道'));
      }
      const facMer = measurements.find(m => m.metric_type === 'fac_weighted_mer');
      metric('FAC MER', locked ? (facMer ? facMer.value : measured('MER')) : undefined, 'dB');
      metric(l('Carrier offset', '载波偏移'), status.carrierOffsetHz, 'Hz');
      metric(l('FAC valid', 'FAC 正确'), status.facOk ?? counts?.[3], '', 0);
      metric(l('FAC errors', 'FAC 错误'), status.facErrors ?? counts?.[4], '', 0);
      metric('SDC', status.sdcOk, '', 0);
      metric(l('MSC frames', 'MSC 帧数'), counts?.[1], '', 0);
      metric(l('Audio AUs', '音频单元'), counts?.[2], '', 0);
      if (Array.isArray(status.services)) for (const service of status.services) if (record(service)) {
        const card = doc.createElement('section'); card.className = 'decoder-service';
        const name = doc.createElement('strong'), info = doc.createElement('span');
        name.textContent = String(service.label || l('Programme', '节目'));
        info.textContent = [`ID ${service.serviceId ?? '—'}`, service.audioCoding === 0 ? 'AAC' : service.audioCoding === 3 ? 'xHE-AAC' : service.audioCoding != null ? `${l('Audio coding', '音频编码')} ${service.audioCoding}` : '',
          typeof service.sampleRate === 'number' ? `${service.sampleRate / 1000} kHz` : '', typeof service.channels === 'number' ? `${service.channels} ch` : ''].filter(Boolean).join(' · ');
        card.append(name, info); extras.append(card);
      }
      const plot = doc.createElement('figure'); plot.className = 'decoder-constellation';
      const caption = doc.createElement('figcaption'); caption.textContent = l('FAC constellation', 'FAC 星座图');
      const points = locked && Array.isArray(data.points) ? data.points.filter(p => record(p) && Number.isFinite(p.re) && Number.isFinite(p.im)).slice(-1024) as { re: number; im: number }[] : [];
      // A bounded vector plot stays crisp in both docked and desktop windows.
      plot.innerHTML = `<svg viewBox="0 0 180 180" role="img" aria-label="${caption.textContent}"><path class="decoder-plot-grid" d="M90 8v164M8 90h164"/>${[-1, 1].flatMap(re => [-1, 1].map(im => `<circle class="decoder-plot-target" cx="${90 + re * 56.57}" cy="${90 + im * 56.57}" r="5"/>`)).join('')}${points.map(p => `<circle cx="${Math.max(2, Math.min(178, 90 + p.re * 80))}" cy="${Math.max(2, Math.min(178, 90 - p.im * 80))}" r="1.2"/>`).join('')}</svg>`;
      if (!points.length) { const empty = doc.createElement('span'); empty.textContent = l('Waiting for constellation', '等待星座数据'); plot.append(empty); }
      plot.append(caption);
      const dashboard = doc.createElement('div'); dashboard.className = 'decoder-drm-dashboard'; dashboard.append(metrics, extras, plot);
      body.append(heading, dashboard);
    } else if (entry.mode === 'rtty' || entry.mode === 'rtty-reverse') {
      heading.textContent = data.state === 'receiving' ? l('Receiving RTTY', '正在接收 RTTY')
        : data.state === 'tone-pair-locked' ? l('Tone pair locked', '双音已锁定') : l('Searching for RTTY', '正在搜索 RTTY');
      metric(l('Baud', '波特率'), data.baud, 'Bd', 2);
      metric(l('Shift', '频移'), data.shiftHz, 'Hz');
      metric('Mark', data.markHz, 'Hz'); metric('Space', data.spaceHz, 'Hz');
      metric(l('Quality', '质量'), data.quality, '', 2);
      metric(l('Characters', '字符数'), data.characters, '', 0);
      metric(l('Framing errors', '帧错误'), data.framingErrors, '', 0);
      metric(l('Polarity', '极性'), data.reverse ? l('Reverse', '反转') : l('Normal', '正常'));
    } else if (entry.mode === 'cw') {
      heading.textContent = data.keyed ? l('Morse signal detected', '检测到摩尔斯信号') : l('Waiting for Morse signal', '等待摩尔斯信号');
      metric(l('Speed', '速度'), data.wpm, 'WPM');
      metric(l('Tone', '音调'), data.pitchHz, 'Hz');
      metric(l('Tuning offset', '调谐偏移'), data.afcHz, 'Hz');
      metric(l('Signal level', '信号电平'), typeof data.rms === 'number' && data.rms > 0 ? 20 * Math.log10(data.rms) : undefined, 'dBFS');
      metric(l('Tone energy share', '窄带能量占比'), typeof data.share === 'number' ? data.share * 100 : undefined, '%');
    } else {
      heading.textContent = entry.mode === 'radiofax' && typeof data.stage === 'string' ? t(`radiofax_stage_${data.stage}`)
        : entry.mode === 'sstv' ? data.hasPicture ? l('Receiving picture', '正在接收图像') : t('sstv_waiting')
          : l('FT8 decoder', 'FT8 解码器');
      metric(l('Audio buffered', '已缓冲音频'), data.bufferedSeconds, 's');
      metric(l('Decode attempts', '解码尝试'), data.attempts, '', 0);
      if (entry.mode === 'ft8') metric(l('Messages decoded', '已解码消息'), data.count, '', 0);
      if (entry.mode === 'sstv') { metric(l('Signal level', '信号电平'), data.level); metric(l('Source level', '输入电平'), data.srcLevel); }
      if (entry.mode === 'radiofax') {
        metric('IOC', data.ioc, '', 0); metric('LPM', data.lpm, '', 0);
        metric(l('Signal level', '信号电平'), data.signalLevel);
        metric(l('Carrier coherence', '载波相干度'), data.carrierCoherence, '', 2);
        metric(l('Elapsed', '已接收'), data.elapsedSeconds, 's');
        metric(l('Clock correction', '时钟校正'), data.clockPpm, 'ppm');
        metric(l('Clock confidence', '时钟置信度'), data.clockConfidence, '', 2);
        metric(l('Phase', '相位'), data.phasePixels, 'px');
        metric(l('Modulation', '调制'), data.modulation); metric(l('Polarity', '极性'), data.polarity);
      }
    }
    if (!body.contains(heading)) body.append(heading, metrics);
    if (audio.some(samples => samples.length > 1)) {
      const canvas = doc.createElement('canvas'); canvas.dataset.audioScope = ''; canvas.dataset.key = 'audio-scope';
      canvas.width = 640; canvas.height = audio.length > 1 ? 160 : 100;
      canvas.className = 'mc-audio-scope'; canvas.setAttribute('role', 'img');
      canvas.title = canvas.ariaLabel = l('Demodulated audio before local volume; full scale ±1', '本地音量调整前的解调音频，满刻度 ±1');
      body.append(canvas);
    }
    commit(); return;
  }
  if (entry.mode === 'gps' && record(data) && data.bench === true) {
    const origin = doc.createElement('strong');
    origin.className = 'decoder-origin';
    origin.textContent = t('decoder_gps_bench');
    body.appendChild(origin);
  }
  if (entry.kind === 'text' || typeof data === 'string') {
    const text = doc.createElement('pre');
    text.textContent = record(data) && typeof data.text === 'string' ? data.text : readable(data);
    body.appendChild(text);
  } else if (Array.isArray(data)) appendTable(data, entry.mode);
  else if (record(data)) {
    fields(body, data, entry.kind === 'status');
    if (entry.kind !== 'status') {
      for (const [key, value] of Object.entries(data)) {
        if (Array.isArray(value)) appendTable(value, key === 'rows' && typeof data.table === 'string' ? data.table : key.replace(/_/g, ' '));
      }
    } else if (entry.mode === 'gps' && (data.state === 'stopped' || (record(data.fix) && data.fix.valid === false))) {
      const position = doc.createElement('p');
      position.textContent = t('decoder_position_invalid');
      body.appendChild(position);
    }
  } else {
    const text = doc.createElement('p');
    text.textContent = readable(data);
    body.appendChild(text);
  }
  commit();
}

/** Repaint a live entry in place; a replaced node would flash and lose the viewer's expansion. */
type ResultUpdater = (entry: DecoderLogEntry, utc: boolean) => void;
const resultUpdaters = new WeakMap<HTMLElement, ResultUpdater>();

function resultNode(doc: Document, entry: DecoderLogEntry, utc: boolean): HTMLElement {
  const article = doc.createElement('article');
  article.className = `decoder-result decoder-result-${entry.kind}`;
  if (entry.kind === 'image' && (entry.data as DecoderRaster).raster) {
    const title = doc.createElement('strong');
    const canvas = doc.createElement('canvas');
    let painted: { rgb: Uint8Array; version: number } | undefined;
    canvas.style.width = '100%'; canvas.setAttribute('role', 'img');
    const update: ResultUpdater = next => {
      const picture = next.data as DecoderRaster;
      title.textContent = `${picture.label} · ${picture.w} × ${picture.h} · ${picture.rows}/${picture.h}${picture.final ? ` · ${t('decoder_image_complete')}` : ''}${picture.partial ? ` · ${t('decoder_image_partial')}` : ''}`;
      if (canvas.width !== picture.w || canvas.height !== picture.h) {
        canvas.width = picture.w; canvas.height = picture.h; painted = undefined;
      }
      canvas.setAttribute('aria-label', picture.label);
      const context = canvas.getContext('2d');
      if (context) {
        const change = decoderRasterChanges(picture, painted);
        if (!change.rows) return;
        const pixels = context.createImageData(picture.w, change.rows), start = change.row * picture.w * 3;
        for (let i = 0; i < picture.w * change.rows; i++) {
          pixels.data[i * 4] = picture.rgb[start + i * 3]; pixels.data[i * 4 + 1] = picture.rgb[start + i * 3 + 1];
          pixels.data[i * 4 + 2] = picture.rgb[start + i * 3 + 2]; pixels.data[i * 4 + 3] = 255;
        }
        context.putImageData(pixels, 0, change.row);
        painted = { rgb: picture.rgb, version: change.version };
      }
    };
    update(entry, utc); resultUpdaters.set(article, update);
    article.append(title, canvas); return article;
  }
  if (entry.kind === 'image') {
    const title = doc.createElement('strong');
    article.appendChild(title);
    const image = doc.createElement('div');
    image.className = 'decoder-image';
    image.setAttribute('role', 'img');
    const strips = new Map<DecoderImage['strips'][number], HTMLImageElement>();
    const update: ResultUpdater = next => {
      const picture = next.data as DecoderImage;
      const extent = picture.strips.reduce((end, strip) => Math.max(end, strip.y + strip.rows), 1);
      title.textContent = `${picture.label || next.mode} · ${picture.w} × ${extent} · ${t('decoder_image_rows', { rows: extent, total: picture.h })}${picture.final && !picture.partial ? ` · ${t('decoder_image_complete')}` : ''}${picture.partial ? ` · ${t('decoder_image_partial')}` : ''}`;
      image.style.aspectRatio = `${picture.w} / ${extent}`;
      image.setAttribute('aria-label', picture.label || next.mode);
      for (const [strip, img] of strips) if (!picture.strips.includes(strip)) { img.remove(); strips.delete(strip); }
      for (const strip of picture.strips) {
        let img = strips.get(strip);
        if (!img) {
          img = doc.createElement('img'); img.alt = ''; img.src = `data:image/png;base64,${strip.png}`;
          img.addEventListener('error', () => { title.textContent = t('decoder_image_failed'); title.setAttribute('role', 'alert'); });
          strips.set(strip, img); image.appendChild(img);
        }
        img.style.top = `${strip.y / extent * 100}%`;
        img.style.height = `${strip.rows / extent * 100}%`;
      }
    };
    update(entry, utc); resultUpdaters.set(article, update);
    article.appendChild(image);
    return article;
  }
  const header = doc.createElement('div');
  header.className = 'decoder-result-header';
  header.title = t('decoder_received_tip');
  article.appendChild(header);
  const body = doc.createElement('div');
  article.appendChild(body);
  const details = doc.createElement('details');
  const summary = doc.createElement('summary');
  summary.textContent = t('decoder_details');
  const raw = doc.createElement('pre');
  const current = { entry };
  const copy = doc.createElement('template');
  copy.innerHTML = copyDetailsButton();
  const copyButton = copy.content.firstElementChild as HTMLButtonElement;
  copyButton.addEventListener('click', event => {
    event.preventDefault(); event.stopPropagation();
    void copyDetails(copyButton, JSON.stringify(current.entry.data, null, 2));
  });
  // Materialize raw details only when requested; the bounded log owns the original payload.
  details.addEventListener('toggle', () => { raw.textContent = details.open ? JSON.stringify(current.entry.data, null, 2) : ''; });
  details.append(summary, copyButton, raw);
  article.appendChild(details);
  const update: ResultUpdater = (next, nextUtc) => {
    current.entry = next;
    article.className = `decoder-result decoder-result-${next.kind}`;
    header.title = t('decoder_received_tip'); summary.textContent = t('decoder_details');
    header.textContent = `${next.provider === 'native' ? next.mode.toUpperCase() : next.provider} · ${clock(next.at, nextUtc)} ${nextUtc ? 'UTC' : 'LT'}`;
    fillResultBody(body, next, nextUtc);
    if (details.open) raw.textContent = JSON.stringify(next.data, null, 2);
  };
  update(entry, utc);
  resultUpdaters.set(article, update);
  return article;
}

export function renderDecoderResults(host: HTMLElement, channel: number, mode: string, utc: boolean): void {
  renderDecoderEntries(host, decoderLog(channel, mode).entries, `${channel}:${mode}`, utc, decoderLog(channel, mode).evicted);
}

/** Shared renderer; development and native adapters supply isolated retained entries. */
export function renderDecoderEntries(host: HTMLElement, input: readonly DecoderLogEntry[], identity: string, utc: boolean, evicted = 0, limitMiB = 8): void {
  const log = { entries: input, evicted };
  const entries = [...log.entries].sort((a, b) => Number(b.kind === 'image') - Number(a.kind === 'image'));
  const presentation = `${utc}:${getLang()}`;
  let cache = rendered.get(host);
  if (!cache || cache.identity !== identity) {
    cache = { identity, presentation, nodes: new Map() };
    rendered.set(host, cache);
    host.replaceChildren();
  }
  const ids = new Set(log.entries.map(entry => entry.id));
  for (const [id, item] of cache.nodes) if (!ids.has(id)) { item.node.remove(); cache.nodes.delete(id); }
  let notice = host.querySelector<HTMLElement>('.decoder-retention');
  if (!notice) { notice = host.ownerDocument.createElement('p'); notice.className = 'decoder-retention'; host.prepend(notice); }
  const noticeText = !log.entries.length ? t('decoder_waiting') : t('decoder_retention', { count: log.entries.length, evicted: log.evicted, limit_mib: limitMiB });
  if (notice.textContent !== noticeText) notice.textContent = noticeText;
  let previous: Element = notice;
  for (const entry of entries) {
    let item = cache.nodes.get(entry.id);
    if (item && (item.entry !== entry || cache.presentation !== presentation)) {
      const update = resultUpdaters.get(item.node);
      if (update) { update(entry, utc); item.entry = entry; }
      else item = undefined;
    }
    if (!item) {
      const node = resultNode(host.ownerDocument, entry, utc);
      cache.nodes.get(entry.id)?.node.replaceWith(node);
      item = { entry, node };
      cache.nodes.set(entry.id, item);
    }
    const timing = entry.timing;
    const title = timing ? `${entry.source} · input ${timing.input_index} + ${timing.fraction_num}/${timing.fraction_den} @ ${timing.input_rate} S/s · delay ${timing.delay_known ? timing.delay_seconds : '?'} s${timing.causal_presentation ? ' · causal presentation' : ''}` : '';
    if (item.node.title !== title) item.node.title = title;
    if (previous.nextElementSibling !== item.node) previous.after(item.node);
    previous = item.node;
  }
  cache.presentation = presentation;
}
