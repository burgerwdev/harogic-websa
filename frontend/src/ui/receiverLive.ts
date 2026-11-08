import type { ReceiverState, RecordingTask } from '../core/multichannel';
import { getLang } from '../core/i18n';
const l=(en:string,zh:string)=>getLang()==='zh'?zh:en;
const esc=(v:unknown)=>String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'})[c]!);
export function liveStatus(r:ReceiverState): {label:string; kind:string; hint:string} {
  const recovery=r.reception, actual=r.processing_runtime?.observed_receiver;
  if (!r.confirmed.enabled) return {label:l('Stopped','已停止'),kind:'stopped',hint:l('Start this channel to receive.','启用此信道开始接收。')};
  if (r.coverage?.state==='out_of_band') return {label:l('Out of band · waiting for coverage','带外 · 等待覆盖'),kind:'out_of_band',hint:l('Frequency, mode and listening settings are retained. Reception resumes when the full passband and transition fit the actual capture.','频率、模式及监听设置已保留；实际采集范围完整覆盖通带及过渡带后自动恢复。')};
  if (recovery?.state==='recovering') return {label:l('Recovering','恢复中'),kind:'recovering',hint:l('Waiting to resume from a new signal boundary.','正在等待从新的信号边界续收。')};
  if (recovery?.state==='preparing' || r.processing_state==='preparing') return {label:l('Preparing reception','正在准备接收'),kind:'recovering',hint:l('Preparing fresh signal processing; reception is not confirmed yet.','正在重新准备信号处理，尚未确认恢复接收。')};
  if (recovery?.state==='stopped' || r.fault || r.processing_state==='failed') return {label:recovery?.policy==='strict' && recovery.last_interruption.kind!=='device' && recovery.last_interruption.kind!=='internal'?l('Stopped at interruption','已在中断处停止'):l('Reception fault','接收故障'),kind:'fault',hint:recovery?.manual_recovery_allowed?l('Review the interruption, then resume when ready.','查看中断记录后，可重新开始接收。'):l('Check the source and diagnostic details before restarting.','请检查信号源及诊断详情，再重新启动。')};
  if (r.state!=='running' || r.processing_state==='waiting' && r.confirmed.processors.length>0) return {label:l('Waiting for input','等待输入'),kind:'waiting',hint:l('No valid input has arrived yet.','尚未收到有效输入。')};
  if (actual?.squelch?.open===false) return {label:l('Waiting for signal','等待信号'),kind:'waiting',hint:l('Reception is active; squelch has muted the audio.','接收正在进行，静噪已关闭音频输出。')};
  if (actual?.protocol_waiting) return {label:l('Waiting for protocol result','等待协议结果'),kind:'waiting',hint:l('Receiving samples for the next complete protocol window.','正在接收样本，等待下一个完整协议窗口。')};
  if (r.processing_state==='backlogged') return {label:l('Receiving · processing behind','接收中 · 处理积压'),kind:'waiting',hint:l('Processing is behind the incoming signal.','处理暂时落后于输入信号。')};
  return {label:l('Receiving normally','正常接收'),kind:'healthy',hint:r.confirmed.processors.length?'':l('Raw IQ reception is active.','正在接收原始 IQ。')};
}
export function receiverLive(r:ReceiverState,tasks:RecordingTask[],pending=false,expanded=false):string {
  const status=liveStatus(r), recovery=r.reception, gap=recovery?.last_interruption;
  const interrupted=tasks.filter(t=>t.receiver_uid===r.receiver_uid && t.state==='failed' && ['SOURCE_FAILED','PROCESSING_FAILED'].includes(t.fault?.code??''));
  const last=interrupted.at(-1);
  const reason=gap ? gap.kind==='overload'?l('This channel fell behind.','本路处理积压。'):gap.kind==='source_gap'?l('The shared signal source was interrupted; all active channels were affected.','共享信号源中断，影响当时所有活动信道。'):gap.kind==='device'?l('The receiving device reported an error.','接收设备报告错误。'):l('Signal processing reported an internal error.','信号处理报告内部错误。') : '';
  return `<section class="mc-live-status" data-key="live-status-${esc(r.receiver_uid)}" data-live-state="${status.kind}">
    <div class="mc-live-heading"><strong role="status" aria-live="polite">${esc(status.label)}</strong>${recovery?.manual_recovery_allowed?`<button class="btn" type="button" data-action="recover-processing" title="${l('Start a new reception after the recorded gap','保留缺口记录，重新开始接收')}" ${pending?'disabled aria-busy="true"':''}>${pending?l('Submitting…','正在提交…'):l('Resume reception','重新接收')}</button>`:''}</div>
    <p class="mc-live-facts">${(r.confirmed.baseband.frequency_hz/1e6).toLocaleString(undefined,{maximumFractionDigits:6})} MHz <span>·</span> ${esc(r.confirmed.processors[0]?.mode.toUpperCase()??'IQ')} <span>·</span> ${l('Passband','通带')} ${(r.confirmed.baseband.passband_hz/1000).toLocaleString()} kHz</p>
    ${status.hint?`<p class="mc-live-hint">${esc(status.hint)}</p>`:''}
    ${gap?`<p class="mc-live-interruption">${reason} ${recovery?.state==='recovering'?l('Retry scheduled.','正在等待重试。'):''} ${l('Interruptions','累计中断')} ${recovery!.interruptions}${gap.recovered_at_ms?` · ${l('Last resumed','最近续收')} <time datetime="${new Date(gap.recovered_at_ms).toISOString()}">${esc(new Date(gap.recovered_at_ms).toLocaleTimeString())}</time>`:''}</p>`:''}
    ${last?`<p class="mc-error" data-key="recording-interrupted">${l('Recording ended at the interruption. Its valid prefix is retained; resuming reception does not restart recording.','录制已在中断处结束，有效前缀已保留；恢复接收不会自动重启录制。')} <button type="button" data-action="tab-recording" title="${l('Review the ended recording and its valid prefix','查看已结束录制及有效前缀')}">${l('View recording','查看录制')}</button></p>`:''}
    <label class="mc-reception-policy">${l('Reception policy','接收策略')} <select data-continuity aria-label="${l('Reception policy','接收策略')}" ${pending?'disabled':''}><option value="continuous" ${(r.confirmed.continuity??'continuous')==='continuous'?'selected':''}>${l('Continuous reception','持续接收')}</option><option value="strict" ${r.confirmed.continuity==='strict'?'selected':''}>${l('Strict continuity','严格连续')}</option></select></label>
    <p class="mc-live-hint">${r.confirmed.continuity==='strict'?l('Stop at an unexpected gap. Planned out-of-band pauses resume when covered.','出现异常缺口即停止；计划内的带外暂停在覆盖后恢复。'):l('Attempt reception recovery after unexpected gaps. Recordings end at faults; planned coverage pauses wait and resume a new segment.','异常中断后尝试续收并保留缺口记录。录制遇故障结束；计划内的带外暂停进入等待，覆盖后另起片段。')}</p>
    <details data-section="live-diagnostics" ${expanded?'open':''}><summary>${l('Diagnostic details','诊断详情')}</summary><pre>${esc(JSON.stringify({stream:r.stream,clock:r.clock,reception:recovery,processing:r.processing_runtime,fault:r.fault,processing_fault:r.processing_fault},null,2))}</pre></details>
  </section>`;
}
