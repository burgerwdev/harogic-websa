import { t, getLang } from '../core/i18n';
import { clock } from '../sdr/ft8Log';
import { decoderLog, type DecoderImage, type DecoderLogEntry } from '../sdr/decoderLog';

const rendered = new WeakMap<HTMLElement, { signature: string; nodes: Map<number, { entry: DecoderLogEntry; node: HTMLElement }> }>();
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
  list.className = 'decoder-fields';
  const hasRows = Object.values(data).some(Array.isArray);
  for (const [key, value] of Object.entries(data)) {
    if (['v', 'kind', 'type', 'png'].includes(key) || Object.prototype.hasOwnProperty.call(provenanceFields, key) || /^virtual/i.test(key) || Array.isArray(value)) continue;
    if (hasRows && !Object.prototype.hasOwnProperty.call(summaryFields, key)) continue;
    if (status && !['state', 'message', 'why', 'lines', 'rows', 'snr_db'].includes(key)) continue;
    const term = doc.createElement('dt');
    term.textContent = fieldLabel(key);
    const description = doc.createElement('dd');
    description.textContent = readable(value);
    list.append(term, description);
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
function table(host: HTMLElement, rows: unknown[], label: string, utc: boolean): void {
  const doc = host.ownerDocument;
  const heading = doc.createElement('strong');
  heading.textContent = ['aircraft', 'satellites', 'pvt', 'navigation'].includes(label) ? t(`decoder_table_${label}`) : label;
  host.appendChild(heading);
  if (!rows.length) return;
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
    grid.replaceChildren();
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
      const body = grid.createTBody();
      for (const value of visible) body.insertRow().insertCell().textContent = readable(value);
    } else {
      const header = grid.createTHead().insertRow();
      for (const key of columns) {
        const th = doc.createElement('th');
        th.textContent = key === 'slot_utc' ? `${t('ft8_time')} (${utc ? 'UTC' : 'LT'})` : fieldLabel(key);
        header.appendChild(th);
      }
      const body = grid.createTBody();
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
    page.textContent = ` ${offset + 1}–${Math.min(offset + 25, rows.length)} / ${rows.length} `;
    previous.disabled = offset === 0;
    next.disabled = offset + 25 >= rows.length;
  };
  previous.addEventListener('click', () => { offset = Math.max(0, offset - 25); draw(); });
  next.addEventListener('click', () => { offset += 25; draw(); });
  draw();
  navigation.append(previous, page, next);
  if (rows.length > 25) host.appendChild(navigation);
  wrapper.appendChild(grid);
  host.appendChild(wrapper);
}
function fillResultBody(body: HTMLElement, entry: DecoderLogEntry, utc: boolean): void {
  const doc = body.ownerDocument;
  body.replaceChildren();
  const data = entry.data;
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
  } else if (Array.isArray(data)) table(body, data, entry.mode, utc);
  else if (record(data)) {
    fields(body, data, entry.kind === 'status');
    if (entry.kind !== 'status') {
      for (const [key, value] of Object.entries(data)) {
        if (Array.isArray(value)) table(body, value, key === 'rows' && typeof data.table === 'string' ? data.table : key.replace(/_/g, ' '), utc);
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
}

/** Repaint a live entry in place; a replaced node would flash and lose the viewer's expansion. */
type ResultUpdater = (entry: DecoderLogEntry, utc: boolean) => void;
const resultUpdaters = new WeakMap<HTMLElement, ResultUpdater>();

function resultNode(doc: Document, entry: DecoderLogEntry, utc: boolean): HTMLElement {
  const article = doc.createElement('article');
  article.className = `decoder-result decoder-result-${entry.kind}`;
  if (entry.kind === 'image') {
    const picture = entry.data as DecoderImage;
    const extent = picture.strips.reduce((end, strip) => Math.max(end, strip.y + strip.rows), 1);
    const title = doc.createElement('strong');
    title.textContent = `${picture.label || entry.mode} · ${picture.w} × ${extent} · ${t('decoder_image_rows', { rows: extent, total: picture.h })}${picture.final ? ` · ${t('decoder_image_complete')}` : ''}`;
    article.appendChild(title);
    const image = doc.createElement('div');
    image.className = 'decoder-image';
    image.style.aspectRatio = `${picture.w} / ${extent}`;
    image.setAttribute('role', 'img');
    image.setAttribute('aria-label', picture.label || entry.mode);
    for (const strip of picture.strips) {
      const img = doc.createElement('img');
      img.alt = '';
      img.src = `data:image/png;base64,${strip.png}`;
      img.style.top = `${strip.y / extent * 100}%`;
      img.style.height = `${strip.rows / extent * 100}%`;
      img.addEventListener('error', () => {
        title.textContent = t('decoder_image_failed');
        title.setAttribute('role', 'alert');
      });
      image.appendChild(img);
    }
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
  // Materialize raw details only when requested; the bounded log owns the original payload.
  details.addEventListener('toggle', () => { raw.textContent = details.open ? JSON.stringify(current.entry.data, null, 2) : ''; });
  details.append(summary, raw);
  article.appendChild(details);
  const update: ResultUpdater = (next, nextUtc) => {
    current.entry = next;
    article.className = `decoder-result decoder-result-${next.kind}`;
    header.textContent = `${next.provider} · ${clock(next.at, nextUtc)} ${nextUtc ? 'UTC' : 'LT'}`;
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
export function renderDecoderEntries(host: HTMLElement, input: readonly DecoderLogEntry[], identity: string, utc: boolean, evicted = 0): void {
  const log = { entries: input, evicted };
  const entries = [...log.entries].sort((a, b) => Number(b.kind === 'image') - Number(a.kind === 'image'));
  const signature = `${identity}:${utc}:${getLang()}`;
  let cache = rendered.get(host);
  if (!cache || cache.signature !== signature) {
    cache = { signature, nodes: new Map() };
    rendered.set(host, cache);
    host.replaceChildren();
  }
  const ids = new Set(log.entries.map(entry => entry.id));
  for (const [id, item] of cache.nodes) if (!ids.has(id)) { item.node.remove(); cache.nodes.delete(id); }
  let notice = host.querySelector<HTMLElement>('.decoder-retention');
  if (!notice) { notice = host.ownerDocument.createElement('p'); notice.className = 'decoder-retention'; host.prepend(notice); }
  notice.textContent = !log.entries.length ? t('decoder_waiting') : t('decoder_retention', { count: log.entries.length, evicted: log.evicted });
  let previous: Element = notice;
  for (const entry of entries) {
    let item = cache.nodes.get(entry.id);
    if (item && item.entry !== entry) {
      const update = resultUpdaters.get(item.node);
      if (update) { update(entry, utc); item.entry = entry; }
      else item = undefined;   // image strips rebuild; the picture owns its node
    }
    if (!item) {
      const node = resultNode(host.ownerDocument, entry, utc);
      cache.nodes.get(entry.id)?.node.replaceWith(node);
      item = { entry, node };
      cache.nodes.set(entry.id, item);
    }
    if (previous.nextElementSibling !== item.node) previous.after(item.node);
    previous = item.node;
  }
}
