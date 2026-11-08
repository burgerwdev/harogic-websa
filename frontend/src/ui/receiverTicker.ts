import { getLang } from '../core/i18n';
import type { ReceiverState } from '../core/multichannel';
import type { DecoderLogEntry } from '../sdr/decoderLog';
import { ft8GeoTexts } from './ft8GeoText';
import { belongsToReceiverStream } from './receiverLevel';

const textModes = new Set(['ft8', 'ft4', 'wspr', 'cw', 'rtty', 'rtty-reverse', 'navtex', 'dsc']);
const continuousModes = new Set(['cw', 'rtty', 'rtty-reverse']);
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
export interface StripMessage { key: string; text: string }
export function stripMessages(entries: readonly DecoderLogEntry[], receiver: ReceiverState, source?: string | null): StripMessage[] {
  const mode = receiver.confirmed.processors[0]?.mode;
  // Bound retained messages, never the number of characters visible in the strip.
  return entries.filter(entry => (source === undefined || source === entry.source) && entry.mode === mode && belongsToReceiverStream(receiver, entry)
    && ['text', 'table'].includes(entry.kind)).slice(0, 64).reverse().flatMap(entry => {
    const data = entry.data;
    if (entry.kind === 'text') {
      const text = typeof data === 'string' ? data : object(data) ? data.text ?? data.message : undefined;
      return typeof text === 'string' && text ? [{ key: String(entry.id), text }] : [];
    }
    const rows = object(data) && Array.isArray(data.decodes) ? data.decodes : Array.isArray(data) ? data : [];
    return rows.flatMap((row, index) => {
      if (!object(row)) return [];
      const text = row.msg ?? row.message ?? row.text;
      return typeof text === 'string' && text ? [{ key: `${entry.id}:${index}`, text }] : [];
    });
  }).slice(-48);
}

export interface ReceiverTicker {
  update(entries: readonly DecoderLogEntry[], receiver: ReceiverState | undefined, service: string, source?: string | null): void;
  dispose(): void;
}

/** Animation belongs to this view; it never consumes or clears shared decoder history. */
export function mountReceiverTicker(host: HTMLElement, root: HTMLElement): ReceiverTicker {
  const doc = host.ownerDocument;
  host.className = 'mc-strip-text'; host.dataset.noDrag = ''; host.hidden = true;
  const badge = doc.createElement('span'); badge.className = 'mc-ticker-mode';
  const viewport = doc.createElement('div'); viewport.className = 'mc-ticker-viewport'; viewport.tabIndex = 0;
  const line = doc.createElement('div'); line.className = 'mc-ticker-line';
  const text = doc.createElement('span'); text.className = 'mc-ticker-text';
  line.append(text); viewport.append(line); host.append(badge, viewport);
  let language = getLang();
  let identity = '', mode = '', items: StripMessage[] = [], index = 0, continuous = false, receiving = false;
  let target = '', visible = '', glyphs: string[] = [], written = 0, lastTick = 0, elapsed = 0;
  let hovered = false, focused = false, wasActive = false, disposed = false, width = 0;
  let animations: Animation[] = [], outgoing: HTMLElement | undefined;
  const segmenter = typeof Intl.Segmenter === 'function' ? new Intl.Segmenter(undefined, { granularity: 'grapheme' }) : undefined;
  const cancelMotion = (): void => { animations.forEach(animation => animation.cancel()); animations = []; outgoing?.remove(); outgoing = undefined; };
  const write = (value: string): void => { visible = value; text.textContent = value.replace(/\s+/g, ' '); };
  const choose = (next: number, animate: boolean): void => {
    cancelMotion();
    if (animate && line.animate && visible) {
      outgoing = line.cloneNode(true) as HTMLElement; outgoing.classList.add('mc-ticker-outgoing'); viewport.append(outgoing);
      outgoing.setAttribute('aria-hidden', 'true');
      const exiting = outgoing, exit = exiting.animate([{ transform: 'translateY(0)', opacity: 1 }, { transform: 'translateY(-100%)', opacity: 0 }], { duration: 260, fill: 'forwards' });
      exit.onfinish = () => { exiting.remove(); if (outgoing === exiting) outgoing = undefined; };
      animations.push(exit);
      animations.push(line.animate([{ transform: 'translateY(100%)', opacity: 0 }, { transform: 'translateY(0)', opacity: 1 }], { duration: 260 }));
    }
    index = next;
    const message = items[index]?.text ?? '';
    const geo = ['ft8', 'ft4'].includes(mode) && message ? ft8GeoTexts(message) : null;
    const details = geo ? [geo.country, geo.location].filter(value => value !== '—') : [];
    write([message, ...details].join(' · ')); elapsed = 0; text.style.transform = ''; viewport.title = visible;
  };
  const tick = (): void => {
    if (disposed) return;
    const now = performance.now(), delta = Math.min(80, Math.max(0, now - lastTick)); lastTick = now;
    const active = root.isConnected && !host.hidden && !root.closest('[hidden]')
      && (root.classList.contains('mc-collapsed') || root.classList.contains('mc-overview-card')) && !host.ownerDocument.hidden;
    if (!active) { if (wasActive) cancelMotion(); wasActive = false; return; }
    if (!wasActive) { elapsed = 0; wasActive = true; }
    if (!items.length || !receiving) return;
    const reduced = host.ownerDocument.defaultView?.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
    if (hovered || focused) return;
    if (width !== viewport.clientWidth) { width = viewport.clientWidth; elapsed = 0; }
    elapsed += delta;
    if (continuous) {
      if (written < glyphs.length) {
        // Catch up smoothly after bursts without accumulating minutes of typing lag.
        const step = reduced ? glyphs.length : Math.max(1, Math.ceil((glyphs.length - written) / 36));
        written = Math.min(glyphs.length, written + step); write(glyphs.slice(0, written).join(''));
      }
      const overflow = Math.max(0, text.scrollWidth - viewport.clientWidth);
      text.style.transform = `translateX(${-overflow}px)`;
      viewport.title = target;
    } else {
      if (reduced) { if (index !== items.length - 1) choose(items.length - 1, false); text.style.transform = ''; return; }
      const overflow = Math.max(0, text.scrollWidth - viewport.clientWidth);
      const travel = overflow / 32 * 1000;
      text.style.transform = `translateX(${-Math.min(overflow, Math.max(0, elapsed - 1400) * .032)}px)`;
      if (elapsed > 3000 + travel) {
        if (items.length > 1) choose((index + 1) % items.length, true);
        else elapsed = 0;
      }
    }
  };
  const enter = (): void => { hovered = true; };
  const leave = (): void => { hovered = false; };
  const focus = (): void => { focused = true; };
  const blur = (): void => { focused = false; };
  viewport.addEventListener('pointerenter', enter); viewport.addEventListener('pointerleave', leave);
  viewport.addEventListener('focus', focus); viewport.addEventListener('blur', blur);
  const timer = setInterval(tick, 40);
  return {
    update(entries, receiver, service, source): void {
      const languageChanged = language !== getLang(); language = getLang();
      const nextMode = receiver?.confirmed.processors[0]?.mode ?? '';
      const paused = receiver?.coverage?.state === 'out_of_band';
      const logicalIdentity = `${service}:${receiver?.receiver_uid}:${nextMode}:`;
      const nextIdentity = paused && identity.startsWith(logicalIdentity) ? identity
        : `${logicalIdentity}${receiver?.stream?.epoch}:${receiver?.stream?.channel_id}:${receiver?.processing_runtime?.processing_generation ?? ''}:${source ?? ''}`;
      const reset = identity !== nextIdentity;
      if (reset) {
        cancelMotion(); identity = nextIdentity; mode = nextMode; continuous = continuousModes.has(mode);
        items = []; index = 0; target = ''; glyphs = []; written = 0; write(''); text.style.transform = ''; elapsed = 0; wasActive = false;
      }
      receiving = !!receiver && !paused && receiver.state === 'running' && !receiver.fault && receiver.processing_state !== 'failed'
        && (!receiver.reception || receiver.reception.state === 'receiving');
      const historicalStream = paused ? entries.find(entry => entry.mode === nextMode && entry.timing?.baseband_stream)?.timing?.baseband_stream : undefined;
      const retained = receiver && paused && !receiver.stream && historicalStream ? { ...receiver, stream: historicalStream } as ReceiverState : receiver;
      const next = retained && (receiving || paused) ? stripMessages(entries, retained, paused ? undefined : source) : [];
      if (paused) { cancelMotion(); if (continuous && target) { written = glyphs.length; write(target); } }
      host.hidden = !receiver || !textModes.has(mode) && !next.length;
      root.classList.toggle('mc-has-strip-text', !host.hidden);
      host.dataset.presentation = continuous ? 'typing' : 'messages';
      badge.textContent = mode.toUpperCase();
      const waiting = paused ? getLang() === 'zh' ? '带外 · 等待覆盖' : 'Out of band · waiting for coverage'
        : getLang() === 'zh' ? receiving ? '等待解码文字…' : '接收已暂停' : receiving ? 'Waiting for decoded text…' : 'Reception paused';
      viewport.setAttribute('aria-label', getLang() === 'zh' ? '解码文字；悬停或聚焦暂停滚动' : 'Decoded text; hover or focus to pause');
      if (!next.length) {
        cancelMotion(); items = []; target = ''; glyphs = []; written = 0; write(waiting); text.style.transform = ''; viewport.title = waiting; return;
      }
      if (items.length === next.length && items.every((item, i) => item.key === next[i].key && item.text === next[i].text)) { if (languageChanged && !continuous) choose(index, false); return; }
      const previous = items, oldKey = items[index]?.key; items = next;
      if (continuous) {
        const nextText = items.map(item => item.text).join('');
        const extendsPrevious = !!target && nextText.startsWith(target);
        glyphs = segmenter ? [...segmenter.segment(nextText)].map(part => part.segment) : Array.from(nextText);
        target = nextText;
        if (!extendsPrevious || paused) { written = glyphs.length; write(target); }
        // The next frame types only newly received graphemes, keeping the live tail in view.
      } else {
        const existing = items.findIndex(item => item.key === oldKey);
        if (!previous.length || existing < 0) choose(Math.max(0, items.length - 1), false);
        else if (previous[index]?.text !== items[existing].text || languageChanged) choose(existing, false);
        else index = existing;
      }
      lastTick = performance.now();
    },
    dispose(): void {
      disposed = true; clearInterval(timer); cancelMotion();
      viewport.removeEventListener('pointerenter', enter); viewport.removeEventListener('pointerleave', leave);
      viewport.removeEventListener('focus', focus); viewport.removeEventListener('blur', blur);
      host.replaceChildren(); root.classList.remove('mc-has-strip-text');
    },
  };
}
