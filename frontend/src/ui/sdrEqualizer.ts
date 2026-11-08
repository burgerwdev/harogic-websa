import './workbench.css';
import './sdrEqualizer.css';
import { EQ_BANDS, EQ_PRESETS, equalizerActive, getEqualizer, onEqualizerChange, resetEqualizer, setEqualizer } from '../audio/equalizer';
import { getLang, onLangChange } from '../core/i18n';
import { getUiScale } from '../core/uiScale';
import { createFloatingWindow, type FloatingWindow } from './floatingWindow';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
const db = (value: number): string => `${value > 0 ? '+' : ''}${value.toFixed(1)}`;
export function initSdrEqualizer(): () => void {
  const button = document.getElementById('btn-sdr-eq');
  if (!button) return () => {};
  let panel: FloatingWindow | undefined;
  const render = (): void => {
    const state = getEqualizer();
    button.classList.toggle('active', equalizerActive());
    button.setAttribute('aria-expanded', String(!!panel && !panel.root.hidden));
    button.title = l('Overall listening equalizer', '整体监听均衡器');
    if (!panel) return;
    panel.root.setAttribute('aria-label', l('Listening EQ', '监听 EQ'));
    panel.body.querySelector<HTMLElement>('[data-eq-subtitle]')!.textContent = l('All live listening channels · this browser', '全部实时监听信道 · 当前浏览器');
    const bypass = panel.body.querySelector<HTMLButtonElement>('[data-eq-bypass]')!;
    bypass.textContent = state.enabled ? l('On', '已启用') : l('Bypassed', '已旁路');
    bypass.setAttribute('aria-pressed', String(state.enabled));
    const preset = panel.body.querySelector<HTMLSelectElement>('[data-eq-preset]')!;
    const selected = Object.keys(EQ_PRESETS).find(key => EQ_PRESETS[key].every((gain, index) => gain === state.gains[index]));
    preset.innerHTML = [['custom', l('Custom', '自定义')], ['flat', l('Flat', '平直')], ['voice', l('Voice', '人声')], ['warm', l('Warm', '温暖')], ['bright', l('Bright', '明亮')]]
      .map(([key, text]) => `<option value="${key}" ${key === (selected ?? 'custom') ? 'selected' : ''} ${key === 'custom' ? 'disabled' : ''}>${text}</option>`).join('');
    preset.setAttribute('aria-label', l('EQ profile', 'EQ 曲线'));
    for (const input of panel.body.querySelectorAll<HTMLInputElement>('[data-eq-band]')) {
      const index = Number(input.dataset.eqBand), gain = state.gains[index];
      input.value = String(gain); input.setAttribute('aria-valuetext', `${db(gain)} dB`);
      input.closest('label')!.querySelector('output')!.textContent = db(gain);
    }
    panel.body.querySelector<HTMLInputElement>('[data-eq-trim]')!.value = String(state.trim);
    panel.body.querySelector('[data-eq-trim]')!.setAttribute('aria-label', l('EQ output level (dB)', 'EQ 输出电平（dB）'));
    panel.body.querySelector('[data-eq-trim-value]')!.textContent = `${db(state.trim)} dB`;
    panel.body.querySelector('[data-eq-level-label]')!.textContent = l('Output level', '输出电平');
    panel.body.querySelector('[data-eq-flat]')!.textContent = l('Reset to flat', '恢复平直');
    panel.body.querySelector('[data-eq-hint]')!.textContent = l('Recordings and decoder input keep their original sound.', '录制与解码输入保留原始声音。');
    panel.body.classList.toggle('eq-bypassed', !state.enabled);
  };
  const open = (): void => {
    if (panel) { panel.root.hidden ? panel.show() : panel.hide(); render(); return; }
    const scale = getUiScale();
    panel = createFloatingWindow('speaker-eq', () => l('Listening EQ', '监听 EQ'), { x: 220, y: 140, w: 440 * scale, h: 420 * scale }, render);
    panel.root.classList.add('sdr-eq-window');
    panel.body.innerHTML = `<p class="eq-subtitle" data-eq-subtitle></p>
      <div class="eq-toolbar"><button type="button" data-eq-bypass></button><select data-eq-preset></select><span>±12 dB</span></div>
      <div class="eq-bands">${EQ_BANDS.map((frequency, index) => `<label><output>0.0</output><input type="range" min="-12" max="12" step="0.5" value="0" data-eq-band="${index}" aria-label="${frequency} Hz"><strong>${frequency >= 1000 ? `${frequency / 1000}k` : frequency}</strong><small>Hz</small></label>`).join('')}</div>
      <label class="eq-trim"><span data-eq-level-label></span><input type="range" min="-12" max="6" step="0.5" data-eq-trim aria-label="EQ output dB"><output data-eq-trim-value></output></label>
      <footer><p data-eq-hint></p><button type="button" data-eq-flat></button></footer>`;
    panel.body.addEventListener('input', event => {
      const input = event.target as HTMLInputElement, state = getEqualizer();
      if (input.matches('[data-eq-band]')) { state.gains[Number(input.dataset.eqBand)] = Number(input.value); setEqualizer({ enabled: true, gains: state.gains }); }
      if (input.matches('[data-eq-trim]')) setEqualizer({ enabled: true, trim: Number(input.value) });
    });
    panel.body.addEventListener('click', event => {
      if ((event.target as HTMLElement).closest('[data-eq-bypass]')) setEqualizer({ enabled: !getEqualizer().enabled });
      if ((event.target as HTMLElement).closest('[data-eq-flat]')) resetEqualizer();
    });
    panel.body.addEventListener('change', event => {
      const select = event.target as HTMLSelectElement;
      if (select.matches('[data-eq-preset]') && EQ_PRESETS[select.value]) {
        const gains = EQ_PRESETS[select.value]; setEqualizer({ enabled: true, gains, trim: -Math.max(0, ...gains) });
      }
    });
    panel.body.addEventListener('keydown', event => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); panel?.hide(); }
    });
    panel.show(); render();
  };
  button.addEventListener('click', open);
  const stop = onEqualizerChange(render), language = onLangChange(() => { panel?.sync(); render(); }); render();
  return () => { stop(); language(); button.removeEventListener('click', open); panel?.dispose(); };
}
