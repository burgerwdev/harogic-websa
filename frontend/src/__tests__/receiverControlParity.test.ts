import { afterEach, expect, it, vi } from 'vitest';
import { copyModeControls } from '../core/receiverControls';
import { receiverControlFields, readReceiverControlFields, syncReceiverControlFields } from '../ui/receiverControlFields';
import { syncMainReceiverControls } from '../ui/mainReceiverControls';
import { ReceiverModeMemory } from '../ui/receiverModeMemory';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';

afterEach(() => {document.body.replaceChildren();sessionStorage.clear();});
it('round trips every persisted mode field, and exposes controls by their mode in either editor', () => {
  const controls = {...copyModeControls(undefined),am_sync:true,pitch:1350,deemph_us:75,stereo:false,audio_rate:24000,
    radiofax_acquisition:'manual',radiofax_ioc:'576',radiofax_lpm:'120',radiofax_modulation:'am',radiofax_polarity:'inverted',
    radiofax_clock_recovery:'off',radiofax_phase_pixels:7.5,radiofax_clock_ppm:-23};
  const form=document.createElement('form');form.innerHTML=receiverControlFields(controls,'am');document.body.append(form);
  expect(readReceiverControlFields(form)).toEqual(controls);
  expect(form.querySelector<HTMLElement>('[data-mode-field=am_sync]')!.hidden).toBe(false);
  expect(form.querySelector<HTMLElement>('[data-mode-field=stereo]')!.hidden).toBe(true);
  syncReceiverControlFields(form,'wfm');expect(form.querySelector<HTMLElement>('[data-mode-field=stereo]')!.hidden).toBe(false);
  syncReceiverControlFields(form,'radiofax');expect(form.querySelector<HTMLElement>('[data-mode-actions]')!.hidden).toBe(false);
  expect(readReceiverControlFields(form)).not.toHaveProperty('radiofax_new_page');
});
it('main C8 submission retains UID/revision and all mode fields when selection changes before its response', () => {
  document.body.innerHTML='<div><div id="sdr-ifbw-group"></div></div>';
  const send=vi.fn(), row={...copyModeControls(undefined),channel:8,mode:'wfm',control:{service_id:'parity-service',receiver_uid:'uid-eight',revision:'11',action_token:'100'}};
  syncMainReceiverControls(row,send);
  const form=document.querySelector<HTMLFormElement>('#sdr-mode-controls')!;
  const stereo=form.elements.namedItem('mode-stereo') as HTMLInputElement;stereo.checked=false;stereo.dispatchEvent(new Event('input',{bubbles:true}));
  form.dispatchEvent(new Event('submit',{cancelable:true}));
  syncMainReceiverControls({...row,channel:1,control:{...row.control,receiver_uid:'uid-one'}},send);
  expect(send.mock.calls[0][0]).toMatchObject({channel:8,receiver_uid:'uid-eight',service_id:'parity-service',expected_revision:'11',stereo:false,audio_rate:48000});
  syncMainReceiverControls(row,send);expect((form.elements.namedItem('mode-stereo') as HTMLInputElement).checked).toBe(false);
  expect(send).toHaveBeenCalledTimes(1);
});
it('confirmed mode memory preserves the complete model across reload and excludes instant actions', () => {
  const adapter=new FixtureReceiverAdapter(),snapshot=adapter.snapshot();adapter.dispose();
  snapshot.service_id='parity-memory';snapshot.capabilities.mode_descriptors=[{id:'am',kind:'analog',label:'AM',provider:'native',available:true,
    unavailableReason:null,defaultBandwidthHz:6000,minimumRateHz:0,audioCompanion:null,outputs:['audio'],sideband:'symmetric'}];
  const processor=snapshot.receivers[0].confirmed.processors[0];processor.mode='am';Object.assign(processor,copyModeControls(undefined),{am_sync:true,pitch:1200,radiofax_clock_ppm:44});
  const memory=new ReceiverModeMemory();memory.observe(snapshot);
  const reloaded=new ReceiverModeMemory();reloaded.observe(snapshot);
  expect(reloaded.get(snapshot.receivers[0].receiver_uid,'am')).toMatchObject({am_sync:true,pitch:1200,radiofax_clock_ppm:44,stereo:true,audio_rate:48000});
  expect(JSON.stringify(reloaded.get(snapshot.receivers[0].receiver_uid,'am'))).not.toContain('radiofax_new_page');
});

it('a shared memory refuses an older panel confirmation after a newer workbench revision', () => {
  const adapter=new FixtureReceiverAdapter(),snapshot=adapter.snapshot();adapter.dispose();
  snapshot.service_id='shared-revisions';snapshot.capabilities.mode_descriptors=[{id:'am',kind:'analog',label:'AM',provider:'native',available:true,
    unavailableReason:null,defaultBandwidthHz:6000,minimumRateHz:0,audioCompanion:null,outputs:['audio'],sideband:'symmetric'}];
  const row=snapshot.receivers[0];row.confirmed.processors[0].mode='am';row.revision='8';row.confirmed.processors[0].am_sync=true;
  const memory=new ReceiverModeMemory();memory.observe(snapshot);
  const old=structuredClone(snapshot);old.receivers[0].revision='7';old.receivers[0].confirmed.processors[0].am_sync=false;memory.observe(old);
  expect(memory.get(row.receiver_uid,'am')?.am_sync).toBe(true);
});
it('runtime UI uses observed model state, distinguishes waiting windows and offers per-UID recovery', async () => {
  const {receiverRuntime}=await import('../ui/receiverRuntime');
  const html=receiverRuntime({processing_state:'failed',processing_fault:{error:'C8 input retained two seconds',generation:7,receiver_uid:'eight',service_id:'test'},
    processing_runtime:{state:'failed',processing_generation:'7',input_held_bytes:16384,input_held_media_ns:2000000000,oldest_input_wait_ns:600000000,
      observed_receiver:{nr_requested:'dfn',nr_effective:null,nr_state:'failed',protocol_waiting:false}}});
  expect(html).toContain('data-action="recover-processing"');expect(html).toContain('C8 input retained two seconds');expect(html).not.toContain('NR dfn (ready)');
  const waiting=receiverRuntime({processing_state:'active',processing_runtime:{state:'active',processing_generation:'8',input_held_bytes:0,input_held_media_ns:0,oldest_input_wait_ns:0,
    observed_receiver:{nr_requested:'off',nr_effective:'off',nr_state:'off',protocol_waiting:true}}});
  expect(waiting).toContain('protocol');expect(waiting).not.toContain('data-action="recover-processing"');
});
