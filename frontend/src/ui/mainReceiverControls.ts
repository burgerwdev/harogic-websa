import { getLang } from '../core/i18n';
import { onCommandReply } from '../core/wsSend';
import { modeControlDefaults } from '../core/receiverControls';
import { setDomProperty } from './domState';

const l = (en: string, zh: string) => getLang() === 'zh' ? zh : en;
const rates = [12_000, 24_000, modeControlDefaults.audio_rate];
const modes = new Set(['am', 'dsb', 'usb', 'lsb', 'cw', 'nfm', 'wfm', 'pm']);
interface Context { service_id: string; receiver_uid: string; revision: string; action_token?: string }
interface Row { channel: number; mode: string; audio_rate?: number; control?: Context }
interface Control {
  element: HTMLElement; row: Row; send(command: Record<string, unknown>): void;
  pending: { request: string; rate: number; timer: number } | null;
  stopReplies?: () => void;
}
let current: Control | null = null;

function clearPending(control: Control): void {
  if (control.pending) window.clearTimeout(control.pending.timer);
  control.pending = null;
}
/** Remove the old actor as well as its buttons when selection or Preset retires it. */
export function clearMainReceiverControls(): void {
  if (!current) return;
  clearPending(current); current.stopReplies?.(); current.element.remove(); current = null;
  document.removeEventListener('websa:preset-applied', clearMainReceiverControls);
}
function render(control: Control): void {
  const rate = control.row.audio_rate ?? modeControlDefaults.audio_rate;
  const label = l('Audio rate', '音频采样率');
  control.element.querySelector('label')!.textContent = label;
  control.element.querySelector('[role=group]')!.setAttribute('aria-label', label);
  control.element.setAttribute('aria-busy', String(control.pending !== null));
  for (const button of control.element.querySelectorAll<HTMLButtonElement>('[data-sdr-audio-rate]')) {
    const value = Number(button.dataset.sdrAudioRate);
    const active = value === rate;
    button.classList.toggle('active', active);
    button.classList.toggle('pending', value === control.pending?.rate);
    button.setAttribute('aria-pressed', String(active));
    button.title = `${value / 1000} kHz${value === modeControlDefaults.audio_rate
      ? l(' (default; restored by Preset)', '（默认；Preset 恢复此值）') : ''}`;
    setDomProperty(button, 'disabled', control.pending !== null);
  }
  control.element.querySelector('[data-audio-rate-unit]')!.textContent = rates.includes(rate) ? 'kHz' : `${rate / 1000} kHz`;
}
/** Only sample-rate shortcuts live in the main panel; mode editors stay in their existing views. */
export function syncMainReceiverControls(row: Row, send: (command: Record<string, unknown>) => void): void {
  const context = row.control, anchor = document.getElementById('sdr-ifbw-group')?.parentElement;
  if (!context?.receiver_uid || !modes.has(row.mode) || !anchor) { clearMainReceiverControls(); return; }
  if (current && (!current.element.isConnected || current.row.control?.service_id !== context.service_id
      || current.row.control?.receiver_uid !== context.receiver_uid || current.row.channel !== row.channel)) clearMainReceiverControls();
  if (!current) {
    const element = document.createElement('div');
    element.id = 'sdr-audio-rate-row'; element.className = 'param-row sdr-audio-rate-row';
    element.innerHTML = `<label></label><div class="btn-group" role="group">${rates.map(rate =>
      `<button class="btn" type="button" data-sdr-audio-rate="${rate}"${rate === modeControlDefaults.audio_rate ? ' data-default="true"' : ''}>${rate / 1000}</button>`).join('')}</div><span class="cur-val" data-audio-rate-unit>kHz</span>`;
    const control: Control = { element, row, send, pending: null }; current = control;
    element.addEventListener('click', event => {
      const button = (event.target as Element).closest<HTMLButtonElement>('[data-sdr-audio-rate]');
      if (current !== control || !element.isConnected || !button || button.disabled || control.pending) return;
      const rate = Number(button.dataset.sdrAudioRate), captured = control.row.control!;
      if (!rates.includes(rate) || rate === (control.row.audio_rate ?? modeControlDefaults.audio_rate)) return;
      const request = `main-audio-rate-${crypto.randomUUID()}`;
      control.pending = { request, rate, timer: window.setTimeout(() => {
        clearPending(control); if (current === control) render(control);
      }, 3000) };
      render(control);
      control.send({ cmd: 'SET_SDR_CHANNEL', channel: control.row.channel, service_id: captured.service_id,
        receiver_uid: captured.receiver_uid, expected_revision: captured.revision, action_token: captured.action_token,
        audio_rate: rate, request_id: request });
    });
    control.stopReplies = onCommandReply(reply => {
      if (reply.cmd === 'DISCONNECTED') { clearMainReceiverControls(); return; }
      if (reply.cmd === 'ERROR' && reply.response_to === 'SET_SDR_CHANNEL'
          && reply.request_id === control.pending?.request) { clearPending(control); render(control); }
    });
    document.addEventListener('websa:preset-applied', clearMainReceiverControls);
    anchor.after(element);
  }
  if (current.pending && (row.control?.revision !== current.row.control?.revision
      || row.audio_rate === current.pending.rate)) clearPending(current);
  current.row = { ...row, control: { ...context } }; current.send = send;
  render(current);
}
