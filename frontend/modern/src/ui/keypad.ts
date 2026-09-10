// Virtual keypad for the numeric inputs.
//
// Disabled by default: while the toggle in the top bar is off nothing changes. When it is on,
// clicking any enabled numeric input opens a floating pad anchored to that input; the pad types
// into the field and commits through the same paths the panel already uses, so a value entered
// here behaves exactly like one typed by hand.
//
//  - frequency-like fields (those with a unit button group) also get unit keys; pressing one sets
//    the unit and commits immediately, exactly like clicking the panel's unit button
//  - every other field shows its own unit (dBm, dB, pts, s ...) in the display line
//  - the sign is a state, not a character, so "5 then -" and "- then 5" give the same result
//  - fields marked data-keypad="text" are edited as plain text and only accept digits, commas and
//    minus signs (the N dB threshold list): their display line is a real text input, so the caret
//    can be placed anywhere with the mouse, backspace deletes one character at the caret and the
//    C key clears the whole entry
import { t } from '../core/i18n';
import { setUnit, UNIT_OPTIONS } from '../core/units';

const LS_KEY = 'websa-keypad';

// Some fields are staged inputs: the panel applies them through their own Set button, so a
// change event alone does nothing. Those are listed here and the pad clicks the button after
// writing the value (everything else commits on the change event).
const ROW_APPLY: Record<string, string> = {
  'input-ref': '#btn-ref-set',
  'input-points': '#btn-points',
  'input-rbw': '[data-action="apply-rbw"]',
  'input-vbw': '#btn-vbw-set',
  'input-harm-f0': '#btn-harm-set',
  'input-pnm': '#btn-pnm-set',
  'input-ampdbs': '#btn-amp-meas',
};

let enabled = false;
let pad: HTMLDivElement | null = null;
let displayEl: HTMLSpanElement | null = null;
let textEl: HTMLInputElement | null = null;    // the display line of a text field
let headUnitEl: HTMLSpanElement | null = null;  // the unit shown next to the display line
let sepBtn: HTMLButtonElement | null = null;    // '.', or ',' in a text field
let unitsEl: HTMLDivElement | null = null;
let target: HTMLInputElement | null = null;
let field = '';                 // units[] key when the field has a unit group
let buffer = '';                // typed digits (without sign)
let negative = false;
let replaced = false;           // the first key press replaces the previous value
let unitText = '';              // what the display line shows as the unit
let moved = false;              // the user dragged the pad: keep that spot

function el<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

/** A field edited as plain text: digits, commas and minus signs only (the N dB threshold list). */
function isTextField(input: HTMLInputElement): boolean {
  return input.dataset.keypad === 'text';
}

/** A field the pad is allowed to serve: visible, enabled, numeric or a text field. */
function isServed(input: EventTarget | null): input is HTMLInputElement {
  if (!(input instanceof HTMLInputElement) || input.disabled || input.readOnly) return false;
  if (input.offsetParent === null) return false;
  return input.type === 'number' || isTextField(input);
}

/** units[] key for an input id (`input-center` -> `center`) when it has a unit group. */
function fieldOf(input: HTMLInputElement): string {
  const key = input.id.startsWith('input-') ? input.id.slice('input-'.length) : '';
  return key && UNIT_OPTIONS[key] ? key : '';
}

/** Unit shown on the display line: the group's active unit, or the sibling value label. */
function unitOf(input: HTMLInputElement, key: string): string {
  if (key) {
    const grp = el(`unit-${key}-group`);
    const active = grp?.querySelector('.unit-btn.active') as HTMLElement | null;
    return (active?.textContent ?? '') || '';
  }
  if (input.dataset.unit) return input.dataset.unit;   // text fields carry their unit explicitly
  const label = input.parentElement?.querySelector('.cur-val');
  return (label?.textContent ?? '').trim();
}

function renderDisplay(): void {
  if (!displayEl) return;
  if (headUnitEl) headUnitEl.textContent = unitText;
  if (target && isTextField(target)) return;   // the text input shows its own contents
  const value = buffer === '' ? (replaced ? '0' : (target?.value ?? '0')) : buffer;
  displayEl.textContent = `${negative ? '-' : ''}${value}`;
}

// ---- text fields: real editing, the caret is wherever the mouse put it ----------------------

/** Insert at the caret (replacing the selection), the way a text box behaves. */
function insertText(ch: string): void {
  if (!textEl) return;
  const from = textEl.selectionStart ?? textEl.value.length;
  const to = textEl.selectionEnd ?? from;
  textEl.value = textEl.value.slice(0, from) + ch + textEl.value.slice(to);
  textEl.setSelectionRange(from + ch.length, from + ch.length);
  textEl.focus();
}

/** Delete one character before the caret (or the selection) - never the whole value. */
function backspaceText(): void {
  if (!textEl) return;
  const from = textEl.selectionStart ?? 0;
  const to = textEl.selectionEnd ?? from;
  if (from !== to) {
    textEl.value = textEl.value.slice(0, from) + textEl.value.slice(to);
    textEl.setSelectionRange(from, from);
  } else if (from > 0) {
    textEl.value = textEl.value.slice(0, from - 1) + textEl.value.slice(from);
    textEl.setSelectionRange(from - 1, from - 1);
  }
  textEl.focus();
}

/** Drop anything the field does not accept, keeping the caret where it was. */
function filterTextInput(): void {
  if (!textEl) return;
  const cleaned = textEl.value.replace(/[^-0-9,]/g, '');
  if (cleaned !== textEl.value) {
    const pos = Math.max(0, (textEl.selectionStart ?? 0) - (textEl.value.length - cleaned.length));
    textEl.value = cleaned;
    textEl.setSelectionRange(pos, pos);
  }
}

function buildPad(): HTMLDivElement {
  const host = document.createElement('div');
  host.id = 'keypad';
  host.className = 'keypad';
  host.style.display = 'none';
  host.setAttribute('role', 'dialog');

  const head = document.createElement('div');
  head.className = 'keypad-display';
  head.title = t('kp_drag');
  displayEl = document.createElement('span');
  displayEl.className = 'keypad-value';
  head.appendChild(displayEl);
  textEl = document.createElement('input');
  textEl.type = 'text';
  textEl.className = 'keypad-text';
  textEl.autocomplete = 'off';
  textEl.spellcheck = false;
  textEl.style.display = 'none';
  textEl.addEventListener('input', filterTextInput);
  head.appendChild(textEl);
  headUnitEl = document.createElement('span');
  headUnitEl.className = 'keypad-head-unit';
  head.appendChild(headUnitEl);
  host.appendChild(head);
  makeDraggable(head);

  unitsEl = document.createElement('div');
  unitsEl.className = 'keypad-units';
  host.appendChild(unitsEl);

  const grid = document.createElement('div');
  grid.className = 'keypad-grid';
  const layout: [string, string, string][] = [
    // label, css class, i18n key for the aria label ('' = symbol only)
    ['7', 'digit', ''], ['8', 'digit', ''], ['9', 'digit', ''], ['\u232b', 'back', 'kp_back'],
    ['4', 'digit', ''], ['5', 'digit', ''], ['6', 'digit', ''], ['C', 'clear', 'kp_clear'],
    ['1', 'digit', ''], ['2', 'digit', ''], ['3', 'digit', ''], ['\u2212', 'minus', 'kp_minus'],
    ['0', 'digit', ''], ['.', 'digit', ''], ['OK', 'ok', 'kp_ok'],
  ];
  layout.forEach(([label, cls, key]) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = `keypad-key ${cls}`;
    b.textContent = label;
    if (label === '.') { b.classList.add('sep'); sepBtn = b; }   // comma in a text field
    if (key) b.setAttribute('aria-label', t(key));
    // the live text, because the '.' key turns into ',' for text fields
    b.addEventListener('click', (e) => { e.stopPropagation(); press(cls, b.textContent ?? label); });
    grid.appendChild(b);
  });
  host.appendChild(grid);

  host.addEventListener('click', (e) => e.stopPropagation());
  return host;
}

/** Drag the pad by its display line. While it has been moved, scrolling keeps the spot. */
function makeDraggable(handle: HTMLElement): void {
  let armed = false;                 // a press is only a drag once it really moves
  let dragging = false;
  let startX = 0;
  let startY = 0;
  let dx = 0;
  let dy = 0;
  handle.addEventListener('pointerdown', (e) => {
    if (!pad) return;                // a plain click must still place the text caret
    armed = true;
    dragging = false;
    startX = e.clientX;
    startY = e.clientY;
    const r = pad.getBoundingClientRect();
    dx = e.clientX - r.left;
    dy = e.clientY - r.top;
    e.stopPropagation();
  });
  handle.addEventListener('pointermove', (e) => {
    if (armed && !dragging && Math.abs(e.clientX - startX) + Math.abs(e.clientY - startY) > 4) {
      dragging = true;
      try { handle.setPointerCapture(e.pointerId); } catch { /* not capturable */ }
    }
    if (!dragging || !pad) return;
    const x = Math.min(Math.max(2, e.clientX - dx), window.innerWidth - pad.offsetWidth - 2);
    const y = Math.min(Math.max(2, e.clientY - dy), window.innerHeight - pad.offsetHeight - 2);
    pad.style.left = `${Math.round(x)}px`;
    pad.style.top = `${Math.round(y)}px`;
    moved = true;
  });
  const stop = (e: PointerEvent) => {
    armed = false;
    dragging = false;
    try { handle.releasePointerCapture(e.pointerId); } catch { /* already released */ }
  };
  handle.addEventListener('pointerup', stop);
  handle.addEventListener('pointercancel', stop);
}

function buildUnits(): void {
  if (!unitsEl) return;
  unitsEl.innerHTML = '';
  const opts = field ? UNIT_OPTIONS[field] : [];
  unitsEl.style.display = opts.length ? '' : 'none';
  opts.forEach((u) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'keypad-key unit';
    b.textContent = u;
    b.classList.toggle('active', u === unitText);
    b.addEventListener('click', (e) => {
      e.stopPropagation();
      unitText = u;
      commit(u);                       // unit keys commit immediately (same as the panel buttons)
    });
    unitsEl!.appendChild(b);
  });
}

function place(): void {
  if (!pad || !target || moved) return;
  const r = target.getBoundingClientRect();
  const pw = pad.offsetWidth || 190;
  const ph = pad.offsetHeight || 250;
  let x = Math.min(Math.max(6, r.left), window.innerWidth - pw - 6);
  let y = r.bottom + 6;
  if (y + ph > window.innerHeight - 6) y = Math.max(6, r.top - ph - 6);
  pad.style.left = `${Math.round(x)}px`;
  pad.style.top = `${Math.round(y)}px`;
}

function open(input: HTMLInputElement): void {
  if (!enabled) return;
  target = input;
  field = fieldOf(input);
  buffer = '';
  negative = false;
  replaced = false;
  moved = false;                     // a new open re-anchors to its field
  unitText = unitOf(input, field);
  // The pad (and with it the display line and the unit row) must exist BEFORE they are filled:
  // building the rows first left the first open of a session without unit keys.
  if (!pad) pad = buildPad();
  if (!pad.isConnected) document.body.appendChild(pad);
  // A text field has no decimals, so the '.' key becomes the list separator there, and its
  // display line becomes an editable input so the caret can be placed with the mouse.
  const text = isTextField(input);
  if (sepBtn) {
    sepBtn.textContent = text ? ',' : '.';
    sepBtn.title = text ? t('kp_comma') : '';
  }
  if (displayEl) displayEl.style.display = text ? 'none' : '';
  if (textEl) {
    textEl.style.display = text ? '' : 'none';
    if (text) {
      textEl.value = input.value;
      filterTextInput();
      textEl.focus();
      textEl.setSelectionRange(textEl.value.length, textEl.value.length);
    }
  }
  buildUnits();
  renderDisplay();
  pad.style.display = '';
  place();
  input.blur();                        // the pad owns the input while it is open
}

function close(): void {
  target = null;
  if (pad) pad.style.display = 'none';
}

function press(cls: string, label: string): void {
  if (!target) return;
  if (isTextField(target)) {
    // the display line is a text box: every key edits it at the caret
    if (cls === 'digit') insertText(label);
    else if (cls === 'minus') insertText('-');
    else if (cls === 'back') backspaceText();
    else if (cls === 'clear') { if (textEl) { textEl.value = ''; textEl.focus(); } }
    else if (cls === 'ok') { commit(''); return; }
    renderDisplay();
    return;
  }
  if (cls === 'digit') {
    if (label === ',') { /* commas exist in the threshold list only */ }
    else if (!replaced) { buffer = label; replaced = true; }
    else if (label === '.') { if (!buffer.includes('.')) buffer += '.'; }
    else buffer += label;
  } else if (cls === 'minus') {
    negative = !negative;              // a state, so the order of the presses does not matter
  } else if (cls === 'back') {
    buffer = buffer.slice(0, -1);
    replaced = true;
  } else if (cls === 'clear') {
    buffer = '';
    negative = false;
    replaced = true;
  } else if (cls === 'ok') {
    commit('');
    return;
  }
  renderDisplay();
}

/** Write the value into the field and let the existing commit paths do the rest. */
function commit(chosenUnit: string): void {
  if (!target) return;
  const input = target;
  if (isTextField(input)) {
    // keep only what the field accepts, then tidy the separators
    input.value = (textEl?.value ?? '').replace(/[^-0-9,]/g, '')
      .replace(/,{2,}/g, ',').replace(/^,|,$/g, '');
    input.dispatchEvent(new Event('change', { bubbles: true }));
    const apply = ROW_APPLY[input.id];
    if (apply) (document.querySelector(apply) as HTMLElement | null)?.click();
    close();
    return;
  }
  const raw = buffer === '' ? '' : buffer;
  if (raw === '' && !replaced) { close(); return; }
  let value = parseFloat((negative ? '-' : '') + (raw === '' ? '0' : raw));
  if (!Number.isFinite(value)) value = 0;
  const min = target.min !== '' ? parseFloat(target.min) : NaN;
  const max = target.max !== '' ? parseFloat(target.max) : NaN;
  if (Number.isFinite(min) && value < min) value = min;
  if (Number.isFinite(max) && value > max) value = max;
  input.value = String(value);
  if (field) {
    // Mark the field as manually edited so setUnit() interprets the number in the new unit
    // instead of converting it, then reuse the panel's own commit (it dispatches the command).
    input.dataset.edited = '1';
    if (chosenUnit) unitText = chosenUnit;
    setUnit(field, unitText);
    delete input.dataset.edited;
  } else {
    input.dispatchEvent(new Event('change', { bubbles: true }));
    const apply = ROW_APPLY[input.id];
    if (apply) (document.querySelector(apply) as HTMLElement | null)?.click();
  }
  close();
}

function onDocClick(e: MouseEvent): void {
  if (!enabled) return;
  if (isServed(e.target)) { open(e.target); return; }
  if (pad && e.target instanceof Node && pad.contains(e.target)) return;
  close();
}

function onKeyDown(e: KeyboardEvent): void {
  if (!enabled || !target) return;
  const k = e.key;
  if (isTextField(target)) {
    // the display line is a real input: typing and backspace are the browser's job there
    if (k === 'Enter') { e.preventDefault(); commit(''); }
    else if (k === 'Escape') close();
    else if (document.activeElement !== textEl) {
      // a mouse press on a pad key moved the focus away: keep the keyboard usable
      if (/^[-0-9,]$/.test(k)) { insertText(k); e.preventDefault(); }
      else if (k === 'Backspace') { backspaceText(); e.preventDefault(); }
    }
    return;
  }
  if (/^[0-9]$/.test(k)) press('digit', k);
  else if (k === '.') press('digit', '.');
  else if (k === '-') press('minus', '-');
  else if (k === ',') press('digit', ',');
  else if (k === 'Backspace') press('back', '');
  else if (k === 'Enter') press('ok', '');
  else if (k === 'Escape') { close(); return; }
  else return;
  e.preventDefault();
}

function syncToggle(): void {
  const btn = el<HTMLButtonElement>('btn-keypad');
  if (!btn) return;
  btn.classList.toggle('active', enabled);
  btn.setAttribute('aria-pressed', String(enabled));
  btn.title = t('tip_keypad');
}

export function initKeypad(): void {
  enabled = localStorage.getItem(LS_KEY) === 'on';
  const btn = el<HTMLButtonElement>('btn-keypad');
  btn?.addEventListener('click', (e) => {
    e.stopPropagation();
    enabled = !enabled;
    localStorage.setItem(LS_KEY, enabled ? 'on' : 'off');
    if (!enabled) close();
    syncToggle();
  });
  document.addEventListener('click', onDocClick, true);
  document.addEventListener('keydown', onKeyDown);
  window.addEventListener('resize', () => { if (target) place(); });
  // the control column scrolls: keep the pad glued to its field, or close it when it leaves
  document.querySelector('.control-panel')?.addEventListener('scroll', () => {
    if (!target || moved) return;      // a pad the user placed stays where it is
    const r = target.getBoundingClientRect();
    if (r.bottom < 0 || r.top > window.innerHeight) close();
    else place();
  }, { passive: true });
  syncToggle();
}
