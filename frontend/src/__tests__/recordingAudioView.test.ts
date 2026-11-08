import { afterEach, describe, expect, it, vi } from 'vitest';
import { mountRecordingAudioView, recordedAudioWav } from '../ui/recordingAudioView';
import type { RecordingTask } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountWorkbench } from '../ui/workbenchApp';

const task = (id: string): RecordingTask => ({ task_id: id, receiver_uid: 'old-uid', service_id: 'old-service', state: 'completed', fault: null,
  tracks: [{ kind: 'audio', stored_bytes: '0', raw_bytes: '0', segments: [{ segment_id: 'audio-000000', samples: '18446744073709551615', actual_rate_hz: 48000, channels: 2 }] }] } as RecordingTask);
afterEach(() => { document.body.innerHTML = ''; vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe('stored PCM playback ownership', () => {
  it('shows the finished historical task entry only on a service advertising audio reads', () => {
    const adapter = new FixtureReceiverAdapter(), session = new WorkbenchSession(adapter);
    session.snapshot = { ...session.snapshot, transport: 'native', recordings: [task('history')],
      capabilities: { ...session.snapshot.capabilities, audio_archive_export: true, audio_archive_read: false } };
    const view = mountWorkbench(session, document.body);
    try {
      document.querySelector<HTMLButtonElement>('.mc-workspace-actions [data-action=recording-files]')!.click();
      expect(document.querySelector('[data-action=audio-archive]')).toBeNull();
      session.snapshot.capabilities.audio_archive_read = true; session.notify();
      const entry = document.querySelector<HTMLElement>('[data-action=audio-archive]');
      expect(entry?.dataset.archiveTask).toBe('history');
      expect(document.querySelector('.mc-audio-download')).not.toBeNull();
    } finally { view.dispose(); session.dispose(); adapter.dispose(); }
  });
  it('preserves exact offsets and aborts obsolete requests on edits, task changes, hiding and disposal', async () => {
    const pending: Array<(response: Response) => void> = [];
    const fetch = vi.fn((_url: string, _options: RequestInit) => new Promise<Response>(resolve => { pending.push(resolve); })); vi.stubGlobal('fetch', fetch);
    const view = mountRecordingAudioView(); view.open(task('first'));
    const submit = (): void => document.querySelector<HTMLFormElement>('[data-recorded-audio] form')!.requestSubmit();
    const start = document.querySelector<HTMLInputElement>('[name=start]')!, count = document.querySelector<HTMLInputElement>('[name=count]')!;
    start.value = '9007199254740993'; count.value = '1000'; submit();
    expect(fetch.mock.calls[0][0]).toContain('start=9007199254740993');
    const firstSignal = fetch.mock.calls[0][1].signal!;
    count.value = '2000'; count.dispatchEvent(new Event('input', { bubbles: true })); expect(firstSignal.aborted).toBe(true);
    expect(document.querySelector('[name=start]')).toBe(start);
    submit(); view.open(task('second')); expect(fetch.mock.calls[1][1].signal!.aborted).toBe(true);
    submit(); document.querySelector<HTMLElement>('[data-recorded-audio]')!.hidden = true;
    await Promise.resolve(); expect(fetch.mock.calls[2][1].signal!.aborted).toBe(true);
    document.querySelector<HTMLElement>('[data-recorded-audio]')!.hidden = false;
    submit(); view.dispose(); expect(fetch.mock.calls[3][1].signal!.aborted).toBe(true);
    for (const resolve of pending) resolve({ ok: true, headers: new Headers({ 'Content-Length': '2048' }), body: new ReadableStream({ start(c) { c.enqueue(new Uint8Array(2048)); c.close(); } }) } as Response);
    await new Promise(resolve => setTimeout(resolve, 0)); expect(document.querySelector('[data-recorded-audio]')).toBeNull();
  });
  it('rejects more than 60 seconds and a response exceeding the allocation bound', async () => {
    const fetch = vi.fn(async () => ({ ok: true, headers: new Headers({ 'Content-Length': String(16 * 1024 * 1024 + 1) }) })); vi.stubGlobal('fetch', fetch);
    const item = task('mono'); item.tracks[0].segments[0].actual_rate_hz = 8000;
    const view = mountRecordingAudioView(); view.open(item);
    const count = document.querySelector<HTMLInputElement>('[name=count]')!; count.value = '480001';
    const form = document.querySelector<HTMLFormElement>('form')!; form.requestSubmit(); await Promise.resolve();
    expect(fetch).not.toHaveBeenCalled(); expect(document.querySelector('[data-audio-status]')!.textContent).toContain('60');
    count.value = '1000'; form.requestSubmit(); await new Promise(resolve => setTimeout(resolve, 0));
    expect(document.querySelector('[data-audio-status]')!.textContent).toContain('length');
    expect(document.querySelector<HTMLButtonElement>('[data-audio-load]')!.disabled).toBe(false); view.dispose();
  });
  it('rejects truncated, oversized, path-bearing and unchecked playback envelopes', () => {
    expect(() => recordedAudioWav(new ArrayBuffer(0))).toThrow();
    expect(() => recordedAudioWav(new ArrayBuffer(16 * 1024 * 1024 + 1))).toThrow();
    const bytes = new Uint8Array(2048); bytes.set(new TextEncoder().encode('../audio.wav'));
    expect(() => recordedAudioWav(bytes.buffer)).toThrow();
    bytes.fill(0); bytes.set(new TextEncoder().encode('audio.wav')); bytes.set(new TextEncoder().encode('00000000004'), 124);
    expect(() => recordedAudioWav(bytes.buffer)).toThrow();
  });
});
