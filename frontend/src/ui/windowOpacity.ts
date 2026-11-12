import { onLangChange, t } from '../core/i18n';

const KEY = 'web-sa-window-opacity';
const MIN = .35;
let opacity = 1;
let initialized = false;
const bindings = new Map<HTMLElement, { update(): void; dispose(): void }>();
const clamp = (value: number) => Math.min(1, Math.max(MIN, value));

function read(): number {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved !== null) return saved.trim() && Number.isFinite(Number(saved)) ? clamp(Number(saved)) : 1;
    // The original Decode window was the only surface with this preference.
    const legacy = JSON.parse(localStorage.getItem('websa-decode-window') ?? 'null')?.opacity;
    if (typeof legacy === 'number' && Number.isFinite(legacy)) return clamp(legacy);
  } catch { /* optional preference */ }
  return 1;
}

export function initWindowOpacity(): void {
  if (initialized) return;
  initialized = true;
  opacity = read();
  try { localStorage.setItem(KEY, String(opacity)); } catch { /* session only */ }
  window.addEventListener('storage', event => {
    if (event.key === KEY || event.key === null) {
      opacity = read();
      bindings.forEach(binding => binding.update());
    }
  });
}

export function setWindowOpacity(value: number): void {
  initWindowOpacity();
  opacity = Number.isFinite(value) ? clamp(value) : 1;
  try { localStorage.setItem(KEY, String(opacity)); } catch { /* session only */ }
  bindings.forEach(binding => binding.update());
}

/** One preference for all tools, including DOM adopted into desktop popouts. */
export function bindWindowOpacity(root: HTMLElement, slider: HTMLInputElement): () => void {
  initWindowOpacity();
  bindings.get(root)?.dispose();
  slider.type = 'range'; slider.min = '35'; slider.max = '100'; slider.step = '5';
  slider.classList.add('window-opacity');
  const update = () => {
    root.style.opacity = String(opacity);
    root.dataset.windowOpacity = opacity === 1 ? 'opaque' : 'translucent';
    root.style.setProperty('--window-solid-fill', `${(opacity - MIN) / (1 - MIN) * 100}%`);
    const percent = Math.round(opacity * 100);
    slider.value = String(percent);
    slider.title = `${t('window_opacity')}: ${percent}%`;
    slider.setAttribute('aria-label', t('window_opacity'));
    slider.setAttribute('aria-valuetext', `${percent}%`);
  };
  const input = () => setWindowOpacity(Number(slider.value) / 100);
  slider.addEventListener('input', input);
  const unlang = onLangChange(update);
  const dispose = () => {
    slider.removeEventListener('input', input); unlang(); bindings.delete(root);
    root.style.removeProperty('opacity'); root.style.removeProperty('--window-solid-fill');
    delete root.dataset.windowOpacity;
  };
  bindings.set(root, { update, dispose }); update();
  return dispose;
}
