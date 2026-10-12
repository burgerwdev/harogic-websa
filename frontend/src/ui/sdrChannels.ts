// SDR channel rows (ux-3): three tunable DDC channels with a listen selector, activity
// LEDs, Ham S badges and the PEP peak meter.
//
// State ownership follows the app pattern: the STATUS handler calls confirmSdrChans()
// (authoritative), user actions only send commands and let STATUS confirm. The DOM is
// updated exclusively here.

import { sReport, sScalePct, S_SCALE_TICKS } from '../core/sReport';
import { setFt8Source } from '../sdr/iqStream';
import { send } from '../core/wsSend';

let listenChannel = 1;
let chans: Record<string, any> = {};
let peakDbm: number | null = null;
let statusWarning = 0;
// LED 三态 (design §4.2): on = active now; burst = activity ended within 2 s; off = quiet.
const lastActiveAt: Record<number, number> = {};
// FT8 background source (ux-4): 'listen' or a channel number with its centre frequency.
let ft8Source: 'listen' | 2 | 3 = 'listen';
let ft8FreqHz = 0;

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
    if (cfg.active) lastActiveAt[ch] = Date.now();
    const led = el(`chan-led-${ch}`);
    if (led) {
      // 三态: on (持续活动) / burst (活动结束后 2 s 内) / off (静默)
      const since = Date.now() - (lastActiveAt[ch] || 0);
      const cls = cfg.active ? ' on' : (since < 2000 ? ' burst' : '');
      led.className = `chan-led${cls}`;
    }
    const input = el(`chan-freq-${ch}`) as HTMLInputElement | null;
    if (input && document.activeElement !== input) input.value = fmtMhz(cfg.freq);
    const badge = el(`chan-s-${ch}`);
    if (badge) {
      const lvl = Number(cfg.level_db);
      const has = Number.isFinite(lvl) && lvl > -199;
      const rep = has ? sReport(lvl) : '-';
      badge.textContent = rep;
      badge.className = `s-badge${/S9/.test(rep) ? ' hi' : (/S[5-8]/.test(rep) ? ' mid' : '')}`;
      // Ham RST readout on the listen row: readability is 5 by convention, strength is
      // the S value (S9+X dB over stays "59+X").
      if (ch === listenChannel && has) badge.title = rep.replace('S', '5');
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
  const bar = el('pep-bar');
  if (bar && !bar.dataset.ticks) {
    // S-scale ticks (ux-3): S1..S9 unit boundaries + the +dB range ends, mounted once.
    bar.dataset.ticks = '1';
    for (const t of S_SCALE_TICKS) {
      const line = document.createElement('span');
      line.className = 'pep-tick';
      line.style.left = `${t.pct}%`;
      const label = document.createElement('span');
      label.className = 'pep-tick-label';
      label.textContent = t.label;
      label.style.left = `${t.pct}%`;
      line.appendChild(label);
      bar.appendChild(line);
    }
  }
  const fill = el('pep-fill');
  if (fill) {
    const over = statusWarning === -12 || (peakDbm != null && peakDbm >= -13);
    fill.classList.toggle('over', over);
    fill.style.width = `${peakDbm != null ? sScalePct(peakDbm) : 2}%`;
  }
  // FT8 background hint + centre auto-follow when the FT8 channel is retuned.
  const hint = el('ft8-hint');
  if (ft8Source !== 'listen') {
    const freq = Number(chans[String(ft8Source)]?.freq);
    if (freq && freq !== ft8FreqHz) {
      ft8FreqHz = freq;
      setFt8Source(ft8Source, freq);
    }
    if (hint) {
      hint.textContent = `FT8 decoding CH${ft8Source} ${fmtMhz(freq)} MHz (background)`;
      hint.style.display = '';
    }
  } else if (hint) {
    hint.style.display = 'none';
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
      if (v === 'listen') {
        ft8Source = 'listen';
        ft8FreqHz = 0;
        setFt8Source('listen');
        return;
      }
      const freq = Number(chans[v]?.freq);
      if (!freq) { ft8sel.value = 'listen'; return; }   // an unfrequency'd channel cannot feed FT8
      ft8Source = Number(v) as 2 | 3;
      ft8FreqHz = freq;
      setFt8Source(ft8Source, freq);
    });
  }
  // Drives the LED burst->off decay between STATUS frames (1 Hz).
  setInterval(render, 500);
  // Keyboard 1/2/3: switch listen, but never while the user is typing somewhere.
  document.addEventListener('keydown', (e: KeyboardEvent) => {
    if (inTextEntry(e.target) || e.ctrlKey || e.altKey || e.metaKey) return;
    if (e.key === '1' || e.key === '2' || e.key === '3') setListen(Number(e.key));
  });
}
