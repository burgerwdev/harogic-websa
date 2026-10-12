// SDR channel rows (ux-3): three tunable DDC channels with a listen selector, activity
// LEDs, Ham S badges and the PEP peak meter.
//
// State ownership follows the app pattern: the STATUS handler calls confirmSdrChans()
// (authoritative), user actions only send commands and let STATUS confirm. The DOM is
// updated exclusively here.

import { sReport } from '../core/sReport';
import { setFt8Source } from '../sdr/iqStream';
import { send } from '../core/wsSend';

let listenChannel = 1;
let chans: Record<string, any> = {};
let peakDbm: number | null = null;
let statusWarning = 0;

function el<T extends HTMLElement = HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

/** MHz with four decimals, or '' when the channel has no frequency yet. */
function fmtMhz(freqHz: unknown): string {
  const f = Number(freqHz);
  return Number.isFinite(f) && f > 0 ? (f / 1e6).toFixed(4) : '';
}

export function confirmSdrChans(s: any): void {
  listenChannel = Number(s.req?.sdr?.listen_channel ?? s.sdr_listen_channel ?? 1);
  chans = s.req?.sdr?.chans ?? s.sdr_chans ?? {};
  peakDbm = s.req?.sdr?.peak_dbm ?? null;
  statusWarning = Number(s.status_warning) || 0;
  render();
}

function render(): void {
  const seg = el('listen-seg');
  if (seg) {
    for (const btn of Array.from(seg.querySelectorAll('.seg-btn'))) {
      btn.classList.toggle('on', Number((btn as HTMLElement).dataset.chan) === listenChannel);
    }
  }
  for (const ch of [1, 2, 3]) {
    const row = el(`chan-row-${ch}`);
    if (!row) continue;
    const rowAny = row as any;
    const cfg = chans[String(ch)] || {};
    const enabled = !!cfg.enabled;
    const isListen = ch === listenChannel;
    rowAny.classList.toggle('listen', isListen);
    rowAny.classList.toggle('dis', !enabled);
    const led = el(`chan-led-${ch}`);
    if (led) led.className = `chan-led${cfg.active ? ' on' : ''}`;
    const input = el(`chan-freq-${ch}`) as HTMLInputElement | null;
    if (input && document.activeElement !== input) input.value = fmtMhz(cfg.freq);
    const badge = el(`chan-s-${ch}`);
    if (badge) {
      const lvl = Number(cfg.level_db);
      const has = Number.isFinite(lvl) && lvl > -199;
      const rep = has ? sReport(lvl) : '-';
      badge.textContent = rep;
      badge.className = `s-badge${/S9/.test(rep) ? ' hi' : (/S[5-8]/.test(rep) ? ' mid' : '')}`;
    }
    const thr = el(`chan-thr-${ch}`) as HTMLInputElement | null;
    if (thr && document.activeElement !== thr) {
      thr.value = String(Number(cfg.threshold_offset_db ?? 6));
    }
  }
  // PEP row
  const val = el('pep-val');
  if (val) val.textContent = peakDbm != null ? `${peakDbm.toFixed(1)} dBm` : '-';
  const ps = el('pep-s');
  if (ps) {
    const rep = peakDbm != null ? sReport(peakDbm) : '-';
    ps.textContent = rep;
    ps.className = `s-badge${/S9/.test(rep) ? ' hi' : (/S[5-8]/.test(rep) ? ' mid' : '')}`;
  }
  const fill = el('pep-fill');
  if (fill) {
    const over = statusWarning === -12;
    fill.classList.toggle('over', over);
    const pct = peakDbm != null ? Math.max(2, Math.min(100, (peakDbm + 140) / 130 * 100)) : 2;
    fill.style.width = `${pct}%`;
  }
}

/** Commit one channel's frequency (MHz). Re-enables a disabled channel. */
export function chanFreqCommit(ch: number, mhz: number): void {
  if (!Number.isFinite(mhz) || mhz <= 0) return;
  send({ cmd: 'SET_SDR_DDC', channel: ch, enabled: true, freq: mhz * 1e6 });
}

export function setListen(ch: number): void {
  if (ch === listenChannel) return;
  send({ cmd: 'SET_SDR_LISTEN', channel: ch });
}

export function chanDisable(ch: number): void {
  if (ch === listenChannel) return;               // refused server-side too; keep UX honest
  send({ cmd: 'SET_SDR_DDC', channel: ch, enabled: false });
}

export function chanThreshold(ch: number, offsetDb: number): void {
  send({ cmd: 'SET_SDR_DDC', channel: ch, threshold: offsetDb });
}

function inTextEntry(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  if (!t) return false;
  const tag = t.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || !!t.isContentEditable;
}

export function initSdrChannels(): void {
  // Row click = switch listen (inputs / threshold / × excluded).
  for (const ch of [1, 2, 3]) {
    const row = el(`chan-row-${ch}`);
    if (row) {
      row.addEventListener('click', (ev) => {
        const t = ev.target as HTMLElement;
        if (t.tagName === 'INPUT' || t.tagName === 'SELECT' || t.closest('.chan-x')) return;
        setListen(ch);
      });
    }
    const freq = el(`chan-freq-${ch}`) as HTMLInputElement | null;
    if (freq) {
      const commit = () => chanFreqCommit(ch, parseFloat(freq.value));
      freq.addEventListener('change', commit);
      freq.addEventListener('keydown', (e) => {
        if ((e as KeyboardEvent).key === 'Enter') commit();
      });
    }
    const thr = el(`chan-thr-${ch}`) as HTMLInputElement | null;
    if (thr) {
      thr.addEventListener('change', () => {
        if (freq && !fmtMhz(chans[String(ch)]?.freq)) return;   // no freq -> nothing to guard
        chanThreshold(ch, parseFloat(thr.value));
      });
    }
    const x = el(`chan-x-${ch}`);
    if (x) x.addEventListener('click', () => chanDisable(ch));
  }
  const seg = el('listen-seg');
  if (seg) {
    seg.addEventListener('click', (ev) => {
      const btn = (ev.target as HTMLElement).closest('[data-chan]') as HTMLElement | null;
      if (btn) setListen(Number(btn.dataset.chan));
    });
  }
  const ft8sel = el('ft8-source') as HTMLSelectElement | null;
  if (ft8sel) {
    ft8sel.addEventListener('change', () => {
      const v = ft8sel.value;
      setFt8Source(v === 'listen' ? 'listen' : 'aux');
    });
  }
  // Keyboard 1/2/3: switch listen, but never while the user is typing somewhere.
  document.addEventListener('keydown', (e: KeyboardEvent) => {
    if (inTextEntry(e.target) || e.ctrlKey || e.altKey || e.metaKey) return;
    if (e.key === '1' || e.key === '2' || e.key === '3') setListen(Number(e.key));
  });
}
