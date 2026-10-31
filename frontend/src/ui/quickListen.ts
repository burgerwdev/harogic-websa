import { getLang, onLangChange } from '../core/i18n';
import { convertUnitValue } from '../core/units';
import { applyListen, defaultListenTarget, LISTEN_MODES, planListen, type ListenPlan } from './workbenchListening';
import type { WorkbenchSession } from './workbenchSession';

const l = (en: string, zh: string) => getLang() === 'zh' ? zh : en;
function errorText(error: { code: string; message: string }): string {
  const messages: Record<string, string> = {
    FREQUENCY: '请输入有效频率。', MODE: '该听音模式当前不可用。', TARGET: '请选择听音信道。',
    LIMIT: '当前路数已满，请选择已有听音信道。', RF_RANGE: '频点及完整通带超出共享采集范围，请先调整采集设置。',
    NO_SOURCE: '接收已中断，请先恢复接收。', BUSY: '信道请求正在处理，请稍候。',
    STALE_PREVIEW: '状态已变化，请查看更新后的影响说明后重试。',
    CONFIRMATION: '此信道已被其他窗口修改，请重新选择监听频点。',
    FIXTURE_REJECTED: '模拟请求被拒绝，原接收和监听配置保持不变。',
  };
  return `${error.code}: ${getLang() === 'zh' ? messages[error.code] ?? error.message : error.message}`;
}
/** A transient frequency entry; the workbench remains a separate floating view. */
export function mountQuickListen(session: WorkbenchSession, host: HTMLElement): { open(hz: number): void; dispose(): void } {
  const doc = host.ownerDocument;
  const dialog = doc.createElement('dialog'); dialog.className = 'mc-quick-listen'; dialog.dataset.quickListen = '';
  dialog.innerHTML = '<form><header><strong data-title></strong><button type="button" data-dismiss aria-label="Close">×</button></header><p data-fixture class="mc-notice"></p><label><span data-frequency-label></span><span class="mc-frequency-input"><input name="frequency" type="number" min="0" step="any" required><select name="unit"><option>MHz</option><option>kHz</option><option>Hz</option></select></span></label><label><span data-mode-label></span><select name="mode"></select></label><label><span data-target-label></span><select name="target"></select></label><div data-impact aria-live="polite"></div><p data-error class="mc-error" role="alert"></p><button type="submit" data-listen-submit></button></form>';
  host.appendChild(dialog);
  const input = dialog.querySelector<HTMLInputElement>('[name=frequency]')!;
  const unit = dialog.querySelector<HTMLSelectElement>('[name=unit]')!;
  const mode = dialog.querySelector<HTMLSelectElement>('[name=mode]')!;
  const target = dialog.querySelector<HTMLSelectElement>('[name=target]')!;
  const submit = dialog.querySelector<HTMLButtonElement>('[data-listen-submit]')!;
  const impact = dialog.querySelector<HTMLElement>('[data-impact]')!;
  const error = dialog.querySelector<HTMLElement>('[data-error]')!;
  let previousUnit = 'MHz', preferredMode: string | null = null, busy = false, disposed = false, plan: ListenPlan | null = null;
  const frequency = () => input.value.trim() ? convertUnitValue(Number(input.value), unit.value, 'Hz') : NaN;
  const update = (): void => {
    if (!dialog.open || busy) return;
    const previous = target.value;
    target.replaceChildren();
    const next = doc.createElement('option'); next.value = 'new';
    next.disabled = session.snapshot.receivers.length >= session.snapshot.capabilities.max_channels;
    next.textContent = next.disabled ? l('New channel · limit reached', '新建听音信道 · 路数已满') : l('New listening channel', '新建听音信道');
    target.appendChild(next);
    for (const receiver of session.snapshot.receivers.filter(r => r.confirmed.purpose === 'listen')) {
      const option = doc.createElement('option'); option.value = receiver.receiver_uid;
      option.textContent = `C${receiver.display_number} · ${receiver.confirmed.name} · ${receiver.confirmed.baseband.frequency_hz / 1e6} MHz`;
      target.appendChild(option);
    }
    target.value = previous || defaultListenTarget(session, frequency());
    if (!target.value) target.value = defaultListenTarget(session, frequency());
    const oldMode = preferredMode ?? mode.value; preferredMode = null; mode.replaceChildren();
    for (const name of LISTEN_MODES.filter(m => session.snapshot.capabilities.modes.includes(m))) {
      const option = doc.createElement('option'); option.value = name; option.textContent = name.toUpperCase(); mode.appendChild(option);
    }
    if (oldMode) mode.value = oldMode;
    plan = planListen(session, { frequency_hz: frequency(), target: target.value, mode: mode.value });
    dialog.querySelector('[data-title]')!.textContent = l('Add frequency to listening', '将频点加入监听');
    dialog.querySelector('[data-fixture]')!.textContent = l('FIXTURE · simulated routing, no speaker output', 'FIXTURE · 模拟监听路由，不输出实际声音');
    dialog.querySelector('[data-frequency-label]')!.textContent = l('Frequency', '频率');
    dialog.querySelector('[data-mode-label]')!.textContent = l('Mode', '解调');
    dialog.querySelector('[data-target-label]')!.textContent = l('Use channel', '使用信道');
    const receiver = session.snapshot.receivers.find(r => r.receiver_uid === target.value);
    const same = !plan.command && !plan.fault;
    submit.textContent = same ? l('Enable listening', '开启监听') : target.value === 'new' ? l('Add and listen', '新增并监听') : l('Tune and listen', '调谐并监听');
    submit.disabled = !!plan.fault;
    const lines = [same ? l('Already receiving this frequency. Only the local listening route changes.', '此信道已接收该频点，仅开启本地监听，不重启接收。')
      : receiver ? `${l('Tune only', '仅调谐')} C${receiver.display_number}: ${receiver.confirmed.baseband.frequency_hz / 1e6} → ${frequency() / 1e6} MHz`
        : l('Adds an independent listening channel to the local mix.', '新增独立听音信道，并加入本地混听。')];
    if (next.disabled) lines.push(l('All channels occupied. Retune a listening channel; digital and IQ channels stay intact.', '三路已占用，可调谐已有听音信道；数字和 IQ 信道保持原配置。').replace('三路', `${session.snapshot.capabilities.max_channels} 路`));
    if (plan.affected_tracks.length) lines.push(`${l('Recording continues with new segments', '记录继续，并为以下轨道建立新片段')}: ${[...new Set(session.snapshot.recordings.flatMap(t => t.tracks).filter(track => plan!.affected_tracks.includes(track.track_id)).map(track => track.kind.toUpperCase()))].join(' / ')}`);
    if (session.monitorMode === 'follow') lines.push(l('Switches to manual mixing; the channel being edited stays selected.', '切换为手动混听，保留当前正在编辑的信道。'));
    if (session.masterGain === 0 || (receiver && (session.route(receiver.receiver_uid).gain === 0 || [...session.routes].some(([uid, route]) => route.solo && uid !== receiver.receiver_uid)))) lines.push(l('Existing volume and Solo settings still apply.', '现有音量与 Solo 设置仍然生效。'));
    impact.replaceChildren(...lines.map(line => { const p = doc.createElement('p'); p.textContent = line; return p; }));
    error.textContent = plan.fault ? errorText(plan.fault) : '';
  };
  const close = () => { if (!busy) dialog.close(); };
  dialog.querySelector('[data-dismiss]')!.addEventListener('click', close);
  dialog.addEventListener('cancel', event => { if (busy) event.preventDefault(); });
  dialog.addEventListener('input', update);
  unit.addEventListener('change', () => {
    if (input.value.trim()) input.value = String(convertUnitValue(Number(input.value), previousUnit, unit.value)); previousUnit = unit.value; update();
  });
  target.addEventListener('change', () => {
    const receiver = session.snapshot.receivers.find(r => r.receiver_uid === target.value);
    preferredMode = receiver?.confirmed.processors.find(p => p.kind === 'demodulator')?.mode ?? 'nfm'; update();
  });
  mode.addEventListener('change', update);
  dialog.addEventListener('submit', event => {
    event.preventDefault(); if (busy || !plan || plan.fault) return;
    const confirmedPlan = plan; busy = true;
    dialog.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>('input,select,button').forEach(el => { el.disabled = true; });
    submit.textContent = l('Request pending…', '请求中…');
    void applyListen(session, confirmedPlan).then(result => {
      busy = false; if (disposed) return;
      dialog.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>('input,select,button').forEach(el => { el.disabled = false; });
      if (result.fault) { update(); error.textContent = errorText(result.fault); return; }
      const receiver = session.snapshot.receivers.find(r => r.receiver_uid === result.receiver_uid)!;
      host.querySelector<HTMLElement>('[data-listen-status]')!.textContent = `${receiver.confirmed.baseband.frequency_hz / 1e6} MHz → C${receiver.display_number} · ${session.audible(receiver.receiver_uid) ? l('Listening route enabled (fixture)', '已加入监听（模拟）') : l('Added; check volume / Solo (fixture)', '已加入，请检查音量 / Solo（模拟）')}`;
      dialog.close();
    });
  });
  const stop = session.subscribe(update), stopLang = onLangChange(update);
  return { open: hz => {
    if (busy) return;
    input.value = String(hz / 1e6); unit.value = previousUnit = 'MHz';
    const chosen = defaultListenTarget(session, hz); target.value = chosen;
    const receiver = session.snapshot.receivers.find(r => r.receiver_uid === chosen);
    preferredMode = receiver?.confirmed.processors.find(p => p.kind === 'demodulator')?.mode ?? 'nfm';
    if (!dialog.open) dialog.showModal(); update(); input.focus(); input.select();
  }, dispose: () => { disposed = true; stop(); stopLang(); dialog.remove(); } };
}
