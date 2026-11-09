import { selectIsEditing } from './selectControl';

/** Reconcile a workbench surface without detaching live controls or scroll containers.
 * Keys describe ownership, so a retired/rebound receiver never inherits another draft.
 */
const key = (node: Node): string | null => {
  if (node.nodeType !== 1) return null;
  const element = node as Element;
  for (const name of ['data-key', 'data-uid', 'data-task-id', 'data-section', 'data-action', 'name']) {
    if (element.hasAttribute(name)) return `${element.tagName}:${name}:${element.getAttribute(name)}`;
  }
  return null;
};

export function patchWorkbench(host: HTMLElement, html: string, preserve: (element: Element) => boolean = () => false): void {
  const template = host.ownerDocument.createElement('template');
  template.innerHTML = html;
  // Ownership keys do not change during reconciliation. Each compatibility
  // check used to scan the same attributes again throughout the subtree.
  const keys = new WeakMap<Node, string | null>();
  const nodeKey = (node: Node): string | null => {
    if (!keys.has(node)) keys.set(node, key(node));
    return keys.get(node)!;
  };
  const compatible = (left: Node, right: Node): boolean => left.nodeType === right.nodeType
    && (left.nodeType !== 1 || (left as Element).tagName === (right as Element).tagName) && nodeKey(left) === nodeKey(right);
  const patch = (parent: Node, source: Node): void => {
    const old = [...parent.childNodes], used = new Set<Node>();
    const keyed = new Map(old.filter(node => nodeKey(node) !== null).map(node => [nodeKey(node), node]));
    let cursor = parent.firstChild;
    for (const next of [...source.childNodes]) {
      const id = nodeKey(next);
      const current = id !== null ? keyed.get(id) : old.find(node => !used.has(node) && compatible(node, next));
      if (!current || !compatible(current, next)) {
        parent.insertBefore(next, cursor);
        continue;
      }
      used.add(current);
      if (current !== cursor) parent.insertBefore(current, cursor);
      if (current.nodeType === 1) {
        const element = current as HTMLElement, fresh = next as HTMLElement;
        if (!preserve(element)) {
          // Disclosure state belongs to the user, including the interval before toggle fires.
          const keep = (name: string): boolean => name === 'open' && element.tagName === 'DETAILS';
          for (const attr of [...element.attributes]) if (!keep(attr.name) && !fresh.hasAttribute(attr.name)) element.removeAttribute(attr.name);
          for (const attr of [...fresh.attributes]) if (!keep(attr.name) && element.getAttribute(attr.name) !== attr.value) element.setAttribute(attr.name, attr.value);
          const selectedValue = fresh.tagName === 'SELECT' ? (fresh as HTMLSelectElement).value : '';
          // Protect the entire native menu, not just its value: changing option
          // labels, order or selected attributes also resets the hovered item.
          if (element.tagName !== 'SELECT' || !selectIsEditing(element as HTMLSelectElement)) patch(element, fresh);
          // Properties can differ from the template attributes after native editing.
          if (element.tagName === 'INPUT') {
            const input = element as HTMLInputElement, value = fresh as HTMLInputElement;
            if (element !== host.ownerDocument.activeElement) {
              if (input.value !== value.value) input.value = value.value;
              if (input.checked !== value.checked) input.checked = value.checked;
            }
          }
          if (element.tagName === 'SELECT' && !selectIsEditing(element as HTMLSelectElement)) {
            const select = element as HTMLSelectElement, value = selectedValue;
            if (select.value !== value) select.value = value;
          }
        }
      } else if (current.nodeValue !== next.nodeValue) current.nodeValue = next.nodeValue;
      cursor = current.nextSibling;
    }
    for (const node of old) if (!used.has(node)) node.remove();
  };
  patch(host, template.content);
}

/** For a surface owned by one renderer, keep live child lanes and skip identical
 * chrome. Reset after replacing the host's children outside this renderer. */
export function createWorkbenchPatcher(host: HTMLElement) {
  let previous: string | undefined;
  const patch = (html: string, preserve?: (element: Element) => boolean): boolean => {
    const editing = host.contains(host.ownerDocument.activeElement)
      && host.ownerDocument.activeElement?.matches('input,select,textarea');
    if (!editing && previous === html) return false;
    patchWorkbench(host, html, preserve);
    // A preserved input may still need the same update once editing ends.
    previous = editing ? undefined : html;
    return true;
  };
  return Object.assign(patch, { reset: () => { previous = undefined; } });
}
