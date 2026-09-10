// Left-side jump rail: one click scrolls to a control group (expanding it when collapsed).
//
// Labels are taken from the localized group titles, so the rail needs no i18n keys of
// its own and follows a language switch automatically. Collapsing is delegated to the
// group's own toggle button, keeping a single owner for the collapse state.
import { hasKey, onLangChange, t } from '../core/i18n';
import { activeIndex } from './railMath';

interface RailItem { el: HTMLButtonElement; group: HTMLElement; }

let items: RailItem[] = [];
let scrollRaf = 0;
let lockedGroup: HTMLElement | null = null;   // keeps a jumped-to group highlighted
let lockUntil = 0;

function panel(): HTMLElement | null {
  return document.querySelector('.control-panel');
}

function railGroups(): HTMLElement[] {
  const p = panel();
  if (!p) return [];
  return Array.from(p.children).filter((e): e is HTMLElement => e.classList.contains('control-group'));
}

// Read the label from the dictionary, not from textContent: setLang() notifies
// listeners before applyI18n() rewrites the DOM, so copying the DOM here would lag
// one language switch behind.
function titleOf(group: HTMLElement): string {
  const el = group.querySelector('.group-title') as HTMLElement | null;
  const key = el?.dataset.i18n;
  if (key && hasKey(key)) return t(key);
  return (el?.textContent ?? '').trim();
}

function isCollapsed(group: HTMLElement): boolean {
  const body = group.querySelector('.group-body') as HTMLElement | null;
  return !!body && body.offsetParent === null;
}

/** Group tops in scroll coordinates, independent of the offsetParent chain. */
function groupTops(p: HTMLElement): number[] {
  const panelTop = p.getBoundingClientRect().top;
  return items.map((it) => it.group.getBoundingClientRect().top - panelTop + p.scrollTop);
}

function markActive(group: HTMLElement | null): void {
  items.forEach((it) => it.el.classList.toggle('active', it.group === group));
}

function jumpTo(group: HTMLElement): void {
  if (isCollapsed(group)) {
    // reuse the app's own toggle so collapse state has one owner
    const toggle = group.querySelector('.group-head .panel-toggle') as HTMLElement | null;
    toggle?.click();
  }
  // expanding changes the layout, so position after the next frame
  requestAnimationFrame(() => {
    group.scrollIntoView({ block: 'start', behavior: 'smooth' });
    group.classList.add('rail-flash');
    window.setTimeout(() => group.classList.remove('rail-flash'), 700);
    // The last groups can never reach the viewport top, so the scroll spy would move the
    // highlight elsewhere right after the jump; hold the clicked group for a moment.
    lockedGroup = group;
    lockUntil = performance.now() + 1500;
    markActive(group);
  });
}

/** Refresh the labels (init and language switch). */
function syncLabels(): void {
  items.forEach((it) => {
    const label = titleOf(it.group);
    it.el.textContent = label;
    it.el.title = label;
    it.el.setAttribute('aria-label', label);
  });
}

/** Highlight the group the user is currently looking at. */
function syncActive(): void {
  const p = panel();
  if (!p) return;
  if (lockedGroup && performance.now() < lockUntil) {
    markActive(lockedGroup);
    return;
  }
  lockedGroup = null;
  const idx = activeIndex(groupTops(p), p.scrollTop);
  items.forEach((it, i) => it.el.classList.toggle('active', i === idx));
}

export function initControlRail(): void {
  const host = document.getElementById('control-rail');
  const p = panel();
  if (!host || !p) return;
  host.innerHTML = '';
  items = railGroups().map((group) => {
    const el = document.createElement('button');
    el.type = 'button';
    el.className = 'rail-item';
    el.addEventListener('click', () => jumpTo(group));
    host.appendChild(el);
    return { el, group };
  });
  syncLabels();
  p.addEventListener('scroll', () => {
    if (scrollRaf) return;
    scrollRaf = requestAnimationFrame(() => { scrollRaf = 0; syncActive(); });
  }, { passive: true });
  window.addEventListener('resize', syncActive);
  onLangChange(syncLabels);
  syncActive();
}
