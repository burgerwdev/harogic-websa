import { receiverRuntime } from './receiverRuntime';
import type { ReceiverState } from '../core/multichannel';
import { getLang } from '../core/i18n';
import { copyModeControls, type ModeControls } from '../core/receiverControls';
import { receiverControlFields, readReceiverControlFields } from './receiverControlFields';
const l = (en: string, zh: string) => getLang() === 'zh' ? zh : en;
interface Context { service_id: string; receiver_uid: string; revision: string; action_token: string }
interface Row extends ModeControls, Pick<ReceiverState,'processing_runtime'|'processing_state'|'processing_fault'> { channel:number; mode: string; control?: Context; control_receipts?: {status: string; request_id: string}[] }
let service = '', active = '', revision = '', dirty = false, language = '';
const drafts = new Map<string, {html: ModeControls; revision: string}>();
/** Invalidate closures as well as the visible editor when its receiver disappears. */
export function clearMainReceiverControls(): void {
  document.getElementById('sdr-mode-settings')?.remove();
  drafts.delete(active); active = ''; revision = ''; dirty = false;
}
/** The compatibility panel uses the same fields/serializer as the workbench. */
export function syncMainReceiverControls(row: Row, send: (command: Record<string, unknown>) => void): void {
  const context = row.control, anchor = document.getElementById('sdr-ifbw-group')?.parentElement;
  if (!context?.receiver_uid) { clearMainReceiverControls(); return; }
  if (!anchor) return;
  let form = document.getElementById('sdr-mode-controls') as HTMLFormElement | null;
  if (!form) {
    active='';revision='';dirty=false;language='';
    const details = document.createElement('details'); details.id = 'sdr-mode-settings';
    details.innerHTML = '<summary></summary><form id="sdr-mode-controls"></form>'; anchor.after(details);
    form = details.querySelector('form')!;
    form.addEventListener('input', () => { dirty = true; });
    form.addEventListener('change', () => { dirty = true; });
    form.addEventListener('submit', event => {
      event.preventDefault();
      if (!form!.isConnected) return;
      const captured = JSON.parse(form!.dataset.context!) as Context;
      send({cmd:'SET_SDR_CHANNEL',...captured,expected_revision:form!.dataset.revision,
        ...readReceiverControlFields(form!),request_id:`main-mode-${crypto.randomUUID()}`});
      // Keep the draft until the server confirms its revision; rejection remains editable.
    });
    form.addEventListener('click', event => {
      if (!form!.isConnected) return;
      const button = (event.target as Element).closest<HTMLButtonElement>('[data-action]'); if (!button) return;
      const captured = JSON.parse(form!.dataset.context!) as Context;
      if (button.dataset.action === 'recover-processing') {send({cmd:'SET_SDR_CHANNEL',...captured,recover:true});return;}
      if (button.dataset.action === 'restore-mode-controls') {
        drafts.delete(captured.receiver_uid); dirty = false; revision = ''; return;
      }
      if (button.dataset.action === 'radiofax_new_page' || button.dataset.action === 'radiofax_reset_correction') {
        send({cmd:'SET_SDR_CHANNEL',...captured,expected_revision:form!.dataset.revision,
          [button.dataset.action]:true,action_id:crypto.randomUUID(),request_id:`main-action-${crypto.randomUUID()}`});
      }
    });
  }
  if (service !== context.service_id) {drafts.clear();dirty = false;active = '';service = context.service_id;}
  if (active && active !== context.receiver_uid && dirty) drafts.set(active,{html:readReceiverControlFields(form),revision:form.dataset.revision!});
  if (active !== context.receiver_uid) {dirty = drafts.has(context.receiver_uid);revision = '';}
  const serializedContext = JSON.stringify({...context,channel:row.channel});
  if (form.dataset.context !== serializedContext) form.dataset.context = serializedContext;
  const lang = getLang();
  const focused = form.ownerDocument.activeElement;
  const editing = !!focused && form.contains(focused) && !!focused.closest('input,select,textarea');
  // A status revision can change while a native menu is open, before change/input
  // marks the form dirty. Keep that editor mounted until the user leaves it.
  if (revision !== context.revision && !dirty && !editing || active !== context.receiver_uid || language !== lang) {
    const draft = drafts.get(context.receiver_uid);
    const values = dirty && active === context.receiver_uid ? readReceiverControlFields(form) : draft?.html ?? row;
    form.innerHTML = receiverControlFields(values,row.mode)
      + `<button class="btn" type="submit" title="${l('Apply mode settings to this receiver', '应用到此信道')}">${l('Apply mode settings','应用模式设置')}</button>`
      + `<button class="btn" type="button" data-action="restore-mode-controls" title="${l('Restore confirmed settings','还原已确认设置')}">${l('Reset','还原')}</button><p data-control-state role="status"></p><div data-main-runtime></div>`;
    form.dataset.revision = draft?.revision ?? context.revision;
    revision = context.revision;
  }
  // A changed confirmation can acknowledge a successful edit without erasing a
  // rejected/stale draft. Compare actual values, never the HTTP acceptance alone.
  if (dirty && JSON.stringify(readReceiverControlFields(form)) === JSON.stringify(copyModeControls(row))) {dirty = false;drafts.delete(context.receiver_uid);form.dataset.revision=context.revision;}
  active = context.receiver_uid;language = lang;
  form.parentElement!.querySelector('summary')!.textContent = l('Mode settings', '模式设置');
  const runtime = row.processing_runtime;
  const runtimeHost=form.querySelector('[data-main-runtime]')!;const diagnosticsOpen=runtimeHost.querySelector('details')?.open;
  runtimeHost.innerHTML = receiverRuntime({...row,processing_state:row.processing_state ?? runtime?.state as ReceiverState['processing_state']});
  if (diagnosticsOpen) runtimeHost.querySelector('details')!.open=true;
  const receipt = row.control_receipts?.at(-1);
  form.querySelector('[data-control-state]')!.textContent = dirty && form.dataset.revision !== context.revision
    ? l('Channel updated elsewhere; review the draft.', '此信道已在其他位置更新，请检查草稿。') : receipt?.status === 'queued' ? l('Accepted · waiting for data', '已接纳 · 等待数据')
    : receipt?.status === 'applied' ? l('Applied at a source block boundary', '已在源数据块边界应用') : '';
}
