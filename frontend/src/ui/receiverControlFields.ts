import './receiverControlFields.css';
import { getLang } from '../core/i18n';
import { copyModeControls, modeControlDefaults, modeControlKeys, type ModeControls } from '../core/receiverControls';
import { sstvModeTable } from '../sdr/registry';
const l = (en: string, zh: string) => getLang() === 'zh' ? zh : en;
const esc = (value: unknown): string => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
const groups: Record<keyof ModeControls, string[]> = {
  am_sync: ['am'], pitch: ['cw'], deemph_us: ['nfm', 'wfm'], stereo: ['wfm'], audio_rate: ['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm'], sstv_mode: ['sstv'],
  radiofax_acquisition: ['radiofax'], radiofax_ioc: ['radiofax'], radiofax_lpm: ['radiofax'], radiofax_modulation: ['radiofax'],
  radiofax_polarity: ['radiofax'], radiofax_clock_recovery: ['radiofax'], radiofax_phase_pixels: ['radiofax'], radiofax_clock_ppm: ['radiofax'],
};
const labels: Record<keyof ModeControls, [string, string]> = {
  am_sync: ['Synchronous AM', 'AM 同步检波'], pitch: ['CW pitch (Hz)', 'CW 音调 (Hz)'], deemph_us: ['FM deemphasis (µs; −1 auto, 0 off)', 'FM 去加重 (µs；−1 自动，0 关闭)'],
  stereo: ['WFM stereo', 'WFM 立体声'], audio_rate: ['Audio sample rate (Hz)', '音频采样率 (Hz)'], sstv_mode: ['SSTV mode', 'SSTV 模式'],
  radiofax_acquisition: ['Acquisition', '捕获'], radiofax_ioc: ['IOC', 'IOC'], radiofax_lpm: ['Lines per minute', '每分钟行数'],
  radiofax_modulation: ['Modulation', '调制'], radiofax_polarity: ['Polarity', '极性'], radiofax_clock_recovery: ['Clock recovery', '时钟恢复'],
  radiofax_phase_pixels: ['Phase (pixels)', '相位 (像素)'], radiofax_clock_ppm: ['Clock correction (ppm)', '时钟校正 (ppm)'],
};
const options: Partial<Record<keyof ModeControls, string[]>> = {
  radiofax_acquisition: ['auto', 'manual'], radiofax_ioc: ['auto', '288', '576'], radiofax_lpm: ['auto', '60', '90', '120', '180', '240'],
  radiofax_modulation: ['fm', 'am'], radiofax_polarity: ['normal', 'inverted'], radiofax_clock_recovery: ['auto', 'off'],
};
export function receiverControlFields(value?: ModeControls, mode = ''): string {
  const controls = copyModeControls(value);
  return `<fieldset data-mode-controls ${Object.values(groups).some(modes => modes.includes(mode)) ? '' : 'hidden'}><legend>${l('Mode settings', '模式设置')}</legend>${modeControlKeys.map(key => {
    const val = controls[key], name = `mode-${key}`, label = l(...labels[key]);
    let input: string;
    if (typeof modeControlDefaults[key] === 'boolean') input = `<input name="${name}" type="checkbox" ${val ? 'checked' : ''}>`;
    else if (key === 'sstv_mode') input = `<select name="${name}"><option value="" ${val == null ? 'selected' : ''}>${l('Automatic', '自动')}</option>${(sstvModeTable() ?? []).map(m => `<option value="${m.index}" ${val === m.index ? 'selected' : ''}>${esc(m.name)}</option>`).join('')}</select>`;
    else if (options[key]) input = `<select name="${name}">${options[key]!.map(option => `<option value="${option}" ${val === option ? 'selected' : ''}>${esc(option)}</option>`).join('')}</select>`;
    else input = `<input name="${name}" type="number" step="any" value="${esc(val)}" required>`;
    return `<label data-mode-field="${key}" ${groups[key].includes(mode) ? '' : 'hidden'}><span>${esc(label)}</span>${input}</label>`;
  }).join('')}<div data-mode-actions ${mode === 'radiofax' ? '' : 'hidden'}><button class="btn" type="button" data-action="radiofax_new_page" title="${l('Start a new fax page once', '新建一页传真')}">${l('New fax page', '传真新页')}</button><button class="btn" type="button" data-action="radiofax_reset_correction" title="${l('Reset fax phase and clock correction once', '重置传真相位与时钟校正')}">${l('Reset correction', '重置校正')}</button></div></fieldset>`;
}
export function syncReceiverControlFields(form: HTMLFormElement, mode: string, values?: ModeControls): void {
  for (const key of modeControlKeys) {
    const label = form.querySelector<HTMLElement>(`[data-mode-field=${key}]`); if (label) label.hidden = !groups[key].includes(mode);
    const input = form.elements.namedItem(`mode-${key}`) as HTMLInputElement | HTMLSelectElement | null;
    if (input && values) {
      const value = values[key] ?? modeControlDefaults[key];
      if (input instanceof HTMLInputElement && input.type === 'checkbox') input.checked = !!value;
      else input.value = value == null ? '' : String(value);
    }
  }
  const fieldset = form.querySelector<HTMLElement>('[data-mode-controls]'); if (fieldset) fieldset.hidden = !Object.values(groups).some(modes => modes.includes(mode));
  const actions = form.querySelector<HTMLElement>('[data-mode-actions]'); if (actions) actions.hidden = mode !== 'radiofax';
}
export function readReceiverControlFields(form: HTMLFormElement): ModeControls {
  return Object.fromEntries(modeControlKeys.flatMap(key => {
    const input = form.elements.namedItem(`mode-${key}`) as HTMLInputElement | HTMLSelectElement | null;
    if (!input) return [];
    return [[key, input instanceof HTMLInputElement && input.type === 'checkbox' ? input.checked
      : key === 'sstv_mode' ? input.value === '' ? null : Number(input.value)
      : typeof modeControlDefaults[key] === 'number' ? Number(input.value) : input.value]];
  }));
}
