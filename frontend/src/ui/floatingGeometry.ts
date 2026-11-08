/** Geometry is always in viewport pixels, even when CSS zoom changes local pixels. */
export interface Geometry { x: number; y: number; w: number; h: number }
export function clampFloatingGeometry(g: Geometry, viewport: { innerWidth: number; innerHeight: number },
  minimum = { w: 260, h: 120 }): Geometry {
  return { x: Math.min(Math.max(0, g.x), Math.max(0, viewport.innerWidth - 120)),
    y: Math.min(Math.max(0, g.y), Math.max(0, viewport.innerHeight - 40)),
    w: Math.min(Math.max(minimum.w, g.w), Math.max(minimum.w, viewport.innerWidth)),
    h: Math.min(Math.max(minimum.h, g.h), Math.max(minimum.h, viewport.innerHeight)) };
}
export function applyFloatingGeometry(root: HTMLElement, g: Geometry, scale: number): void {
  root.style.left = `${g.x / scale}px`; root.style.top = `${g.y / scale}px`;
  root.style.width = `${g.w / scale}px`; root.style.height = `${g.h / scale}px`;
}
