import type { ReceiverState } from '../core/multichannel';
import { getLang } from '../core/i18n';
import { copyDetailsButton } from './copyDetails';
const l = (en:string,zh:string) => getLang()==='zh' ? zh : en;
export function processingLabel(state: string | undefined): string {
  return ({disabled:l('Processing inactive','处理未启用'),waiting:l('Waiting for input','等待输入'),preparing:l('Preparing selected model','正在准备所选模型'),
    active:l('Processing','处理中'),backlogged:l('Processing behind input','处理落后于输入'),retiring:l('Finishing previous processing','正在结束原处理'),failed:l('Processing failed','处理失败')} as Record<string,string>)[state??'']??'';
}
const esc=(value:unknown)=>String(value??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'})[c]!);
export function receiverRuntime(row: Pick<ReceiverState,'processing_state'|'processing_runtime'|'processing_fault'|'reception'>, expanded = false, detail?: ReceiverState): string {
  const runtime=row.processing_runtime,actual=runtime?.observed_receiver,fault=row.processing_fault??runtime?.fault;
  if (!row.processing_state&&!runtime) return '';
  const state=row.processing_state??runtime?.state;
  // Older services omit the supervisor. A current service's explicit null
  // means no interruption is published yet, so wait for its recovery decision.
  const canRecover=row.reception===undefined || row.reception?.manual_recovery_allowed===true;
  return `<section data-processing-runtime role="status"><span>${esc(processingLabel(state))}</span>
    ${actual?.nr_effective && actual.nr_effective!=='off' ? `<span> · NR ${esc(actual.nr_effective)} (${esc(actual.nr_state)})</span>`:''}
    ${actual?.protocol_waiting ? `<span> · ${l('Collecting protocol window','正在积累协议窗口')}</span>`:''}
    ${fault ? `<p class="mc-error">${esc(fault.error)}${canRecover ? ` <button class="btn" type="button" data-action="recover-processing" title="${l('Retry only this processing branch; IQ continues','仅重试本路处理，IQ继续')}">${l('Retry processing','重试处理')}</button>`:''}</p>`:''}
    <details data-section="processing-diagnostics" ${expanded?"open":""}><summary>${l('Processing details','处理详情')}</summary>${copyDetailsButton()}<pre style="white-space:pre-wrap;overflow-wrap:anywhere">${expanded?esc(JSON.stringify(detail?{runtime:detail.processing_runtime,statistics:detail.processing_statistics}:runtime,null,2)):""}</pre></details></section>`;
}
