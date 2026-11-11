import { getLang } from '../core/i18n';
import { currentGraphMode } from './graphMode';
import { getCapture, getView } from './spectrumViewport';
import { plotRect } from '../render/plot';
import { requestRender } from '../render/redraw';
import { sdrIfbw, sdrListenHz } from './sdrState';
import { getUiScale } from '../core/uiScale';

export interface IdentifyBand { low_hz: number; high_hz: number }
export interface IdentifyRequest extends IdentifyBand { origin: 'spectrum' | 'waterfall' | 'manual'; capture_epoch: string }
const l = (en: string, zh: string) => getLang() === 'zh' ? zh : en;
let source = '', captureEpoch = '', bin = 1000, candidates: IdentifyBand[] = [];
let highlight: IdentifyBand | null = null;
let draft: { band: IdentifyBand; focus: HTMLElement | null; source: string; origin: IdentifyRequest['origin']; pointer?: number; surface?: HTMLCanvasElement; x?: number; startHz?: number; view?: { lo: number; hi: number }; rect?: DOMRect } | null = null;
let menu: HTMLElement | null = null, menuAnchor: HTMLCanvasElement | null = null;
const changed = () => { requestRender(); document.dispatchEvent(new CustomEvent('websa:identify-selection', { detail: !!draft })); };
export function identifySelecting(): boolean { return draft !== null; }
export function identifyBand(): IdentifyBand | null { return draft?.band ?? highlight; }
export function highlightIdentifyBand(band: IdentifyBand | null): void { highlight = band; requestRender(); }
export function setIdentifySource(identity: string, epoch: string, spacing: number, rows?: IdentifyBand[]): void {
  if (source && source !== identity) { cancelIdentifySelection(); closeMenu(); highlight = null; candidates = []; }
  source = identity; captureEpoch = epoch; bin = spacing || 1000; if (rows) candidates = rows;
}
function at(hz: number): IdentifyBand { const width = Math.max(sdrIfbw.get(), bin * 8); return { low_hz: hz - width / 2, high_hz: hz + width / 2 }; }
function closeMenu(restore = false): void { const anchor = menu ? menuAnchor : null; menu?.remove(); menu = null; menuAnchor = null; if (restore) anchor?.focus({ preventScroll: true }); }
export function beginIdentifySelection(origin: IdentifyRequest['origin'] = 'manual'): void {
  closeMenu(true); cancelIdentifySelection();
  draft = { band: at(sdrListenHz.get()), focus: document.activeElement as HTMLElement | null, source, origin };
  const canvas = document.querySelector<HTMLCanvasElement>('#spectrum');
  canvas?.focus({ preventScroll: true }); changed();
}
export function cancelIdentifySelection(): boolean {
  if (!draft) return false;
  const previous = draft; draft = null;
  if (previous.pointer !== undefined) { try { previous.surface?.releasePointerCapture(previous.pointer); } catch { /* ended pointer */ } }
  changed(); previous.focus?.focus({ preventScroll: true }); return true;
}
function submit(band: IdentifyBand, origin: IdentifyRequest['origin']): void {
  const valid = !draft || draft.source === source;
  const previous = draft; draft = null;
  if (previous?.pointer !== undefined) { try { previous.surface?.releasePointerCapture(previous.pointer); } catch { /* ended pointer */ } }
  changed(); if (!valid) return;
  highlight = band;
  document.dispatchEvent(new CustomEvent<IdentifyRequest>('websa:identify-region', { detail: { ...band, origin, capture_epoch: captureEpoch } }));
}
function surfaceFor(target: EventTarget | null): HTMLCanvasElement | null {
  if (!(target instanceof HTMLElement)) return null;
  const node = target.closest<HTMLCanvasElement>('canvas');
  return node && (node === document.querySelector<HTMLCanvasElement>('#spectrum') || node === document.querySelector<HTMLCanvasElement>('#waterfall')) ? node : null;
}
function geometry(surface: HTMLCanvasElement, event: MouseEvent): { view: { lo: number; hi: number }; rect: DOMRect; hz: number } | null {
  const view = getView() ?? getCapture(); if (!view) return null;
  const rect = surface.getBoundingClientRect(), p = plotRect();
  const wf = surface === document.querySelector<HTMLCanvasElement>('#waterfall');
  const left = wf ? rect.left : rect.left + p.x * getUiScale();
  const width = wf ? rect.width : p.w * getUiScale();
  const top = wf ? rect.top : rect.top + p.y * getUiScale();
  const height = wf ? rect.height : p.h * getUiScale();
  if (event.clientX < left || event.clientX > left + width || event.clientY < top || event.clientY > top + height) return null;
  const plot = new DOMRect(left, top, width, height);
  return { view: { ...view }, rect: plot, hz: view.lo + (event.clientX - left) / width * (view.hi - view.lo) };
}
function owned(event: Event): void { event.preventDefault(); event.stopImmediatePropagation(); }
export function initIdentifySelection(): void {
  document.addEventListener('pointerdown', event => {
    const surface = surfaceFor(event.target);
    if (menu && !(event.target as HTMLElement).closest('.sdr-identify-menu')) closeMenu();
    if (currentGraphMode() !== 'sdr') { cancelIdentifySelection(); return; }
    if (!surface) { if (draft && !(event.target as HTMLElement).closest('[data-scan-select]')) cancelIdentifySelection(); return; }
    if (!draft) { if (event.button === 2 && !event.shiftKey) owned(event); return; }
    owned(event); if (event.button !== 0 || draft.pointer !== undefined) return;
    const g = geometry(surface, event); if (!g) return;
    draft = { ...draft, origin: surface === document.querySelector<HTMLCanvasElement>('#waterfall') ? 'waterfall' : 'spectrum', pointer: event.pointerId, surface, x: event.clientX, startHz: g.hz, view: g.view, rect: g.rect, band: at(g.hz) };
    surface.setPointerCapture?.(event.pointerId); changed();
  }, true);
  document.addEventListener('pointermove', event => {
    if (!draft || !surfaceFor(event.target) && draft.pointer === undefined) return;
    owned(event); if (draft.pointer !== event.pointerId || !draft.rect || !draft.view || draft.startHz === undefined) return;
    const hz = draft.view.lo + (Math.max(draft.rect.left, Math.min(event.clientX, draft.rect.right)) - draft.rect.left) / draft.rect.width * (draft.view.hi - draft.view.lo);
    draft.band = Math.abs(event.clientX - draft.x!) < 8 ? at(hz) : { low_hz: Math.min(draft.startHz, hz), high_hz: Math.max(draft.startHz, hz) };
    changed();
  }, true);
  document.addEventListener('pointerup', event => {
    if (!draft) return;
    if (draft.pointer !== event.pointerId) { if (surfaceFor(event.target)) owned(event); return; }
    owned(event); submit(draft.band, draft.origin);
  }, true);
  document.addEventListener('pointercancel', event => { if (draft?.pointer === event.pointerId) { owned(event); cancelIdentifySelection(); } }, true);
  document.addEventListener('lostpointercapture', event => { if (draft?.pointer === (event as PointerEvent).pointerId) cancelIdentifySelection(); }, true);
  document.addEventListener('wheel', event => { if (draft && surfaceFor(event.target)) owned(event); }, { capture: true, passive: false });
  window.addEventListener('keydown', event => {
    if (event.key === 'Escape' && menu) { owned(event); closeMenu(true); return; }
    if (!draft) return;
    if (event.key === 'Escape') { owned(event); cancelIdentifySelection(); }
    else if (event.key === 'Enter') { owned(event); submit(draft.band, draft.origin); }
    else if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      owned(event); const delta = (event.key === 'ArrowLeft' ? -1 : 1) * Math.max(bin, (draft.band.high_hz - draft.band.low_hz) / 10);
      if (event.shiftKey) draft.band.high_hz = Math.max(draft.band.low_hz + bin * 2, draft.band.high_hz + delta);
      else { draft.band.low_hz += delta; draft.band.high_hz += delta; } changed();
    } else if ([' ', 'PageUp', 'PageDown'].includes(event.key)) owned(event);
    else if (!['Control','Shift','Alt','Meta'].includes(event.key)) cancelIdentifySelection();
  }, true);
  document.addEventListener('contextmenu', event => {
    const surface = surfaceFor(event.target); if (!surface || currentGraphMode() !== 'sdr' || event.shiftKey) return;
    const g = geometry(surface, event); if (!g) return; owned(event); closeMenu();
    const candidate = candidates.find(c => g.hz >= c.low_hz && g.hz <= c.high_hz), origin = surface === document.querySelector<HTMLCanvasElement>('#waterfall') ? 'waterfall' : 'spectrum';
    menuAnchor = surface; menu = document.createElement('div'); menu.className = 'sdr-identify-menu'; menu.setAttribute('role', 'menu');
    const direct = document.createElement('button'), select = document.createElement('button');
    direct.textContent = origin === 'waterfall' ? l('Identify current signal in this band', '识别此频段当前信号') : candidate ? l('Identify this signal', '识别此信号') : l('Identify here', '识别此处');
    select.textContent = l('Select region…', '选区识别…');
    for (const button of [direct, select]) { button.setAttribute('role', 'menuitem'); menu.append(button); }
    direct.onclick = () => { closeMenu(true); submit(candidate ?? at(g.hz), origin); };
    select.onclick = () => beginIdentifySelection(origin);
    document.body.append(menu); const r = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(0, Math.min(event.clientX, innerWidth - r.width))}px`;
    menu.style.top = `${Math.max(0, Math.min(event.clientY, innerHeight - r.height))}px`; direct.focus();
  }, true);
}
export function selectionHint(): string {
  return draft?.origin === 'waterfall' ? l('Current IQ · Drag band · Esc cancels', '当前信号 · 横向拖选 · Esc 取消') : l('Drag band · Release to identify · Esc cancels', '横向拖选 · 松手识别 · Esc 取消');
}
