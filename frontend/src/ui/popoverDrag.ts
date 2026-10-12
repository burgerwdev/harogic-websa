// Drag any detail popover by its head bar (user request: gnss / refclk / device).
//
// Pointer-based: press the head, move, release. Positions are written as inline
// left/top (bottom/right cleared) so they survive the STATUS handler re-filling the
// rows — re-renders touch text content, never the popover's position. The Close
// button inside the head stays an ordinary click.

const DRAG_POPovers = ['gnss-popover', 'refclk-popover', 'device-popover'];

export function enablePopoverDrag(popoverId: string): void {
  const pop = document.getElementById(popoverId);
  const head = pop?.querySelector('.gnss-popover-head') as HTMLElement | null;
  if (!pop || !head) return;
  head.style.cursor = 'move';
  head.addEventListener('pointerdown', (ev: PointerEvent) => {
    if ((ev.target as HTMLElement).closest('button')) return;
    ev.preventDefault();
    // offsetLeft/offsetTop are relative to the positioned .info-item ancestor, which is
    // also what the stylesheet's top/left anchor against — so dragging just adds deltas.
    const startLeft = pop.offsetLeft;
    const startTop = pop.offsetTop;
    const sx = ev.clientX;
    const sy = ev.clientY;
    const move = (e: PointerEvent) => {
      pop.style.bottom = 'auto';           // inline top would fight the stylesheet's bottom
      pop.style.right = 'auto';
      pop.style.left = `${startLeft + e.clientX - sx}px`;
      pop.style.top = `${startTop + e.clientY - sy}px`;
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  });
}

export function initPopoverDrag(): void {
  for (const id of DRAG_POPovers) enablePopoverDrag(id);
}
