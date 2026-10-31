import { getUiScale } from '../core/uiScale';
import { createSurfacePortal, type SurfacePortal } from './surfacePortal';
import { applyFloatingGeometry, clampFloatingGeometry, type Geometry } from './floatingGeometry';

export interface FloatingLayout { geometry: Geometry; hidden: boolean; collapsed: boolean }
export interface FloatingWindow {
  root: HTMLElement; body: HTMLElement; portal: SurfacePortal;
  show(): void; hide(): void; recall(reset?: boolean): void; sync(): void; dispose(): void;
}
let top = 50;
/** Decode's viewport-pixel geometry convention, reusable without global DOM queries. */
export function createFloatingWindow(id: string, title: () => string, initial: Geometry,
  onChange?: () => void): FloatingWindow {
  const doc = document, parent = window;
  const root = doc.createElement('section'); root.className = 'mc-window'; root.dataset.viewId = id;
  root.setAttribute('role', 'dialog'); root.tabIndex = -1;
  root.innerHTML = '<header class="mc-head"><strong></strong><button data-collapse>−</button><button data-pop>Pop out</button><button data-dock hidden>Dock</button><button data-close>×</button></header><p data-popup-error role="alert" hidden></p><div class="mc-body"></div>';
  const head = root.querySelector<HTMLElement>('.mc-head')!, body = root.querySelector<HTMLElement>('.mc-body')!;
  const key = `websa-workbench-layout:${id}`;
  let layout: FloatingLayout = { geometry: { ...initial }, hidden: false, collapsed: false };
  try {
    const stored = JSON.parse(localStorage.getItem(key) ?? 'null') as FloatingLayout | null;
    if (stored && Object.values(stored.geometry).every(Number.isFinite)) layout = stored;
  } catch { /* disabled or invalid storage */ }
  const save = (): void => { try { localStorage.setItem(key, JSON.stringify(layout)); } catch { /* session only */ } };
  const clamp = (g: Geometry): Geometry => {
    const bounded = clampFloatingGeometry(g, parent, { w: 300, h: 160 });
    // Keep the close/Dock controls reachable after shrinking the host viewport.
    bounded.x = Math.min(bounded.x, Math.max(0, parent.innerWidth - bounded.w));
    bounded.y = Math.min(bounded.y, Math.max(0, parent.innerHeight - bounded.h));
    return bounded;
  };
  const apply = (): void => {
    root.style.zoom = String(getUiScale());
    if (!portal.isPoppedOut()) {
      layout.geometry = clamp(layout.geometry); const g = layout.geometry, s = getUiScale();
      applyFloatingGeometry(root, g, s);
      if (layout.collapsed) root.style.height = 'auto';
    }
    root.classList.toggle('mc-collapsed', layout.collapsed); body.hidden = layout.collapsed;
    root.hidden = layout.hidden; root.setAttribute('aria-label', title());
    head.querySelector('strong')!.textContent = title();
  };
  let gesture: { start: Geometry; px: number; py: number; direction: string } | null = null;
  const portal = createSurfacePortal(root, { title, beforeMove: () => { gesture = null; },
    afterMove: () => { apply(); onChange?.(); }, popSelector: '[data-pop]', dockSelector: '[data-dock]', errorSelector: '[data-popup-error]' });
  const front = (): void => { root.style.zIndex = String(++top); };
  const show = (): void => { layout.hidden = false; apply(); save(); front(); root.focus(); };
  const hide = (): void => { portal.dock(); layout.hidden = true; apply(); save(); };
  root.querySelector('[data-close]')!.addEventListener('click', hide);
  root.querySelector('[data-pop]')!.addEventListener('click', () => portal.popout());
  root.querySelector('[data-dock]')!.addEventListener('click', () => portal.dock());
  root.querySelector('[data-collapse]')!.addEventListener('click', () => { layout.collapsed = !layout.collapsed; apply(); save(); });
  root.addEventListener('pointerdown', front);
  root.addEventListener('keydown', event => {
    if ((event.target as HTMLElement).matches('input,select,textarea')) return;
    if (event.key === 'Escape') { hide(); event.stopPropagation(); }
  });
  const wire = (handle: HTMLElement, direction: string): void => {
    handle.addEventListener('pointerdown', event => {
      if (portal.isPoppedOut() || (event.target as HTMLElement).closest('button,input,select')) return;
      gesture = { start: { ...layout.geometry }, px: event.clientX, py: event.clientY, direction };
      try { handle.setPointerCapture(event.pointerId); } catch { /* synthetic pointer */ }
      event.preventDefault();
    });
    handle.addEventListener('pointermove', event => {
      if (!gesture) return;
      const { start: g, px, py, direction: d } = gesture;
      const dx = event.clientX - px, dy = event.clientY - py; const next = { ...g };
      if (d === 'move') { next.x += dx; next.y += dy; }
      else {
        if (d.includes('e')) next.w = Math.max(300, g.w + dx);
        if (d.includes('s')) next.h = Math.max(160, g.h + dy);
        if (d.includes('w')) { next.w = Math.max(300, g.w - dx); next.x = g.x + g.w - next.w; }
        if (d.includes('n')) { next.h = Math.max(160, g.h - dy); next.y = g.y + g.h - next.h; }
      }
      layout.geometry = clamp(next); apply();
    });
    const stop = (): void => { gesture = null; save(); };
    handle.addEventListener('pointerup', stop); handle.addEventListener('pointercancel', stop);
    handle.addEventListener('lostpointercapture', stop);
  };
  wire(head, 'move');
  for (const d of ['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw']) {
    const h = doc.createElement('i'); h.dataset.resize = d; h.className = `mc-resize mc-resize-${d}`;
    root.appendChild(h); wire(h, d);
  }
  const resize = (): void => { apply(); save(); };
  parent.addEventListener('resize', resize); doc.body.appendChild(root); apply(); front();
  return { root, body, portal, show, hide, sync: () => portal.sync(),
    recall: (reset = false) => { portal.dock(); if (reset) { layout.geometry = { ...initial }; layout.collapsed = false; } show(); },
    dispose: () => { portal.dispose(); parent.removeEventListener('resize', resize); root.remove(); } };
}
