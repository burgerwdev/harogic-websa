import { workbenchIcon } from './workbenchIcons';
import { getLang } from '../core/i18n';
import { getUiScale } from '../core/uiScale';
import { createSurfacePortal, type SurfacePortal } from './surfacePortal';
import { applyFloatingGeometry, clampFloatingGeometry, type Geometry } from './floatingGeometry';

export interface FloatingLayout { geometry: Geometry; hidden: boolean; collapsed: boolean }
export interface FloatingWindow {
  root: HTMLElement; body: HTMLElement; portal: SurfacePortal;
  show(): void; hide(): void; recall(reset?: boolean): void; sync(): void; dispose(): void;
  geometry(): Geometry; setGeometry(geometry: Partial<Geometry>): void;
  setCollapsed(collapsed: boolean): void; setDockHandler(handler: (() => void) | undefined): void;
}
let top = 50;
/** Decode's viewport-pixel geometry convention, reusable without global DOM queries. */
export function createFloatingWindow(id: string, title: () => string, initial: Geometry,
  onChange?: () => void): FloatingWindow {
  const doc = document, parent = window;
  const root = doc.createElement('section'); root.className = 'mc-window'; root.dataset.viewId = id;
  root.setAttribute('role', 'dialog'); root.tabIndex = -1;
  root.innerHTML = '<header class="mc-head"><strong></strong><div class="mc-window-actions"><button data-collapse>−</button><button data-pop>Pop out</button><button data-dock hidden>Dock</button><button data-close>×</button></div></header><p data-popup-error role="alert" hidden></p><div class="mc-body"></div>';
  const head = root.querySelector<HTMLElement>('.mc-head')!, body = root.querySelector<HTMLElement>('.mc-body')!;
  const key = `websa-workbench-layout:${id}`;
  let dockHandler: (() => void) | undefined;
  let layout: FloatingLayout = { geometry: { ...initial }, hidden: false, collapsed: false };
  try {
    const stored = JSON.parse(localStorage.getItem(key) ?? 'null') as Partial<FloatingLayout> | null;
    const geometry = stored?.geometry;
    if (stored && typeof stored === 'object' && !Array.isArray(stored)
        && geometry && typeof geometry === 'object' && !Array.isArray(geometry)
        && [geometry.x, geometry.y, geometry.w, geometry.h].every(Number.isFinite)
        && typeof stored.hidden === 'boolean' && typeof stored.collapsed === 'boolean') {
      layout = { geometry: { x: geometry.x, y: geometry.y, w: geometry.w, h: geometry.h },
        hidden: stored.hidden, collapsed: stored.collapsed };
    }
  } catch { /* disabled or invalid storage */ }
  const save = (): void => { try { localStorage.setItem(key, JSON.stringify(layout)); } catch { /* session only */ } };
  const clamp = (g: Geometry): Geometry => {
    const bounded = clampFloatingGeometry(g, parent, { w: 300 * getUiScale(), h: 160 });
    // Keep the close/Dock controls reachable after shrinking the host viewport.
    bounded.x = Math.min(bounded.x, Math.max(0, parent.innerWidth - bounded.w));
    const height = layout.collapsed ? root.getBoundingClientRect().height : bounded.h;
    bounded.y = Math.min(Math.max(0, g.y), Math.max(0, parent.innerHeight - height));
    return bounded;
  };
  const apply = (): void => {
    root.style.zoom = String(getUiScale());
    root.classList.toggle('mc-collapsed', layout.collapsed); body.hidden = layout.collapsed;
    root.hidden = layout.hidden;
    if (!portal.isPoppedOut()) {
      // Measure the collapsed strip at its new width/scale, preserving the
      // expanded height solely for the next expansion.
      if (layout.collapsed) {
        root.style.width = `${clampFloatingGeometry(layout.geometry, parent, { w: 300 * getUiScale(), h: 160 }).w / getUiScale()}px`;
        root.style.height = 'auto';
      }
      layout.geometry = clamp(layout.geometry); const g = layout.geometry, s = getUiScale();
      applyFloatingGeometry(root, g, s);
      if (layout.collapsed) root.style.height = 'auto';
    }
    root.hidden = layout.hidden; root.setAttribute('aria-label', title());
    head.querySelector('strong')!.textContent = title();
    const zh = getLang() === 'zh';
    for (const [selector, text, tip] of [
      ['[data-collapse]', layout.collapsed ? '+' : '−', zh ? layout.collapsed ? '展开窗口内容' : '收起窗口内容；接收和录制继续' : layout.collapsed ? 'Expand window contents' : 'Collapse contents; reception and recording continue'],
      ['[data-pop]', zh ? '弹出' : 'Pop out', zh ? '移到独立桌面窗口；接收和录制继续' : 'Move this view to a separate desktop window'],
      ['[data-dock]', zh ? '停靠' : 'Dock', zh ? '将此视图移回页面内' : 'Return this view to the main page'],
      ['[data-close]', '×', zh ? '隐藏此窗口；接收和录制继续' : 'Hide this view; reception and recording continue'],
    ]) { const control = head.querySelector<HTMLButtonElement>(selector)!; const symbol = selector === '[data-pop]' ? 'external' : selector === '[data-dock]' ? 'dock'
      : selector === '[data-close]' ? 'close' : layout.collapsed ? 'expand' : 'minus';
      control.innerHTML = `${workbenchIcon(symbol)}<span>${text}</span>`; control.title = tip; control.setAttribute('aria-label', tip); }
    if (dockHandler) {
      const dock = head.querySelector<HTMLButtonElement>('[data-dock]')!;
      dock.hidden = false; dock.title = zh ? '放回信道概览' : 'Return to channel overview';
      dock.setAttribute('aria-label', dock.title);
    } else head.querySelector<HTMLButtonElement>('[data-dock]')!.hidden = !portal.isPoppedOut();
  };
  let gesture: { start: Geometry; px: number; py: number; direction: string } | null = null;
  const portal = createSurfacePortal(root, { title, beforeMove: () => { gesture = null; },
    afterMove: () => { apply(); onChange?.(); }, popSelector: '[data-pop]', dockSelector: '[data-dock]', errorSelector: '[data-popup-error]' });
  const front = (): void => { root.style.zIndex = String(++top); };
  const show = (): void => { layout.hidden = false; apply(); save(); front(); root.focus(); onChange?.(); };
  const hide = (): void => { portal.dock(); layout.hidden = true; apply(); save(); onChange?.(); };
  root.querySelector('[data-close]')!.addEventListener('click', hide);
  root.querySelector('[data-pop]')!.addEventListener('click', () => portal.popout());
  root.querySelector('[data-dock]')!.addEventListener('click', () => dockHandler ? dockHandler() : portal.dock());
  root.querySelector('[data-collapse]')!.addEventListener('click', () => { layout.collapsed = !layout.collapsed; apply(); save(); });
  root.addEventListener('pointerdown', front);
  root.addEventListener('keydown', event => {
    if ((event.target as HTMLElement).matches('input,select,textarea')) return;
    if (event.key === 'Escape') { hide(); event.stopPropagation(); }
  });
  const wire = (handle: HTMLElement, direction: string): void => {
    handle.addEventListener('pointerdown', event => {
      if (portal.isPoppedOut() || (event.target as HTMLElement).closest('button,input,select,[data-no-drag]')) return;
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
        if (d.includes('e')) next.w = Math.max(300 * getUiScale(), g.w + dx);
        if (d.includes('s')) next.h = Math.max(160, g.h + dy);
        if (d.includes('w')) { next.w = Math.max(300 * getUiScale(), g.w - dx); next.x = g.x + g.w - next.w; }
        if (d.includes('n')) { next.h = Math.max(160, g.h - dy); next.y = g.y + g.h - next.h; }
      }
      layout.geometry = next; apply();
    });
    const stop = (): void => { gesture = null; save(); };
    handle.addEventListener('pointerup', stop); handle.addEventListener('pointercancel', stop);
    handle.addEventListener('lostpointercapture', stop);
  };
  wire(head, 'move');
  for (const d of ['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw']) {
    const h = doc.createElement('i'); h.dataset.resize = d; h.className = `mc-resize mc-resize-${d}`;
    if (d === 'e' || d === 'w') h.title = getLang() === 'zh' ? '拖动调整窗口宽度' : 'Drag to resize window width';
    root.appendChild(h); wire(h, d);
  }
  const resize = (): void => { apply(); save(); };
  let resizeFrame: number | null = null;
  // Defer geometry writes until after ResizeObserver delivery. Inline name
  // editing changes strip height and must not trigger a same-frame resize loop.
  const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(() => {
    if (layout.collapsed && !layout.hidden && !portal.isPoppedOut() && resizeFrame === null)
      resizeFrame = parent.requestAnimationFrame(() => { resizeFrame = null; if (layout.collapsed && !layout.hidden && !portal.isPoppedOut()) resize(); });
  });
  observer?.observe(root);
  parent.addEventListener('resize', resize); doc.body.appendChild(root); apply(); front();
  return { root, body, portal, show, hide, sync: () => portal.sync(),
    geometry: () => ({ ...layout.geometry }),
    setGeometry: geometry => { layout.geometry = { ...layout.geometry, ...geometry }; apply(); save(); },
    setCollapsed: collapsed => { layout.collapsed = collapsed; apply(); save(); },
    setDockHandler: handler => { dockHandler = handler; apply(); },
    recall: (reset = false) => { portal.dock(); if (reset) { layout.geometry = { ...initial }; layout.collapsed = false; } show(); },
    dispose: () => { observer?.disconnect(); if (resizeFrame !== null) parent.cancelAnimationFrame(resizeFrame); portal.dispose(); parent.removeEventListener('resize', resize); root.remove(); } };
}
