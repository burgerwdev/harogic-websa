import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { getLang, setLang } from '../core/i18n';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountRecordingAudioView } from '../ui/recordingAudioView';
import { mountRecordingView } from '../ui/recordingView';
import type { RecordingTask, ServiceCommand } from '../core/multichannel';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession, view: { dispose(): void } | undefined;
let language: ReturnType<typeof getLang>;
const button = (selector: string): HTMLButtonElement => document.querySelector(selector)!;
const settle = async (): Promise<void> => { await vi.advanceTimersByTimeAsync(100); };
const send = async (command: ServiceCommand): Promise<void> => {
  const reply = session.send(command); await settle(); expect((await reply).fault).toBeNull();
};
const open = async (kind: 'audio' | 'iq'): Promise<RecordingTask> => {
  await send({ type: 'record_start', request_id: 'start', receiver_uid: kind === 'audio' ? 'fixture-c1' : 'fixture-c3', kinds: [kind] });
  const task = session.snapshot.recordings[0];
  await send({ type: 'record_stop', request_id: 'stop', task_id: task.task_id });
  const finished = session.snapshot.recordings[0];
  finished.tracks[0].segments = [{ segment_id: 'segment-0', samples: '48000', actual_rate_hz: 48000, channels: 1,
    first_input_index: '0', end_reason: 'stopped', stream: { source_id: 'primary', epoch: '1', channel_id: 'channel-0' } }];
  const player = kind === 'audio' ? mountRecordingAudioView(session) : mountRecordingView({ session, demodulateModes: () => ['am'] });
  view = player; player.open(finished); return finished;
};

beforeEach(() => {
  vi.useFakeTimers(); localStorage.clear(); language = getLang(); setLang('en');
  adapter = new FixtureReceiverAdapter(8, 10); session = new WorkbenchSession(adapter);
});
afterEach(() => {
  view?.dispose(); view = undefined; session.dispose(); adapter.dispose(); document.body.replaceChildren();
  setLang(language); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks();
});

it.each(['audio', 'iq'] as const)('confirms and deletes %s recordings from their playback window', async kind => {
  const task = await open(kind), dispatch = vi.spyOn(adapter, 'dispatch');
  button('[data-delete-recording]').click();
  expect(dispatch).not.toHaveBeenCalled();
  expect(button('[data-delete-confirm]').hidden).toBe(false);
  expect(document.activeElement).toBe(button('[data-delete-keep]'));
  button('[data-delete-keep]').click();
  expect(button('[data-delete-confirm]').hidden).toBe(true);
  button('[data-delete-recording]').click();
  button('[data-delete-confirm-button]').click(); button('[data-delete-confirm-button]').click();
  expect(button('[data-delete-confirm-button]').disabled).toBe(true);
  await settle();
  expect(dispatch).toHaveBeenCalledTimes(1);
  expect(dispatch).toHaveBeenCalledWith(expect.objectContaining({ type: 'record_delete', task_id: task.task_id }));
  expect(session.snapshot.recordings).toHaveLength(0);
  expect(document.querySelector('[data-recording-delete]')).toBeNull();
  expect(document.querySelector('[data-view-id=recorded-audio],[data-view-id=iq-recording]')).toBeNull();
});

it('keeps a rejected recording open and allows retrying the deletion', async () => {
  await open('audio');
  const dispatch = vi.spyOn(adapter, 'dispatch');
  dispatch.mockResolvedValueOnce({ request_id: 'busy', snapshot: adapter.snapshot(), fault: { scope: 'recording', code: 'BUSY', message: 'Download in progress', retryable: true } });
  button('[data-delete-recording]').click(); button('[data-delete-confirm-button]').click(); await settle();
  expect(button('[data-delete-error]').textContent).toBe('Download in progress');
  expect(button('[data-delete-error]').hidden).toBe(false);
  expect(session.snapshot.recordings).toHaveLength(1);
  button('[data-delete-confirm-button]').click(); await settle();
  expect(session.snapshot.recordings).toHaveLength(0);
});

it('disables deletion for active recordings and unsupported services, and updates translated labels', async () => {
  await open('iq');
  session.snapshot.recordings[0].state = 'finalizing'; session.notify();
  expect(button('[data-delete-recording]').disabled).toBe(true);
  expect(button('[data-delete-recording]').title).toContain('Stop recording');
  session.snapshot.recordings[0].state = 'failed'; session.notify();
  expect(button('[data-delete-recording]').disabled).toBe(false);
  session.snapshot.transport = 'native'; session.snapshot.capabilities.commands = []; session.notify();
  expect(button('[data-delete-recording]').disabled).toBe(true);
  expect(button('[data-delete-recording]').title).toContain('Update and restart');
  setLang('zh'); expect(button('[data-delete-recording]').textContent).toContain('删除录制');
});

it.each(['audio', 'iq'] as const)('aborts pending %s reads when the recording is deleted elsewhere', async kind => {
  await open(kind);
  const fetch = vi.fn((_url: string, _options: RequestInit) => new Promise<Response>(() => {})); vi.stubGlobal('fetch', fetch);
  document.querySelector<HTMLFormElement>('form')!.requestSubmit();
  expect(fetch).toHaveBeenCalledTimes(1);
  session.snapshot.recordings = []; session.notify();
  expect(fetch.mock.calls[0][1].signal!.aborted).toBe(true);
  expect(document.querySelector('[data-recording-delete]')).toBeNull();
});

it('stops IQ file demodulation and playback when the source service changes', async () => {
  await open('iq');
  const fetch = vi.fn((_url: string, _options: RequestInit) => new Promise<Response>(() => {})); vi.stubGlobal('fetch', fetch);
  button('[data-demod-run]').click(); expect(fetch).toHaveBeenCalledTimes(1);
  const audio = document.querySelector<HTMLAudioElement>('[data-demod-audio]')!;
  const pause = vi.spyOn(audio, 'pause').mockImplementation(() => {});
  vi.spyOn(audio, 'load').mockImplementation(() => {}); audio.src = 'blob:old-playback';
  session.snapshot.service_id = 'replacement-service'; session.notify();
  expect(fetch.mock.calls[0][1].signal!.aborted).toBe(true); expect(pause).toHaveBeenCalledOnce();
  expect(audio.hasAttribute('src')).toBe(false);
  expect(document.querySelector('[data-recording-delete]')).toBeNull();
});
