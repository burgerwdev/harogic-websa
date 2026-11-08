/** Native popups keep hover/keyboard state outside the DOM. Even assigning the
 * same value can reset that state, so status renders must leave active selects alone.
 * ownerDocument also covers controls moved into independent windows.
 */
export function selectIsEditing(select: HTMLSelectElement): boolean {
  // Customizable native selects (appearance: base-select) focus their options.
  return select.contains(select.ownerDocument.activeElement);
}

export function syncSelectValue(select: HTMLSelectElement | null, value: string, force = false): void {
  if (select && select.value !== value && (force || !selectIsEditing(select))) select.value = value;
}

export interface SelectChoice { value: string; label: string; disabled?: boolean }

/** Keep option identities and avoid changing a native menu while it is in use. */
export function syncSelectOptions(select: HTMLSelectElement, choices: SelectChoice[], value: string): void {
  if (select.options.length && selectIsEditing(select)) return;
  const old = new Map([...select.options].map(option => [option.value, option]));
  for (const [index, choice] of choices.entries()) {
    const option = old.get(choice.value) ?? select.ownerDocument.createElement('option');
    old.delete(choice.value);
    if (option.value !== choice.value) option.value = choice.value;
    if (option.textContent !== choice.label) option.textContent = choice.label;
    if (option.disabled !== !!choice.disabled) option.disabled = !!choice.disabled;
    if (select.options[index] !== option) select.insertBefore(option, select.options[index] ?? null);
  }
  for (const option of old.values()) option.remove();
  syncSelectValue(select, value);
}
