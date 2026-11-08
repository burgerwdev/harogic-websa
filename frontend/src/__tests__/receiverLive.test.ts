import {afterEach,expect,it} from 'vitest';
import {FixtureReceiverAdapter} from '../dev/fixtureReceiverAdapter';
import {receiverLive,liveStatus} from '../ui/receiverLive';
import {setLang} from '../core/i18n';
import type {ReceptionState,RecordingTask} from '../core/multichannel';
import {WorkbenchSession} from '../ui/workbenchSession';
const adapter=new FixtureReceiverAdapter();
const base=adapter.snapshot().receivers[0];adapter.dispose();
afterEach(()=>{document.body.replaceChildren();sessionStorage.clear();});
const recovery=(state:ReceptionState['state']):ReceptionState=>({policy:'continuous',state,interruptions:1,attempts:1,retry_limit:3,retry_in_ms:500,manual_recovery_allowed:state==='stopped',last_error:null,last_interruption:{kind:'overload',scope:'processing',reason:'queue full',at_ms:100,recovered_at_ms:state==='receiving'?200:null,before:null,after:null,missing_input_samples:null}});
it('distinguishes healthy, squelched, protocol waiting and IQ-only reception',()=>{
 setLang('en');const r=structuredClone(base);r.state='running';r.processing_state='active';
 expect(liveStatus(r).kind).toBe('healthy');
 r.processing_runtime={state:'active',processing_generation:'2',oldest_input_wait_ns:0,input_held_bytes:0,input_held_media_ns:0,observed_receiver:{nr_requested:'off',nr_effective:'off',nr_state:'off',squelch:{open:false,gain:0}}};
 expect(liveStatus(r).label).toBe('Audio muted by squelch');
 r.processing_runtime.observed_receiver!.squelch=null;r.processing_runtime.observed_receiver!.protocol_waiting=true;
 expect(liveStatus(r).label).toBe('Waiting for protocol result');
 r.confirmed.processors=[];r.processing_state='disabled';r.processing_runtime.observed_receiver!.squelch={open:false,gain:0};r.processing_runtime.observed_receiver!.protocol_waiting=false;
 expect(liveStatus(r).kind).toBe('healthy');
});
it('separates queued recovery, preparation, resumed listening and ended recording',()=>{
 setLang('zh');const r=structuredClone(base);r.reception=recovery('recovering');r.processing_state='failed';
 expect(liveStatus(r).label).toBe('恢复中');expect(receiverLive(r,[])).not.toContain('data-action="recover-processing"');
 r.reception=recovery('preparing');expect(liveStatus(r).label).toBe('正在准备接收');
 r.reception=recovery('receiving');r.processing_state='active';r.state='running';
 const tasks=[{receiver_uid:r.receiver_uid,state:'failed',fault:{code:'PROCESSING_FAILED'}}] as RecordingTask[];
 document.body.innerHTML=receiverLive(r,tasks);
 expect(document.body.textContent).toContain('正常接收');expect(document.body.textContent).toContain('录制已在中断处结束');
 expect(document.querySelector('details')!.open).toBe(false);
 expect(document.querySelector('select')!.getAttribute('aria-label')).toBe('接收策略');
 r.reception=recovery('stopped');document.body.innerHTML=receiverLive(r,[],true,true);
 expect(document.querySelector<HTMLButtonElement>('[data-action="recover-processing"]')!.disabled).toBe(true);
 expect(document.querySelector('details')!.open).toBe(true);
 r.reception.last_interruption.kind='device';r.reception.manual_recovery_allowed=false;
 expect(receiverLive(r,[])).not.toContain('data-action="recover-processing"');
});

it.each(['adsb','ais','gps','ism','meteor','drm','drmplus','ft8','ft4','wspr','rtty','aprs','future-digital'])('%s reception is independent of companion audio squelch',mode=>{
 setLang('en');const r=structuredClone(base);r.state='running';r.processing_state='active';
 r.confirmed.purpose='digital';r.confirmed.processors[0]={...r.confirmed.processors[0],kind:'decoder',mode};
 r.processing_runtime={state:'active',processing_generation:'2',oldest_input_wait_ns:0,input_held_bytes:0,input_held_media_ns:0,observed_receiver:{nr_requested:'off',nr_effective:'off',nr_state:'off',squelch:{open:false,gain:0},protocol_waiting:false}};
 expect(liveStatus(r).label).toBe('Receiving normally');
 r.processing_runtime.observed_receiver!.protocol_waiting=true;
 expect(liveStatus(r).label).toBe('Waiting for protocol result');
 r.processing_state='backlogged';expect(liveStatus(r).label).toBe('Receiving · processing behind');
 r.processing_state='failed';expect(liveStatus(r).kind).toBe('fault');
 r.reception=recovery('recovering');expect(liveStatus(r).kind).toBe('recovering');
});

it('clears only recovered notices, survives refreshes and shows a new interruption',()=>{
 setLang('en');const adapter=new FixtureReceiverAdapter();
 const r=structuredClone(base);r.state='running';r.processing_state='active';r.reception=recovery('receiving');
 const snapshot=adapter.snapshot();snapshot.service_id='notice-test';snapshot.receivers=[r];
 adapter.snapshot=()=>structuredClone(snapshot);
 let session=new WorkbenchSession(adapter);
 expect(receiverLive(r,[])).toContain('data-action="clear-interruption"');
 session.clearInterruption(r.receiver_uid);expect(session.interruptionCleared(r.receiver_uid)).toBe(true);
 expect(receiverLive(r,[],false,false,session.interruptionCleared(r.receiver_uid))).not.toContain('class="mc-live-interruption"');
 expect(receiverLive(r,[],false,false,true)).toContain('queue full'); // Diagnostics retain the record.
 session.dispose();session=new WorkbenchSession(adapter);
 expect(session.interruptionCleared(r.receiver_uid)).toBe(true);
 session.snapshot.receivers[0].reception!.interruptions++;
 expect(session.interruptionCleared(r.receiver_uid)).toBe(false);
 r.reception=recovery('stopped');
 expect(receiverLive(r,[],false,false,true)).toContain('class="mc-live-interruption"');
 expect(receiverLive(r,[])).not.toContain('data-action="clear-interruption"');
 session.snapshot.receivers=[r];session.clearInterruption(r.receiver_uid);
 expect(session.interruptionCleared(r.receiver_uid)).toBe(false);
 session.dispose();adapter.dispose();
});
