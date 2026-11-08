import { getLang } from '../core/i18n';
import './copyDetails.css';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;

export function copyDetailsButton(action: 'copy-raw' | 'copy-stream' | 'copy-task' | 'copy-iq-preview' = 'copy-raw'): string {
  return `<button type="button" class="btn raw-copy" data-action="${action}" title="${l('Copy original fields as JSON', '复制原始字段 JSON')}" aria-live="polite">${l('Copy', '复制')}</button>`;
}

/** Source panels keep their original model without materializing a JSON preview. */
export function appendCopyDetails(host: HTMLElement, value: () => unknown): void {
  const template = host.ownerDocument.createElement('template');
  template.innerHTML = copyDetailsButton();
  const button = template.content.firstElementChild as HTMLButtonElement;
  button.addEventListener('click', event => {
    event.preventDefault(); event.stopPropagation();
    void copyDetails(button, JSON.stringify(value(), null, 2));
  });
  const summary = host.querySelector('summary');
  if (summary) summary.after(button); else host.append(button);
}

/** Use the button's document so detached workbench windows copy in their own context. */
export async function copyDetails(button: HTMLButtonElement, text: string): Promise<void> {
  const doc = button.ownerDocument;
  let copied = false;
  try {
    const clipboard = doc.defaultView?.navigator.clipboard;
    if (clipboard) { await clipboard.writeText(text); copied = true; }
  } catch { /* Plain HTTP and denied clipboard access can still allow a user-initiated copy. */ }
  if (!copied) {
    const active = doc.activeElement as HTMLElement | null;
    const selection = doc.getSelection();
    const ranges = selection ? Array.from({ length: selection.rangeCount }, (_, i) => selection.getRangeAt(i).cloneRange()) : [];
    const input = doc.createElement('textarea');
    input.value = text;
    input.style.cssText = 'position:fixed;left:-10000px;top:0';
    doc.body.append(input);
    try { input.focus({ preventScroll: true }); input.select(); copied = doc.execCommand?.('copy') === true; }
    catch { copied = false; }
    finally {
      input.remove(); active?.focus({ preventScroll: true });
      if (selection) { selection.removeAllRanges(); for (const range of ranges) selection.addRange(range); }
    }
  }
  button.textContent = copied ? l('Copied', '已复制') : l('Copy failed', '复制失败');
  button.title = copied ? l('Original fields copied', '原始字段已复制') : l('Select the original fields and copy manually', '请选中原始字段手动复制');
}
