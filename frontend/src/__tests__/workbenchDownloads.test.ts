import { expect, it, vi } from 'vitest';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountWorkbench } from '../ui/workbenchApp';

it('cancels a held workbench recording download when its displayed service changes', () => {
  vi.useFakeTimers(); localStorage.clear();
  const adapter = new FixtureReceiverAdapter(), session = new WorkbenchSession(adapter);
  session.snapshot = { ...session.snapshot, transport: 'native', service_id: 'displayed-service',
    capabilities: { ...session.snapshot.capabilities, commands: ['record_start'], audio_archive_export: true },
    recordings: [{ task_id: 'saved-audio', receiver_uid: 'fixture-c1', service_id: 'displayed-service', state: 'completed', fault: null,
      tracks: [{ track_id: 'audio', kind: 'audio', format: 'f32_le', source: 'published_audio', compression: 'none',
        state: 'completed', raw_bytes: '800', stored_bytes: '800', segments: [{ segment_id: 'audio-a', samples: '100',
          first_input_index: '9007199254740993', actual_rate_hz: 48000, channels: 2, end_reason: 'stopped',
          frequency_hz: session.snapshot.receivers[0].confirmed.baseband.frequency_hz,
          stream: { source_id: 'primary', epoch: '1', channel_id: '1' } }] }] }] };
  const host = document.createElement('div'); document.body.append(host);
  const ui = mountWorkbench(session, host);
  const overview = document.querySelector<HTMLElement>('[data-view-id=overview-follow]')!;
  // Observe cancellation after app handlers, then suppress jsdom navigation itself.
  const appCancels = (link: HTMLAnchorElement): boolean => {
    let cancelled = false;
    const observe = (event: MouseEvent): void => { cancelled = event.defaultPrevented; event.preventDefault(); };
    document.addEventListener('click', observe, { once: true });
    try { link.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true })); }
    finally { document.removeEventListener('click', observe); }
    return cancelled;
  };
  try {
    overview.querySelector<HTMLButtonElement>('[data-action=tab-recording]')!.click();
    const download = overview.querySelector<HTMLAnchorElement>('a.mc-audio-download')!;
    expect(download.getAttribute('href')).toBe('/api/sdr/recordings/saved-audio/audio');
    expect(appCancels(download)).toBe(false);
    download.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    session.snapshot = { ...structuredClone(session.snapshot), service_id: 'replacement-service' }; session.notify();
    expect(download.isConnected).toBe(true);
    expect(appCancels(download)).toBe(true);
  } finally { ui.dispose(); session.dispose(); adapter.dispose(); document.body.replaceChildren(); vi.useRealTimers(); }
});
