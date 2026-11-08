import { setDomProperty } from './domState';
import { defaultTransitionHz, occupiedBandwidth } from '../core/receiverBandwidth';
import { CHANNEL_COLORS } from '../core/channelColors';
import { getLang } from '../core/i18n';
import { receiverHasAudio, type ReceiverConfig, type ReceiverMode } from '../core/multichannel';
import { convertUnitValue } from '../core/units';
import { frequencyInput, wireFrequencyInputs } from './frequencyInput';
import type { WorkbenchSession } from './workbenchSession';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;

/** The fixture advertises mode IDs only; production uses the service's mode descriptors. */
function modes(session: WorkbenchSession): ReceiverMode[] {
  if (session.snapshot.transport !== 'fixture') return session.snapshot.capabilities.mode_descriptors ?? [];
  return session.snapshot.capabilities.modes.map(id => ({ id, label: id.toUpperCase(), provider: 'fixture',
    kind: id === 'ft8' ? 'digital' : 'analog', available: true, unavailableReason: null,
    defaultBandwidthHz: id === 'nfm' ? 12500 : id === 'am' ? 6000 : 3000,
    minimumRateHz: id === 'ft8' ? 8000 : 6000, sideband: ['usb', 'ft8'].includes(id) ? 'upper' : 'symmetric',
    audioCompanion: null, outputs: id === 'ft8' ? ['table'] : ['audio'] }));
}

/** Configure a channel before creating it, so one submit can start reception and monitoring. */
export function mountWorkbenchCreate(session: WorkbenchSession, root: HTMLElement, created: (uid: string) => void): { open(): void; dispose(): void } {
  const doc = root.ownerDocument, dialog = doc.createElement('dialog');
  dialog.className = 'mc-create-dialog'; dialog.dataset.createChannel = '';
  root.append(dialog);
  const unwireFrequency = wireFrequencyInputs(dialog);
  let busy = false, disposed = false;
  const field = <T extends HTMLElement>(selector: string): T => dialog.querySelector<T>(selector)!;
  const candidate = (): ReceiverConfig => {
    const mode = modes(session).find(m => m.id === field<HTMLSelectElement>('[name=mode]').value);
    const hz = convertUnitValue(Number(field<HTMLInputElement>('[name=frequency]').value), field<HTMLSelectElement>('[name=unit]').value, 'Hz');
    const passband = mode ? occupiedBandwidth(mode) : 3000, transition = defaultTransitionHz(passband);
    return { name: field<HTMLInputElement>('[name=name]').value.trim() || `${hz / 1e6} MHz · ${mode?.label ?? 'IQ'}`,
      color: CHANNEL_COLORS[Array.from({ length: session.snapshot.capabilities.max_channels }, (_, index) => index + 1).findIndex(number => !session.snapshot.receivers.some(receiver => receiver.display_number === number)) % CHANNEL_COLORS.length], tags: [], enabled: true,
      purpose: mode ? mode.kind === 'analog' ? 'listen' : 'digital' : 'iq',
      baseband: { frequency_hz: hz, passband_hz: passband, transition_hz: transition,
        minimum_rate_hz: Math.max(mode?.minimumRateHz ?? 6000, passband + 2 * transition) },
      processors: mode ? [{ id: 'primary', kind: mode.kind === 'analog' ? 'demodulator' : 'decoder', mode: mode.id,
        minimum_rate_hz: mode.minimumRateHz, bandwidth_hz: mode.defaultBandwidthHz, pcm_gain: 0.5, agc: true, squelch_dbm: -160 }] : [] };
  };
  const validate = (): boolean => {
    if (!dialog.open || busy) return false;
    const config = candidate(), capture = session.snapshot.capture, bandwidth = capture.usable_bandwidth_hz;
    const audio = receiverHasAudio(session.snapshot, config) && (session.snapshot.transport === 'fixture' || !!session.snapshot.capabilities.monitor_mixing);
    field<HTMLElement>('[data-create-listen]').hidden = !audio;
    let error = '';
    if (session.snapshot.receivers.length >= session.snapshot.capabilities.max_channels) error = l('Channel limit reached. Remove an unused channel first.', '信道路数已满，请先删除不需要的信道。');
    else if (capture.state !== 'running' || bandwidth === null || capture.parameters_state === 'unknown') error = l('Start or recover capture in the main SDR panel first.', '请先在主 SDR 面板启动或恢复采集。');
    else if (!Number.isFinite(config.baseband.frequency_hz) || config.baseband.frequency_hz <= 0) error = l('Enter a valid frequency.', '请输入有效频率。');
    else if (config.processors.length && !modes(session).some(m => m.available && m.id === config.processors[0].mode)) error = l('Choose an available mode.', '请选择可用的解调 / 解码模式。');
    else if (Math.abs(config.baseband.frequency_hz - capture.center_hz) + config.baseband.passband_hz / 2 + config.baseband.transition_hz > bandwidth! / 2)
      error = l('This frequency and its passband exceed the capture range. Adjust the frequency or the shared capture in the main SDR panel.', '该频率及完整通带超出采集范围，请修改频率，或在主 SDR 面板调整共享采集。');
    field<HTMLElement>('[data-create-range]').textContent = bandwidth === null ? ''
      : `${l('Capture range', '可用采集范围')} · ${((capture.center_hz - bandwidth / 2) / 1e6).toFixed(6)}–${((capture.center_hz + bandwidth / 2) / 1e6).toFixed(6)} MHz`;
    field<HTMLElement>('[data-create-error]').textContent = error;
    const listen = audio && field<HTMLInputElement>('[name=listen]').checked;
    field<HTMLButtonElement>('[type=submit]').textContent = listen ? l('Create and listen', '创建并监听') : l('Create and receive', '创建并接收');
    field<HTMLButtonElement>('[type=submit]').disabled = !!error;
    return !error;
  };
  const close = (): void => { if (!busy) dialog.close(); };
  dialog.addEventListener('keydown', event => event.stopPropagation());
  dialog.addEventListener('cancel', event => { if (busy) event.preventDefault(); });
  dialog.addEventListener('click', event => { if ((event.target as HTMLElement).closest('[data-dismiss]')) close(); });
  dialog.addEventListener('input', validate);
  dialog.addEventListener('change', validate);
  dialog.addEventListener('submit', event => {
    event.preventDefault(); if (!validate()) return;
    const config = candidate(), listen = !field<HTMLElement>('[data-create-listen]').hidden && field<HTMLInputElement>('[name=listen]').checked;
    busy = true;
    dialog.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>('input,select,button').forEach(el => { setDomProperty(el, 'disabled', true); });
    field<HTMLButtonElement>('[type=submit]').textContent = l('Creating…', '正在创建…');
    void session.create(config).then(reply => {
      busy = false; if (disposed) return;
      dialog.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>('input,select,button').forEach(el => { setDomProperty(el, 'disabled', false); });
      validate();
      if (reply.fault) { field<HTMLElement>('[data-create-error]').textContent = reply.fault.message; return; }
      const uid = reply.receiver_uid;
      if (uid) { if (listen) session.monitor(uid, { enabled: true }); created(uid); }
      dialog.close();
    });
  });
  const stop = session.subscribe(validate);
  return { open: () => {
    if (dialog.open) { field<HTMLInputElement>('[name=frequency]').focus(); return; }
    const add = (parent: HTMLElement, value: string, text: string, disabled = false): void => {
      const option = doc.createElement('option'); option.value = value; option.textContent = text; setDomProperty(option, 'disabled', disabled); parent.append(option);
    };
    dialog.innerHTML = `<form><header><strong>${l('Add channel', '添加信道')}</strong><button type="button" data-dismiss aria-label="${l('Cancel adding a channel', '取消添加信道')}" title="${l('Cancel adding a channel', '取消添加信道')}">×</button></header>
      <p class="mc-notice">${l('Choose a frequency and mode. Bandwidth and rate are set automatically.', '选择频率和模式，自动设置合适的带宽与速率。')}</p>
      <label>${l('Frequency', '频率')}${frequencyInput(session.snapshot.capture.center_hz, 'unit')}</label>
      <p data-create-range class="mc-notice"></p>
      <label>${l('Demodulate / decode', '解调 / 解码')}<select name="mode"></select></label>
      <label>${l('Name (optional)', '名称（可选）')}<input name="name" maxlength="120" placeholder="${l('Name automatically', '按频率和模式自动命名')}"></label>
      <label data-create-listen class="mc-check"><input type="checkbox" name="listen" checked>${l('Listen after creating', '创建后开启监听')}</label>
      <p data-create-error class="mc-error" role="alert"></p><footer><button type="button" data-dismiss title="${l('Cancel adding a channel', '取消添加信道')}">${l('Cancel', '取消')}</button><button type="submit" class="mc-primary" title="${l('Create a channel with these settings', '按以上设置创建信道')}"></button></footer></form>`;
    dialog.setAttribute('aria-label', l('Add channel', '添加信道'));
    const modeSelect = field<HTMLSelectElement>('[name=mode]');
    for (const mode of modes(session)) add(modeSelect, mode.id, `${mode.label}${mode.available ? '' : ` · ${mode.unavailableReason ?? l('Unavailable', '不可用')}`}`, !mode.available);
    add(modeSelect, '', l('IQ only · no demodulation', '仅接收 IQ · 不解调'));
    modeSelect.value = modes(session).find(m => m.id === 'nfm' && m.available)?.id ?? modes(session).find(m => m.available)?.id ?? '';
    dialog.showModal(); validate(); field<HTMLInputElement>('[name=frequency]').focus(); field<HTMLInputElement>('[name=frequency]').select();
  }, dispose: () => { disposed = true; stop(); unwireFrequency(); dialog.remove(); } };
}
