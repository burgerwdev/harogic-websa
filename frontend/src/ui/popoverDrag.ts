// Drag any detail popover by its head bar (user request: gnss / refclk / device).
//
// Pointer-based: press the head, move, release. Positions are written as inline
// left/top (bottom/right cleared) so they survive the STATUS handler re-filling the
// rows — re-renders touch text content, never the popover's position. The Close
// button inside the head stays an ordinary click.

import { bindWindowOpacity } from './windowOpacity';

const DRAG_POPovers = ['gnss-popover', 'refclk-popover', 'device-popover'];

export function enablePopoverDrag(popoverId: string): void {
  const pop = document.getElementById(popoverId);
  const head = pop?.querySelector('.gnss-popover-head') as HTMLElement | null;
  if (!pop || !head) return;
  if (!head.querySelector('.window-opacity')) {
    const slider = document.createElement('input');
    slider.className = 'window-opacity';
    head.insertBefore(slider, head.querySelector('button'));
    bindWindowOpacity(pop, slider);
  }
  head.style.cursor = 'move';
  let stopDrag: (() => void) | null = null;
  head.addEventListener('pointerdown', (ev: PointerEvent) => {
    if (ev.button !== 0 || stopDrag || (ev.target as HTMLElement).closest('button,input,select,label,[data-no-drag]')) return;
    ev.preventDefault();
    // offsetLeft/offsetTop are relative to the positioned .info-item ancestor, which is
    // also what the stylesheet's top/left anchor against — so dragging just adds deltas.
    const startLeft = pop.offsetLeft;
    const startTop = pop.offsetTop;
    const sx = ev.clientX;
    const sy = ev.clientY;
    const move = (e: PointerEvent) => {
      if (e.pointerId !== ev.pointerId) return;
      pop.style.bottom = 'auto';           // inline top would fight the stylesheet's bottom
      pop.style.right = 'auto';
      pop.style.left = `${startLeft + e.clientX - sx}px`;
      pop.style.top = `${startTop + e.clientY - sy}px`;
    };
    const stop = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      window.removeEventListener('blur', stop);
      stopDrag = null;
    };
    const up = (e: PointerEvent) => { if (e.pointerId === ev.pointerId) stop(); };
    stopDrag = stop;
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
    window.addEventListener('blur', stop);
  });
}

export function initPopoverDrag(): void {
  for (const id of DRAG_POPovers) enablePopoverDrag(id);
}
