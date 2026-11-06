import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { getLang, setLang } from '../core/i18n';
import type { ServiceCommand, TrackKind } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountWorkbench } from '../ui/workbenchApp';
import { audioDownload, resetAudioFormat, savedAudioFormat } from '../ui/recordingFormats';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession, ui: ReturnType<typeof mountWorkbench>, language: ReturnType<typeof getLang>;
const overview = (): HTMLElement => document.querySelector('[data-view-id=overview-follow]')!;
const library = (): HTMLElement => document.querySelector('[data-view-id=recording-library]')!;
const click = (root: HTMLElement, selector: string): void => root.querySelector<HTMLButtonElement>(selector)!.click();
const settle = async (): Promise<void> => { await vi.advanceTimersByTimeAsync(1000); };
const send = async (command: ServiceCommand): Promise<void> => { const promise = session.send(command); await settle(); expect((await promise).fault).toBeNull(); };
const search = (value: string): void => { const input = library().querySelector<HTMLInputElement>('[data-file-search]')!; input.value = value; input.dispatchEvent(new Event('input', { bubbles: true })); };
const completed = async (uid: string, kind: TrackKind, key: string): Promise<string> => {
  await send({ type: 'record_start', request_id: key, receiver_uid: uid, kinds: [kind] });
  const id = session.snapshot.recordings.at(-1)!.task_id;
  await send({ type: 'record_stop', request_id: `stop-${key}`, task_id: id });
  return id;
};
const checkbox = (selector: string): HTMLInputElement => library().querySelector<HTMLInputElement>(selector)!;

beforeEach(() => {
  vi.useFakeTimers(); localStorage.clear(); language = getLang(); setLang('en');
  adapter = new FixtureReceiverAdapter(8, 10); session = new WorkbenchSession(adapter);
  ui = mountWorkbench(session, document.body);
});
afterEach(() => { ui.dispose(); session.dispose(); adapter.dispose(); document.body.replaceChildren(); setLang(language); vi.useRealTimers(); vi.restoreAllMocks(); });

it('remembers the audio format, sends it on start, and keeps the running recording format fixed', async () => {
  click(overview(), '[data-action=tab-recording]');
  const menu = overview().querySelector<HTMLDetailsElement>('[data-editor] [data-format-menu]')!;
  expect(menu.querySelector('summary')!.title).toContain('editing and analysis');
  expect(menu.querySelector('summary')!.title).toContain('long recordings and sharing');
  click(menu, '[data-format=opus]');
  expect(localStorage.getItem('websa-audio-recording-format')).toBe('opus');
  const dispatch = vi.spyOn(adapter, 'dispatch');
  click(overview(), '[data-editor] [data-action=rec-audio]'); await settle();
  expect(dispatch).toHaveBeenCalledWith(expect.objectContaining({ type: 'record_start', audio_format: 'opus' }));
  expect(session.snapshot.recordings[0].audio_format).toBe('opus');
  expect(overview().querySelector('[data-editor] .mc-record-format')!.textContent).toBe('Opus');
  expect(overview().querySelector('[data-editor] .mc-record-controls [data-format-menu]')).toBeNull();
  click(overview(), '[data-editor] [data-action=rec-audio]'); await settle();
  expect(overview().querySelector('[data-editor] [data-format-menu] summary')!.textContent).toContain('Opus');
  ui.dispose(); ui = mountWorkbench(session, document.body);
  click(overview(), '[data-action=tab-recording]');
  expect(overview().querySelector('[data-editor] [data-format-menu] summary')!.textContent).toContain('Opus');
});

it('defaults to Opus and restores the compact footer setting, including an explicit WAV preference', async () => {
  const select = () => overview().querySelector<HTMLSelectElement>('[data-default-audio-format]')!;
  expect(select().value).toBe('opus');
  expect(select().closest('.mc-master')).not.toBeNull();
  select().value = 'wav'; select().dispatchEvent(new Event('change', { bubbles: true }));
  expect(savedAudioFormat()).toBe('wav');
  ui.dispose(); ui = mountWorkbench(session, document.body);
  expect(select().value).toBe('wav');
  click(overview(), '[data-action=tab-recording]');
  click(overview(), '[data-editor] [data-action=rec-audio]'); await settle();
  expect(session.snapshot.recordings[0].audio_format).toBe('wav');
  resetAudioFormat(); expect(select().value).toBe('opus'); expect(savedAudioFormat()).toBe('opus');
  expect(session.snapshot.recordings[0].audio_format).toBe('wav');
  expect(overview().querySelector('[data-editor] .mc-record-format')!.textContent).toBe('WAV');
  const html = audioDownload(session.snapshot.recordings[0], 'opus', session.snapshot);
  expect(html).toContain('Download WAV'); expect(html).not.toContain('data-format-menu');
});

it('uses Opus for missing or invalid saved format preferences', () => {
  for (const value of ['', 'invalid']) {
    localStorage.setItem('websa-audio-recording-format', value); expect(savedAudioFormat()).toBe('opus');
  }
});

it('searches unit-suffixed frequencies and original channel names after renaming and retiring a channel', async () => {
  const uid = 'fixture-c1', config = structuredClone(session.snapshot.receivers[0].confirmed);
  config.name = 'Harbour radio'; config.baseband.frequency_hz = 7_100_000.3;
  const configured = session.configure(uid, config); await settle(); await configured;
  const id = await completed(uid, 'audio', 'named');
  await send({ type: 'record_rename', request_id: 'rename', task_id: id, name: 'Evening session' });
  await send({ type: 'retire', request_id: 'retire', receiver_uid: uid });
  click(overview(), '.mc-workspace-actions [data-action=recording-files]');
  for (const query of ['7M', '7.1 MHz', '7100k', '7100000 Hz', 'Harbour', 'Harbour 7.1m', 'Evening']) {
    search(query); expect(library().querySelectorAll('.mc-file'), query).toHaveLength(1);
  }
  for (const query of ['14M', '7k', '7.2 MHz', 'other channel']) {
    search(query); expect(library().querySelectorAll('.mc-file'), query).toHaveLength(0);
  }
});

it('shows history only for the displayed frequency, including retired identities and rounded Hz matches', async () => {
  const id = await completed('fixture-c1', 'audio', 'history');
  await completed('fixture-c3', 'iq', 'unrelated');
  const task = session.snapshot.recordings.find(t => t.task_id === id)!;
  task.receiver_uid = 'retired-uid'; task.tracks[0].segments[0].frequency_hz = 7_100_000.4;
  session.notify();
  expect(overview().querySelector('[data-section=recording-history]')).toBeNull();
  click(overview(), '[data-action=tab-recording]');
  expect(overview().querySelectorAll('.mc-task')).toHaveLength(1);
  expect(overview().querySelector('.mc-task')!.getAttribute('data-task-id')).toBe(id);
  session.snapshot.receivers[0].confirmed.baseband.frequency_hz = 7_100_000.6; session.notify();
  expect(overview().querySelectorAll('.mc-task')).toHaveLength(0);
  session.snapshot.receivers[0].confirmed.baseband.frequency_hz = 7_150_000; session.notify();
  expect(overview().querySelectorAll('.mc-task')).toHaveLength(1);
  expect(overview().querySelector('.mc-task')!.getAttribute('data-task-id')).not.toBe(id);
});

it('manages recordings across channels and preserves rename drafts during snapshot updates', async () => {
  await send({ type: 'record_start', request_id: 'audio', receiver_uid: 'fixture-c1', kinds: ['audio'], audio_format: 'flac' });
  await send({ type: 'record_stop', request_id: 'stop', task_id: session.snapshot.recordings[0].task_id });
  await send({ type: 'record_start', request_id: 'iq', receiver_uid: 'fixture-c3', kinds: ['iq'] });
  click(overview(), '.mc-workspace-actions [data-action=recording-files]');
  expect(library().querySelectorAll('.mc-file')).toHaveLength(2);
  const active = library().querySelector('[data-task-state=recording]') as HTMLElement;
  expect(active.querySelector<HTMLButtonElement>('[data-file-action=delete]')!.disabled).toBe(true);
  expect(active.querySelector('[data-file-action=rename]')).toBeNull();
  click(library(), '[data-kind=audio]');
  expect(library().querySelectorAll('.mc-file')).toHaveLength(1);
  expect(library().querySelector('.mc-file-info p')!.textContent).toContain('FLAC');
  click(library(), '[data-file-action=rename]');
  const input = library().querySelector<HTMLInputElement>('[name=recording-name]')!;
  input.value = 'Evening broadcast'; input.dispatchEvent(new Event('input', { bubbles: true })); input.blur();
  session.notify(); expect(library().querySelector('[name=recording-name]')).toBe(input); expect(input.value).toBe('Evening broadcast');
  library().querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); await settle();
  expect(session.snapshot.recordings[0].name).toBe('Evening broadcast');
  search('evening'); expect(library().querySelectorAll('.mc-file')).toHaveLength(1);
  search('missing'); expect(library().querySelector('.mc-files-empty')!.textContent).toContain('No matching');
  click(library(), '[data-file-action=clear]'); expect(library().querySelectorAll('.mc-file')).toHaveLength(2);
  click(library(), '[data-file-action=stop]'); await settle();
  expect(library().querySelectorAll('[data-file-action=delete]')).toHaveLength(2);
});

it('requires an explicit file deletion, permits cancellation, and retains files after a rejected delete', async () => {
  await send({ type: 'record_start', request_id: 'audio', receiver_uid: 'fixture-c1', kinds: ['audio'] });
  const id = session.snapshot.recordings[0].task_id;
  await send({ type: 'record_stop', request_id: 'stop', task_id: id });
  click(overview(), '.mc-workspace-actions [data-action=recording-files]');
  const dispatch = vi.spyOn(adapter, 'dispatch');
  click(library(), '[data-file-action=delete]');
  expect(dispatch).not.toHaveBeenCalled(); expect(library().querySelector('.mc-file-confirm')!.textContent).toContain('cannot be undone');
  click(library(), '[data-file-action=cancel-delete]'); expect(library().querySelector('.mc-file-confirm')).toBeNull();
  // Reject a management command at the transport boundary, then let a retry succeed.
  dispatch.mockResolvedValueOnce({ request_id: 'delete', snapshot: adapter.snapshot(), fault: { scope: 'recording', code: 'BUSY', message: 'Download in progress', retryable: true } });
  click(library(), '[data-file-action=delete]'); click(library(), '[data-file-action=confirm-delete]'); await settle();
  expect(library().querySelector('.mc-files-body [role=alert]')!.textContent).toContain('Download in progress');
  expect(session.snapshot.recordings).toHaveLength(1);
  click(library(), '[data-file-action=confirm-delete]'); await settle();
  expect(session.snapshot.recordings).toHaveLength(0);
  expect(library().querySelector('.mc-files-empty')!.textContent).toContain('Your recordings appear here');
});

it('selects finished files, shows a mixed selection, and confirms a whole batch without deleting an active recording', async () => {
  const audio = await completed('fixture-c1', 'audio', 'audio');
  const iq = await completed('fixture-c3', 'iq', 'iq');
  await send({ type: 'record_start', request_id: 'active', receiver_uid: 'fixture-c2', kinds: ['results'] });
  click(overview(), '.mc-workspace-actions [data-action=recording-files]');
  const dispatch = vi.spyOn(adapter, 'dispatch');
  checkbox(`[data-file-select="${audio}"]`).click();
  expect(checkbox('[data-file-select-all]').indeterminate).toBe(true);
  session.notify();
  expect(checkbox(`[data-file-select="${audio}"]`).checked).toBe(true);
  checkbox('[data-file-select-all]').click();
  expect(checkbox('[data-file-select-all]').checked).toBe(true);
  expect(checkbox(`[data-file-select="${iq}"]`).checked).toBe(true);
  expect(checkbox('[data-task-state=recording] [data-file-select]').disabled).toBe(true);
  expect(library().querySelector('[data-selected-info]')!.textContent).toContain('2 selected');
  click(library(), '[data-file-action=delete-selected]');
  expect(library().querySelector('.mc-files-batch-confirm')!.textContent).toContain('2 recordings');
  expect(dispatch).not.toHaveBeenCalled();
  click(library(), '[data-file-action=cancel-batch-delete]');
  expect(session.snapshot.recordings).toHaveLength(3);
  click(library(), '[data-file-action=delete-selected]'); click(library(), '[data-file-action=confirm-batch-delete]'); await settle();
  expect(dispatch.mock.calls.filter(([c]) => c.type === 'record_delete').map(([c]) => 'task_id' in c && c.task_id)).toEqual([audio, iq]);
  expect(session.snapshot.recordings).toHaveLength(1); expect(session.snapshot.recordings[0].state).toBe('recording');
  expect(library().querySelector('[role=status]')!.textContent).toContain('Deleted 2 recordings');
});

it('limits select all to the filtered list and clears the selection when filters change', async () => {
  const audio = await completed('fixture-c1', 'audio', 'audio');
  const iq = await completed('fixture-c3', 'iq', 'iq');
  click(overview(), '.mc-workspace-actions [data-action=recording-files]');
  click(library(), '[data-kind=audio]'); checkbox('[data-file-select-all]').click();
  expect(library().querySelector('[data-selected-info]')!.textContent).toContain('1 selected');
  click(library(), '[data-kind=all]'); expect(checkbox(`[data-file-select="${audio}"]`).checked).toBe(false);
  expect(checkbox(`[data-file-select="${iq}"]`).checked).toBe(false);
  checkbox('[data-file-select-all]').click(); search('does not exist');
  expect(library().querySelector<HTMLButtonElement>('[data-file-action=delete-selected]')!.disabled).toBe(true);
  click(library(), '[data-file-action=clear]');
  click(library(), '[data-kind=iq]'); checkbox('[data-file-select-all]').click();
  click(library(), '[data-file-action=delete-selected]'); click(library(), '[data-file-action=confirm-batch-delete]'); await settle();
  expect(session.snapshot.recordings.map(t => t.task_id)).toEqual([audio]);
});

it('keeps failed batch deletions selected and retries only those files', async () => {
  const audio = await completed('fixture-c1', 'audio', 'audio');
  const iq = await completed('fixture-c3', 'iq', 'iq');
  click(overview(), '.mc-workspace-actions [data-action=recording-files]');
  const dispatch = vi.spyOn(adapter, 'dispatch');
  dispatch.mockResolvedValueOnce({ request_id: 'busy', snapshot: adapter.snapshot(), fault: { scope: 'recording', code: 'BUSY', message: 'Download in progress', retryable: true } });
  checkbox('[data-file-select-all]').click();
  click(library(), '[data-file-action=delete-selected]'); click(library(), '[data-file-action=confirm-batch-delete]'); await settle();
  expect(session.snapshot.recordings.map(t => t.task_id)).toEqual([audio]);
  expect(checkbox(`[data-file-select="${audio}"]`).checked).toBe(true);
  expect(library().querySelector('.mc-files-body [role=alert]')!.textContent).toContain('1 deleted; 1 failed');
  expect(library().querySelector('.mc-files-body [role=alert]')!.textContent).toContain('Download in progress');
  click(library(), '[data-file-action=delete-selected]'); click(library(), '[data-file-action=confirm-batch-delete]'); await settle();
  expect(session.snapshot.recordings).toHaveLength(0);
  expect(dispatch.mock.calls.filter(([c]) => c.type === 'record_delete').map(([c]) => 'task_id' in c && c.task_id)).toEqual([audio, iq, audio]);
});

it('shows friendly file and total sizes, including multi-track and large recordings', async () => {
  await completed('fixture-c1', 'audio', 'audio');
  click(overview(), '.mc-workspace-actions [data-action=recording-files]');
  const task = session.snapshot.recordings[0];
  for (const [stored, label] of [['0', '0 B'], ['999', '999 B'], ['1500', '1.5 KB'], ['1250000', '1.3 MB'], ['2500000000', '2.5 GB'], ['999999', '1 MB'], ['1500000000000', '1.5 TB'], ['18446744073709551615', '18.4 EB']]) {
    task.tracks[0].stored_bytes = stored; session.notify();
    expect(library().querySelector('[data-file-size]')!.textContent).toBe(`Size ${label}`);
    expect(library().querySelector('[data-files-total-size]')!.textContent).toBe(label);
  }
  task.tracks[0].stored_bytes = '1000'; task.tracks.push({ ...task.tracks[0], track_id: 'another', stored_bytes: '2500' }); session.notify();
  expect(library().querySelector('[data-file-size]')!.textContent).toBe('Size 3.5 KB');
  checkbox('[data-file-select-all]').click();
  expect(library().querySelector('[data-selected-info]')!.textContent).toContain('3.5 KB');
});

it('keeps delete controls visible with an explanation when the connected service is too old', async () => {
  await completed('fixture-c1', 'audio', 'audio');
  session.snapshot.transport = 'native'; session.snapshot.capabilities.commands = ['record_start', 'record_stop'];
  click(overview(), '.mc-workspace-actions [data-action=recording-files]');
  const remove = library().querySelector<HTMLButtonElement>('[data-file-action=delete]')!;
  expect(remove.disabled).toBe(true); expect(remove.title).toContain('Update and restart');
  expect(library().querySelector('[data-key=delete-unavailable]')!.textContent).toContain('Update and restart');
  expect(checkbox('[data-file-select-all]').disabled).toBe(true);
});

it.each(['audio', 'iq', 'results'] as const)('deletes %s from channel recording history through the file confirmation', async kind => {
  const uid = kind === 'audio' ? 'fixture-c1' : kind === 'iq' ? 'fixture-c3' : 'fixture-c2';
  const id = await completed(uid, kind, kind); session.select(uid);
  click(overview(), '[data-action=tab-recording]');
  const dispatch = vi.spyOn(adapter, 'dispatch');
  click(overview(), `[data-task-id="${id}"] [data-action=delete-recording]`);
  expect(dispatch).not.toHaveBeenCalled();
  expect(library().querySelector(`[data-task-id="${id}"] .mc-file-confirm`)).not.toBeNull();
  click(library(), '[data-file-action=confirm-delete]'); await settle();
  expect(overview().querySelector(`[data-task-id="${id}"]`)).toBeNull();
});

it.each(['iq', 'results'] as const)('offers deletion of %s recordings in the decode window', async kind => {
  const uid = kind === 'iq' ? 'fixture-c3' : 'fixture-c2';
  const id = await completed(uid, kind, kind); session.select(uid);
  click(overview(), '[data-action=follow-results]');
  const results = document.querySelector<HTMLElement>('[data-view-id=results-follow]')!;
  expect(results.querySelector('[data-action=recording-files]')).not.toBeNull();
  click(results, `[data-task-id="${id}"] [data-action=delete-recording]`);
  click(library(), '[data-file-action=confirm-delete]'); await settle();
  expect(results.querySelector(`[data-task-id="${id}"]`)).toBeNull();
});
