import { applyI18n, getLang, onLangChange, t } from '../core/i18n';
import { getTheme, onThemeChange } from '../core/theme';
import { getUiScale, onUiScaleChange } from '../core/uiScale';

export interface SurfacePortal {
  isPoppedOut(): boolean; popout(): void; dock(): void; sync(): void; dispose(): void;
}
interface Options {
  title(): string; beforeMove(): void; afterMove(): void;
  popSelector: string; dockSelector: string; errorSelector: string;
}
const synchronizers = new Set<() => void>();
const popupDocuments = new Set<Document>();
export function surfacePortalDocuments(): readonly Document[] { return [...popupDocuments]; }
onLangChange(() => synchronizers.forEach(fn => fn()));
onThemeChange(() => synchronizers.forEach(fn => fn()));
onUiScaleChange(() => synchronizers.forEach(fn => fn()));

/** Extracted from Decode: move live DOM; each surface owns its child and subscriptions. */
export function createSurfacePortal(root: HTMLElement, options: Options): SurfacePortal {
  const parentDocument = root.ownerDocument, parent = parentDocument.defaultView!;
  let child: Window | null = null, placeholder: Comment | null = null;
  let observer: MutationObserver | null = null;
  let ending = false, blocked = false;
  const sync = (): void => {
    const doc = root.ownerDocument;
    if (child && !child.closed) {
      doc.documentElement.dataset.theme = getTheme();
      doc.documentElement.lang = getLang() === 'zh' ? 'zh-CN' : 'en';
      doc.title = options.title();
      doc.documentElement.style.setProperty('--ui', String(getUiScale()));
      doc.documentElement.style.setProperty('--decode-scale', String(getUiScale()));
      doc.documentElement.dataset.uiScale = String(getUiScale());
      root.style.zoom = String(getUiScale());
    }
    applyI18n(root);
    const pop = root.querySelector<HTMLElement>(options.popSelector), dock = root.querySelector<HTMLElement>(options.dockSelector);
    if (pop) pop.hidden = child !== null;
    if (dock) dock.hidden = child === null;
    const error = root.querySelector<HTMLElement>(options.errorSelector);
    if (error) { error.hidden = !blocked; error.textContent = blocked ? t('decode_popup_blocked') : ''; }
    options.afterMove();
  };
  const dock = (): void => {
    if (!child || ending) return;
    options.beforeMove();
    popupDocuments.delete(root.ownerDocument);
    const previous = child; child = null;
    previous.removeEventListener('beforeunload', dock); previous.removeEventListener('pagehide', dock);
    observer?.disconnect(); observer = null;
    root.classList.remove('decode-window-popout', 'surface-popout');
    if (placeholder?.parentNode) placeholder.replaceWith(root); else parentDocument.body.appendChild(root);
    placeholder = null;
    if (!previous.closed) previous.close();
    sync(); parent.dispatchEvent(new Event('websa:surface-documents'));
  };
  const popout = (): void => {
    if (child && !child.closed) { child.focus(); return; }
    const next = parent.open('', '', 'popup=yes,width=840,height=620');
    if (!next) { blocked = true; sync(); return; }
    blocked = false; options.beforeMove(); child = next;
    const doc = next.document; doc.head.replaceChildren(); doc.body.replaceChildren();
    popupDocuments.add(doc);
    const base = doc.createElement('base'); base.href = parentDocument.baseURI; doc.head.appendChild(base);
    const copyStyles = (): void => {
      doc.head.querySelectorAll('[data-surface-style]').forEach(node => node.remove());
      for (const source of parentDocument.querySelectorAll('link[rel="stylesheet"], style')) {
        const copy = source.cloneNode(true) as HTMLElement; copy.dataset.surfaceStyle = '';
        if (source.tagName === 'LINK') (copy as HTMLLinkElement).href = (source as HTMLLinkElement).href;
        doc.head.appendChild(copy);
      }
    };
    copyStyles();
    observer = new MutationObserver(copyStyles);
    observer.observe(parentDocument.head, { childList: true, subtree: true, characterData: true });
    doc.body.className = 'decode-popup-body surface-popup-body';
    placeholder = parentDocument.createComment('Surface dock position'); root.replaceWith(placeholder);
    doc.body.appendChild(root); root.classList.add('decode-window-popout', 'surface-popout');
    next.addEventListener('beforeunload', dock); next.addEventListener('pagehide', dock);
    sync(); parent.dispatchEvent(new Event('websa:surface-documents')); next.focus();
  };
  const refresh = (): void => sync();
  const dispose = (): void => {
    if (ending) return;
    ending = true; observer?.disconnect(); synchronizers.delete(refresh);
    parent.removeEventListener('pagehide', dispose);
    if (child) {
      popupDocuments.delete(root.ownerDocument);
      child.removeEventListener('beforeunload', dock); child.removeEventListener('pagehide', dock);
      if (placeholder?.parentNode) placeholder.replaceWith(root);
      child.close(); child = null;
      parent.dispatchEvent(new Event('websa:surface-documents'));
    }
  };
  synchronizers.add(refresh); parent.addEventListener('pagehide', dispose);
  return { isPoppedOut: () => child !== null, popout, dock, sync, dispose };
}
