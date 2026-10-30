import { applyI18n, getLang, onLangChange, t } from '../core/i18n';
import { getTheme, onThemeChange } from '../core/theme';
import { getUiScale, onUiScaleChange } from '../core/uiScale';

export interface DecodePopout {
  isPoppedOut(): boolean;
  popout(): void;
  dock(): void;
}
let synchronize: (() => void) | null = null;
onLangChange(() => synchronize?.());
onThemeChange(() => synchronize?.());
onUiScaleChange(() => synchronize?.());

/** Move one live surface. Its application, sockets and subscriptions stay in the opener. */
export function createDecodePopout(root: HTMLElement, beforeMove: () => void, afterMove: () => void): DecodePopout {
  const openerDocument = root.ownerDocument;
  const opener = openerDocument.defaultView!;
  let child: Window | null = null;
  let placeholder: Comment | null = null;
  let ending = false;
  let blocked = false;
  let stylesheetObserver: MutationObserver | null = null;
  const update = (): void => {
    const doc = root.ownerDocument;
    if (child && !child.closed) {
      doc.documentElement.dataset.theme = getTheme();
      doc.documentElement.lang = getLang() === 'zh' ? 'zh-CN' : 'en';
      doc.title = t('decode_popup_title');
      root.style.zoom = String(getUiScale());
      doc.documentElement.style.setProperty('--decode-scale', String(getUiScale()));
      doc.documentElement.style.setProperty('--ui', String(getUiScale()));
      doc.documentElement.dataset.uiScale = String(getUiScale());
    }
    applyI18n(root);
    const pop = root.querySelector<HTMLButtonElement>('#btn-decode-popout');
    const dock = root.querySelector<HTMLButtonElement>('#btn-decode-dock');
    if (pop) pop.hidden = child !== null;
    if (dock) dock.hidden = child === null;
    const error = root.querySelector<HTMLElement>('#decode-popout-error');
    if (error) { error.hidden = !blocked; error.textContent = blocked ? t('decode_popup_blocked') : ''; }
  };
  const dock = (): void => {
    if (!child || ending) return;
    beforeMove();
    const previous = child;
    child = null;
    stylesheetObserver?.disconnect();
    stylesheetObserver = null;
    root.classList.remove('decode-window-popout');
    if (placeholder?.parentNode) placeholder.replaceWith(root);
    else openerDocument.body.appendChild(root);
    placeholder = null;
    if (!previous.closed) previous.close();
    update();
    afterMove();
  };
  const popout = (): void => {
    if (child && !child.closed) { child.focus(); return; }
    const next = opener.open('', '', 'popup=yes,width=800,height=600');
    if (!next) { blocked = true; update(); return; }
    blocked = false;
    beforeMove();
    child = next;
    const doc = next.document;
    doc.head.replaceChildren();
    doc.body.replaceChildren();
    const base = doc.createElement('base');
    base.href = openerDocument.baseURI;
    doc.head.appendChild(base);
    const copyStyles = (): void => {
      doc.head.querySelectorAll('[data-decode-style]').forEach(node => node.remove());
      for (const source of openerDocument.querySelectorAll('link[rel="stylesheet"], style')) {
        const copy = source.cloneNode(true) as HTMLElement;
        copy.dataset.decodeStyle = '';
        if (source.tagName === 'LINK') (copy as HTMLLinkElement).href = (source as HTMLLinkElement).href;
        doc.head.appendChild(copy);
      }
    };
    copyStyles();
    stylesheetObserver = new MutationObserver(copyStyles);
    stylesheetObserver.observe(openerDocument.head, { childList: true, subtree: true, characterData: true });
    doc.body.className = 'decode-popup-body';
    placeholder = openerDocument.createComment('Decode surface returns here');
    root.replaceWith(placeholder);
    doc.body.appendChild(root);
    root.classList.add('decode-window-popout');
    // beforeunload runs while the same-origin document is still available for adoption.
    next.addEventListener('beforeunload', dock);
    next.addEventListener('pagehide', dock);
    update();
    afterMove();
    next.focus();
  };
  synchronize = () => { update(); afterMove(); };
  opener.addEventListener('pagehide', () => {
    ending = true;
    stylesheetObserver?.disconnect();
    child?.close();
    child = null;
  });
  return { isPoppedOut: () => child !== null, popout, dock };
}
