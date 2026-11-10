/** Reflected properties still emit DOM mutations when assigned their old value.
 * Native select popups can rebuild on those mutations, losing the hovered item.
 */
export function setDomProperty<T, K extends keyof T>(target: T, key: K, value: T[K]): void {
  if (target[key] !== value) target[key] = value;
}

export function setDomAttribute(target: Element, name: string, value: string): void {
  if (target.getAttribute(name) !== value) target.setAttribute(name, value);
}

export function setStyleProperty(target: HTMLElement, name: string, value: string): void {
  if (target.style.getPropertyValue(name) !== value) target.style.setProperty(name, value);
}
