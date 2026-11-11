import { afterEach, expect, it, vi } from 'vitest';
import { copyModeControls } from '../core/receiverControls';
import { receiverControlFields, readReceiverControlFields, syncReceiverControlFields } from '../ui/receiverControlFields';
import { clearMainReceiverControls, syncMainReceiverControls } from '../ui/mainReceiverControls';
import { publishCommandReply } from '../core/wsSend';
import { ReceiverModeMemory } from '../ui/receiverModeMemory';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import type { ReceptionState } from '../core/multichannel';
import { receiverRuntime } from '../ui/receiverRuntime';

afterEach(() => {clearMainReceiverControls();document.body.replaceChildren();sessionStorage.clear();vi.useRealTimers();});
it('round trips every persisted mode field, and exposes controls by their mode in either editor', () => {
  const controls = {...copyModeControls(undefined),rf_snr:true,am_sync:true,pitch:1350,deemph_us:75,stereo:false,audio_rate:24000,
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
it('main C8 audio-rate action retains UID/revision when selection changes before its response', () => {
  document.body.innerHTML='<div><div id="sdr-ifbw-group"></div></div>';
  const send=vi.fn(), row={channel:8,mode:'wfm',audio_rate:48000,control:{service_id:'parity-service',receiver_uid:'uid-eight',revision:'11',action_token:'100'}};
  syncMainReceiverControls(row,send);
  const button=document.querySelector<HTMLButtonElement>('[data-sdr-audio-rate="24000"]')!;
  button.click();
  syncMainReceiverControls({...row,channel:1,control:{...row.control,receiver_uid:'uid-one'}},send);
  expect(send.mock.calls[0][0]).toMatchObject({channel:8,receiver_uid:'uid-eight',service_id:'parity-service',expected_revision:'11',audio_rate:24000});
  expect(send.mock.calls[0][0]).not.toHaveProperty('stereo');
  expect(document.querySelector('#sdr-mode-settings')).toBeNull();
  expect(document.querySelector('[data-mode-controls]')).toBeNull();
  button.click();
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

const receptionFailure = (allowed: boolean): ReceptionState => ({policy:'continuous',state:'stopped',interruptions:1,
  attempts:0,retry_limit:3,retry_in_ms:null,manual_recovery_allowed:allowed,last_error:null,
  last_interruption:{kind:allowed?'overload':'internal',scope:'processing',reason:'selected dfn model preparation failed: invalid model',
    at_ms:100,recovered_at_ms:null,before:null,after:null,missing_input_samples:null}});
it('processing diagnostics obey explicit recovery permission while retaining the actual failure', () => {
  const row={processing_state:'failed' as const,processing_fault:{error:'selected dfn model preparation failed: invalid model',
    generation:13,receiver_uid:'eight',service_id:'test'},reception:receptionFailure(false)};
  document.body.innerHTML=receiverRuntime(row);
  expect(document.querySelector('[data-processing-runtime]')!.textContent).toContain(row.processing_fault.error);
  expect(document.querySelector('[data-action="recover-processing"]')).toBeNull();
  document.body.innerHTML=receiverRuntime({...row,reception:receptionFailure(true)});
  expect(document.querySelector('[data-action="recover-processing"]')).not.toBeNull();
});
const audioRow = (rate = 48000) => ({ channel: 1, mode: 'am', audio_rate: rate,
  control: { service_id: 'audio-service', receiver_uid: 'first', revision: '1', action_token: '100' } });
const rateButton = (rate: number) => document.querySelector<HTMLButtonElement>(`[data-sdr-audio-rate="${rate}"]`)!;

it('offers 12/24/48 kHz and displays a custom confirmed rate without changing it', () => {
  document.body.innerHTML='<div><div id="sdr-ifbw-group"></div></div>';
  const send=vi.fn(); syncMainReceiverControls(audioRow(),send);
  expect([...document.querySelectorAll<HTMLElement>('[data-sdr-audio-rate]')].map(button => Number(button.dataset.sdrAudioRate))).toEqual([12000,24000,48000]);
  expect(rateButton(48000).getAttribute('aria-pressed')).toBe('true');
  expect(rateButton(48000).dataset.default).toBe('true');
  rateButton(48000).click(); expect(send).not.toHaveBeenCalled();
  syncMainReceiverControls(audioRow(44100),send);
  expect(document.querySelector('[data-audio-rate-unit]')!.textContent).toBe('44.1 kHz');
  expect(document.querySelector('[data-sdr-audio-rate].active')).toBeNull();
  expect(send).not.toHaveBeenCalled();
});
it('waits for confirmation and contains unrelated or rejected request replies', () => {
  document.body.innerHTML='<div><div id="sdr-ifbw-group"></div></div>';
  const send=vi.fn(); syncMainReceiverControls(audioRow(),send);
  rateButton(24000).click(); const command=send.mock.calls[0][0];
  expect(rateButton(12000).disabled).toBe(true);
  expect(rateButton(48000).getAttribute('aria-pressed')).toBe('true');
  publishCommandReply({cmd:'ERROR',response_to:'SET_SDR_CHANNEL',request_id:'unrelated'});
  expect(rateButton(12000).disabled).toBe(true);
  publishCommandReply({cmd:'ERROR',response_to:'SET_SDR_CHANNEL',request_id:command.request_id});
  expect(rateButton(12000).disabled).toBe(false);
  rateButton(12000).click();
  syncMainReceiverControls({...audioRow(12000),control:{...audioRow().control,revision:'2'}},send);
  expect(rateButton(12000).getAttribute('aria-pressed')).toBe('true');
  expect(rateButton(48000).disabled).toBe(false);
});
it('restores confirmed shortcuts after timeout and discards pending state on Preset', () => {
  vi.useFakeTimers();
  document.body.innerHTML='<div><div id="sdr-ifbw-group"></div></div>';
  const send=vi.fn(); syncMainReceiverControls(audioRow(),send);
  rateButton(24000).click(); vi.advanceTimersByTime(3000);
  expect(rateButton(48000).getAttribute('aria-pressed')).toBe('true');
  expect(rateButton(24000).disabled).toBe(false);
  rateButton(12000).click();
  const detached=rateButton(12000);
  document.dispatchEvent(new CustomEvent('websa:preset-applied'));
  expect(document.querySelector('#sdr-audio-rate-row')).toBeNull();
  syncMainReceiverControls({...audioRow(),control:{...audioRow().control,receiver_uid:'preset',revision:'3'}},send);
  expect(rateButton(48000).getAttribute('aria-pressed')).toBe('true');
  detached.click(); expect(send).toHaveBeenCalledTimes(2);
});
it('removes shortcuts for protocol modes and never revives an old actor after disconnect', () => {
  document.body.innerHTML='<div><div id="sdr-ifbw-group"></div></div>';
  const send=vi.fn(); syncMainReceiverControls(audioRow(),send);
  const detached=rateButton(12000);
  syncMainReceiverControls({...audioRow(),mode:'drm'},send);
  expect(document.querySelector('#sdr-audio-rate-row')).toBeNull();
  detached.click(); expect(send).not.toHaveBeenCalled();
  syncMainReceiverControls(audioRow(),send);
  publishCommandReply({cmd:'DISCONNECTED'});
  expect(document.querySelector('#sdr-audio-rate-row')).toBeNull();
});
it('keeps focused audio-rate buttons mounted across identical periodic confirmations', () => {
  document.body.innerHTML='<div><div id="sdr-ifbw-group"></div></div>';
  const send=vi.fn(); syncMainReceiverControls(audioRow(),send);
  const button=rateButton(24000); button.focus();
  for (let i=0;i<5;i++) syncMainReceiverControls(audioRow(),send);
  expect(rateButton(24000)).toBe(button);
  expect(document.activeElement).toBe(button);
  expect(send).not.toHaveBeenCalled();
});
