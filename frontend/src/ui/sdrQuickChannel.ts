import { getLang, onLangChange } from '../core/i18n';
import { clone, receiverHasAudio, type ReceiverConfig, type ReceiverState } from '../core/multichannel';
import { formatFreqHz } from '../core/fmt';
import { currentSdrChannel } from './sdrChannels';
import { demodBandHz, sdrAgc, sdrDemod, sdrIfbw, sdrListenHz, sdrPitchHz, sdrSquelch, sdrVolume } from './sdrState';
import type { WorkbenchSession } from './workbenchSession';

const l = (en: string, zh: string) => getLang() === 'zh' ? zh : en;
export function currentChannelConfig(session: WorkbenchSession): ReceiverConfig {
  const mode = sdrDemod.get(), bandwidth = sdrIfbw.get(), hz = sdrListenHz.get();
  const descriptor = session.snapshot.capabilities.mode_descriptors?.find(m => m.id === mode);
  if (!descriptor?.available) throw new Error(l('This mode is unavailable.', '当前解调模式不可用。'));
  const [lo, hi] = demodBandHz(mode, bandwidth, sdrPitchHz.get());
  const passband = Math.max(Math.abs(lo), Math.abs(hi)) * 2, transition = 1000;
  return { name: `${formatFreqHz(hz)} · ${descriptor.label}`, color: '#e6b95c', tags: [], enabled: true,
    purpose: descriptor.kind === 'analog' ? 'listen' : 'digital',
    baseband: { frequency_hz: hz, passband_hz: passband, transition_hz: transition,
      minimum_rate_hz: Math.max(descriptor.minimumRateHz, passband + 2 * transition) },
    processors: [{ id: `quick-${session.requestId()}`, kind: descriptor.kind === 'analog' ? 'demodulator' : 'decoder',
      mode, minimum_rate_hz: descriptor.minimumRateHz, bandwidth_hz: bandwidth,
      pcm_gain: sdrVolume.get(), agc: sdrAgc.get(), squelch_dbm: sdrSquelch.get() }] };
}

/** Add the main panel's current frequency; a full service always requires an explicit target. */
export function mountSdrQuickChannel(session: WorkbenchSession, host: HTMLElement) {
  const doc = host.ownerDocument, dialog = doc.createElement('dialog');
  dialog.className = 'mc-create-dialog sdr-replace-dialog'; dialog.dataset.replaceChannel = '';
  dialog.innerHTML = '<form><header><strong data-title></strong><button type="button" data-dismiss>×</button></header><p data-frequency></p><div data-targets></div><p data-error class="mc-error" role="alert"></p><footer><button type="button" data-dismiss></button><button type="submit" class="mc-primary"></button></footer></form>';
  host.append(dialog);
  let candidate: ReceiverConfig | null = null, busy = false, disposed = false;
  const revisions = new Map<string, string>();
  const selected = () => dialog.querySelector<HTMLInputElement>('[name=replace]:checked:not(:disabled)')?.value;
  const error = dialog.querySelector<HTMLElement>('[data-error]')!;
  const submit = dialog.querySelector<HTMLButtonElement>('[type=submit]')!;
  const update = () => {
    if (!dialog.open || busy) return;
    dialog.querySelector('[data-title]')!.textContent = l('Choose a channel to replace', '选择要覆盖的信道');
    dialog.querySelector('[data-frequency]')!.textContent = `${l('Channels full · add', '信道已满 · 加入')} ${formatFreqHz(candidate!.baseband.frequency_hz)} · ${candidate!.processors[0].mode.toUpperCase()}`;
    const targets = dialog.querySelector('[data-targets]')!, known = new Set<string>();
    for (const receiver of session.snapshot.receivers) {
      const uid = receiver.receiver_uid; known.add(uid);
      let option = [...targets.querySelectorAll<HTMLLabelElement>('label')].find(label => label.dataset.uid === uid);
      if (!option) {
        option = doc.createElement('label'); option.dataset.uid = uid; option.className = 'sdr-replace-option';
        const input = doc.createElement('input'); input.type = 'radio'; input.name = 'replace'; input.value = uid;
        const summary = doc.createElement('span'); summary.append(doc.createElement('strong'), doc.createElement('small'));
        option.append(input, summary); targets.append(option);
      }
      const config = receiver.confirmed;
      const recording = session.snapshot.recordings.some(task => task.receiver_uid === uid && ['preparing', 'waiting', 'recording', 'finalizing'].includes(task.state));
      option.querySelector('strong')!.textContent = `C${receiver.display_number} · ${formatFreqHz(config.baseband.frequency_hz)} · ${config.processors[0]?.mode.toUpperCase() ?? 'IQ'}${recording ? l(' · Recording', ' · 录制中') : ''}`;
      const name = config.name.trim();
      const customName = !/^C\d+$/.test(name) && !/^\d+(?:\.\d+)?\s*[kMG]?Hz(?:\s*·\s*[\w-]+)?$/i.test(name);
      const detail = option.querySelector('small')!;
      detail.textContent = customName ? name : ''; detail.hidden = !customName;
      option.title = customName ? name : '';
      option.querySelector('input')!.disabled = !!session.edits.get(uid)?.pending;
      revisions.set(uid, receiver.revision);
    }
    targets.querySelectorAll<HTMLLabelElement>('label').forEach(label => { if (!known.has(label.dataset.uid!)) label.remove(); });
    dialog.querySelectorAll<HTMLButtonElement>('[data-dismiss]').forEach(button => {
      button.title = l('Cancel', '取消'); button.setAttribute('aria-label', button.title);
      if (button.closest('footer')) button.textContent = button.title;
    });
    submit.textContent = l('Replace channel', '覆盖信道'); submit.disabled = !selected();
  };
  const apply = async (config: ReceiverConfig, target?: ReceiverState): Promise<void> => {
    const capture = session.snapshot.capture, bb = config.baseband;
    if (capture.state !== 'running' || capture.usable_bandwidth_hz === null || capture.parameters_state === 'unknown')
      throw new Error(l('Start or recover capture first.', '请先启动或恢复采集。'));
    if (Math.abs(bb.frequency_hz - capture.center_hz) + bb.passband_hz / 2 + bb.transition_hz > capture.usable_bandwidth_hz / 2)
      throw new Error(l('The full passband exceeds capture. Adjust the shared capture first.', '完整通带超出采集范围，请先调整采集中心或 IQ 带宽。'));
    const previous = new Set(session.snapshot.receivers.map(r => r.receiver_uid));
    const reply = await session.send(target ? { type: 'configure', request_id: session.requestId(), receiver_uid: target.receiver_uid,
      expected_revision: target.revision, config } : { type: 'create', request_id: session.requestId(), config });
    if (reply.fault) throw new Error(reply.fault.message);
    const receiver = target ? reply.snapshot.receivers.find(r => r.receiver_uid === target.receiver_uid)
      : reply.snapshot.receivers.find(r => !previous.has(r.receiver_uid) && r.confirmed.processors[0]?.id === config.processors[0]?.id);
    if (!receiver) throw new Error(l('Channel state changed. Try again.', '信道状态已变化，请重试。'));
    if (receiverHasAudio(session.snapshot, receiver.confirmed)) session.monitor(receiver.receiver_uid, { enabled: true });
    doc.dispatchEvent(new CustomEvent('websa:sdr-channels-changed'));
  };
  const close = () => { if (!busy) dialog.close(); };
  dialog.querySelectorAll('[data-dismiss]').forEach(button => button.addEventListener('click', close));
  dialog.addEventListener('keydown', event => event.stopPropagation());
  dialog.addEventListener('cancel', event => { if (busy) event.preventDefault(); });
  dialog.addEventListener('change', update);
  dialog.addEventListener('submit', event => {
    event.preventDefault(); if (busy || !candidate) return;
    const target = session.snapshot.receivers.find(r => r.receiver_uid === selected());
    if (!target || session.edits.get(target.receiver_uid)?.pending) return;
    if (target.revision !== revisions.get(target.receiver_uid)) { update(); return; }
    busy = true; error.textContent = '';
    dialog.querySelectorAll<HTMLInputElement | HTMLButtonElement>('input,button').forEach(control => { control.disabled = true; });
    submit.textContent = l('Applying…', '正在覆盖…');
    void apply(clone(candidate), target).then(() => { if (!disposed) dialog.close(); })
      .catch(reason => { if (!disposed) error.textContent = String(reason.message ?? reason); })
      .finally(() => {
        busy = false; if (disposed) return;
        dialog.querySelectorAll<HTMLInputElement | HTMLButtonElement>('input,button').forEach(control => { control.disabled = false; });
        update();
      });
  });
  const stop = session.subscribe(update), stopLang = onLangChange(update);
  return {
    async add(): Promise<string | null> {
      if (busy) return null;
      if (dialog.open) { dialog.focus(); return null; }
      const config = currentChannelConfig(session);
      // C1/current receiver is the tuning source. Repeated saves reuse an existing independent copy.
      const duplicate = session.snapshot.receivers.find(r => r.display_number !== currentSdrChannel()
        && Math.abs(r.confirmed.baseband.frequency_hz - config.baseband.frequency_hz) < 0.5
        && (['mode', 'bandwidth_hz', 'pcm_gain', 'agc', 'squelch_dbm'] as const)
          .every(field => r.confirmed.processors[0]?.[field] === config.processors[0]?.[field]));
      if (duplicate) {
        if (receiverHasAudio(session.snapshot, duplicate.confirmed) && duplicate.state === 'running') session.monitor(duplicate.receiver_uid, { enabled: true });
        return l(`Already added to C${duplicate.display_number}`, `已加入 C${duplicate.display_number}`);
      }
      if (session.snapshot.receivers.length < session.snapshot.capabilities.max_channels) {
        busy = true;
        try { await apply(config); return l('Channel added', '已加入信道'); } finally { busy = false; }
      }
      candidate = config; revisions.clear(); error.textContent = '';
      dialog.querySelectorAll<HTMLInputElement>('[name=replace]').forEach(input => { input.checked = false; });
      dialog.showModal(); update(); return null;
    },
    dispose() { disposed = true; stop(); stopLang(); dialog.remove(); },
  };
}
