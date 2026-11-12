import { getTheme, onThemeChange } from '../core/theme';
import { surfacePortalDocuments } from './surfacePortal';

const NS = 'http://www.w3.org/2000/svg';
const surfaces = '.control-panel-surface, .info-bar, .dev-bar, .mc-window, .decode-window, .gnss-popover, .theme-picker, [data-glass-lens]';
let serial = 0;

function glassActive(): boolean {
  const theme = getTheme();
  return theme === 'liquid-glass' || theme === 'liquid-glass-dark';
}

/** A rounded lens displaces the scene most at its rim and leaves its centre flat.
 * The bounded map is rebuilt on geometry changes, never on spectrum frames. */
export function glassMap(width: number, height: number, radius: number) {
  const w = Math.min(512, Math.max(1, Math.ceil(width)));
  const h = Math.min(256, Math.max(1, Math.ceil(height)));
  const pixels = new Uint8ClampedArray(w * h * 4);
  const rim = Math.min(12, radius, height / 3);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const px = (x + .5) * width / w - width / 2;
    const py = (y + .5) * height / h - height / 2;
    const dx = px - Math.max(-width / 2 + radius, Math.min(width / 2 - radius, px));
    const dy = py - Math.max(-height / 2 + radius, Math.min(height / 2 - radius, py));
    const length = Math.hypot(dx, dy);
    const depth = length ? radius - length : rim;
    const bend = depth >= 0 && depth < rim ? (1 - depth / rim) ** 2 : 0;
    const at = (y * w + x) * 4;
    pixels[at] = 128 + (length ? dx / length : 0) * bend * 112;
    pixels[at + 1] = 128 + (length ? dy / length : 0) * bend * 112;
    pixels[at + 2] = 128;
    pixels[at + 3] = 255;
  }
  return { width: w, height: h, pixels, scale: rim * 1.8 };
}

class GlassDocument {
  readonly elements = new Map<HTMLElement, string>();
  private readonly svg: SVGSVGElement;
  private readonly filters = new Map<string, { node: SVGFilterElement; users: Set<HTMLElement> }>();
  private readonly resize: ResizeObserver;
  private readonly changes: MutationObserver;
  private lightFrame = 0;
  private lightTarget: { element: HTMLElement; x: number; y: number } | null = null;

  constructor(readonly doc: Document, schedule: () => void) {
    this.svg = doc.createElementNS(NS, 'svg');
    this.svg.classList.add('liquid-glass-defs');
    this.svg.setAttribute('width', '0'); this.svg.setAttribute('height', '0');
    this.svg.setAttribute('aria-hidden', 'true');
    doc.body.append(this.svg);
    this.resize = new ResizeObserver(entries => entries.forEach(entry => this.update(entry.target as HTMLElement)));
    this.changes = new MutationObserver(records => {
      if (records.some(record => !(record.target as Element).closest?.('.liquid-glass-defs')
        && [...record.addedNodes, ...record.removedNodes].some(node => node.nodeType === 1))) schedule();
    });
    this.changes.observe(doc.body, { childList: true, subtree: true });
    doc.addEventListener('pointermove', this.move, { passive: true });
  }

  private move = (event: PointerEvent) => {
    if (this.doc.defaultView?.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    const element = (event.target as Element).closest?.<HTMLElement>('.liquid-panel');
    if (!element) return;
    this.lightTarget = { element, x: event.clientX, y: event.clientY };
    if (!this.lightFrame) this.lightFrame = requestAnimationFrame(() => {
      this.lightFrame = 0;
      const target = this.lightTarget;
      if (!target?.element.isConnected) return;
      const rect = target.element.getBoundingClientRect();
      const angle = Math.atan2(target.y - rect.y - rect.height / 2, target.x - rect.x - rect.width / 2) * 180 / Math.PI;
      target.element.style.setProperty('--liquid-light-angle', `${angle + 90}deg`);
      target.element.style.setProperty('--liquid-light-x', `${(target.x - rect.x) / rect.width * 100}%`);
      target.element.style.setProperty('--liquid-light-y', `${(target.y - rect.y) / rect.height * 100}%`);
    });
  };

  add(element: HTMLElement) {
    if (this.elements.has(element)) return;
    this.elements.set(element, ''); element.classList.add('liquid-panel');
    this.resize.observe(element); this.update(element);
  }

  remove(element: HTMLElement) {
    this.release(element); this.elements.delete(element); this.resize.unobserve(element);
    element.classList.remove('liquid-panel');
    element.style.removeProperty('--liquid-refraction'); element.style.removeProperty('--liquid-light-angle');
    element.style.removeProperty('--liquid-light-x'); element.style.removeProperty('--liquid-light-y');
  }

  private release(element: HTMLElement) {
    const key = this.elements.get(element);
    if (!key) return;
    const filter = this.filters.get(key);
    filter?.users.delete(element);
    if (filter && !filter.users.size) { filter.node.remove(); this.filters.delete(key); }
    this.elements.set(element, '');
  }

  private update(element: HTMLElement) {
    if (element.ownerDocument !== this.doc || !element.isConnected) return;
    const width = element.offsetWidth, height = element.offsetHeight;
    if (!width || !height) { this.release(element); return; }
    const radius = Math.min(width / 2, height / 2, parseFloat(this.doc.defaultView!.getComputedStyle(element).borderTopLeftRadius) || 18);
    const key = `${width}:${height}:${radius}`;
    if (this.elements.get(element) === key) return;
    this.release(element);
    let filter = this.filters.get(key);
    if (!filter) {
      const map = glassMap(width, height, radius);
      const canvas = this.doc.createElement('canvas'); canvas.width = map.width; canvas.height = map.height;
      const ctx = canvas.getContext('2d');
      if (!ctx) return;
      const bitmap = ctx.createImageData(map.width, map.height); bitmap.data.set(map.pixels); ctx.putImageData(bitmap, 0, 0);
      const node = this.doc.createElementNS(NS, 'filter');
      node.id = `websa-glass-${++serial}`;
      for (const [name, value] of Object.entries({ filterUnits: 'userSpaceOnUse', x: '0', y: '0', width: String(width), height: String(height), 'color-interpolation-filters': 'sRGB' })) node.setAttribute(name, value);
      const image = this.doc.createElementNS(NS, 'feImage');
      for (const [name, value] of Object.entries({ href: canvas.toDataURL(), width: String(width), height: String(height), preserveAspectRatio: 'none', result: 'lens' })) image.setAttribute(name, value);
      const bend = this.doc.createElementNS(NS, 'feDisplacementMap');
      for (const [name, value] of Object.entries({ in: 'SourceGraphic', in2: 'lens', scale: String(map.scale), xChannelSelector: 'R', yChannelSelector: 'G' })) bend.setAttribute(name, value);
      node.append(image, bend); this.svg.append(node);
      filter = { node, users: new Set() }; this.filters.set(key, filter);
    }
    filter.users.add(element); this.elements.set(element, key);
    // Popouts carry a <base> pointing at the main page. Use their own document
    // URL explicitly so the lens never resolves to the parent document's SVG.
    const reference = new URL(this.doc.URL); reference.hash = filter.node.id;
    element.style.setProperty('--liquid-refraction', `url("${reference.href}")`);
  }

  dispose() {
    this.changes.disconnect(); this.resize.disconnect(); cancelAnimationFrame(this.lightFrame);
    this.doc.removeEventListener('pointermove', this.move);
    for (const element of this.elements.keys()) this.remove(element);
    this.svg.remove();
  }
}

/** Own each document's filters so live surfaces keep their optics when popped out. */
export function initLiquidGlass(): void {
  const documents = new Map<Document, GlassDocument>();
  let frame = 0;
  const stop = () => {
    cancelAnimationFrame(frame); frame = 0;
    documents.forEach(state => state.dispose()); documents.clear();
  };
  const reconcile = () => {
    frame = 0;
    if (!glassActive()) { stop(); return; }
    const needed = new Set<Document>([document, ...surfacePortalDocuments()]);
    documents.forEach(state => state.elements.forEach((_, element) => {
      if (element.isConnected && !element.ownerDocument.defaultView?.closed) needed.add(element.ownerDocument);
    }));
    for (const [doc, state] of documents) {
      if (doc.defaultView?.closed) { state.dispose(); documents.delete(doc); }
      else for (const element of state.elements.keys()) {
        if (!element.isConnected || element.ownerDocument !== doc) state.remove(element);
      }
    }
    for (const doc of needed) {
      let state = documents.get(doc);
      if (!state) { state = new GlassDocument(doc, schedule); documents.set(doc, state); }
      doc.querySelectorAll<HTMLElement>(surfaces).forEach(element => state!.add(element));
    }
  };
  const schedule = () => { if (!frame) frame = requestAnimationFrame(reconcile); };
  onThemeChange(() => glassActive() ? schedule() : stop());
  window.addEventListener('websa:surface-documents', () => { if (glassActive()) schedule(); });
  window.addEventListener('pagehide', stop);
  if (glassActive()) schedule();
}
