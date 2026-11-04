import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { NativeReceiverAdapter, type ReceivedIq } from '../core/nativeReceiverAdapter';
import { clone, type ServiceSnapshot } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountWorkbench } from '../ui/workbenchApp';
import { getLang, setLang } from '../core/i18n';

class Socket {
  static instances: Socket[] = [];
  binaryType = ''; onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null; onclose: (() => void) | null = null;
  close = vi.fn(() => this.onclose?.());
  constructor(readonly url: URL) { Socket.instances.push(this); }
  emit(data: unknown): void { this.onmessage?.({ data: typeof data === 'object' && !(data instanceof ArrayBuffer) ? JSON.stringify(data) : data }); }
}
const response = (value: unknown) => ({ ok: true, json: async () => clone(value) });
const impact = { accepted: true, faults: [], capture_change: false, affected_receivers: [], affected_tracks: [], candidate_center_hz: 0, estimated_cf32_bytes_per_second: 0 };
let adapter: NativeReceiverAdapter, snapshot: ServiceSnapshot;
const fetchMock = vi.fn();
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear(); sessionStorage.clear(); Socket.instances = [];
  vi.stubGlobal('WebSocket', Socket); vi.stubGlobal('fetch', fetchMock); fetchMock.mockReset();
  const fixture = new FixtureReceiverAdapter(); snapshot = fixture.snapshot(); fixture.dispose();
  snapshot.transport = 'native'; snapshot.service_id = 'boot-A'; snapshot.revision = '10';
  snapshot.capabilities.commands = ['create', 'configure', 'retire', 'output'];
  fetchMock.mockResolvedValue(response(snapshot)); adapter = await NativeReceiverAdapter.connect(); fetchMock.mockClear();
});
afterEach(() => { adapter.dispose(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); document.body.replaceChildren(); });

it.each(['idle', 'failed'] as const)('shows unavailable %s capture without fabricated bandwidth and blocks listening preflight', async state => {
  snapshot.capture = { ...snapshot.capture, state, parameters_state:'unknown', usable_bandwidth_hz:null, input_rate_hz:null };
  fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh();
  const session=new WorkbenchSession(adapter), host=document.createElement('div'); document.body.append(host);
  const ui=mountWorkbench(session,host);
  expect(document.querySelector('[data-capture-status]')!.textContent).toContain('Capture parameters unknown');
  const receiver=snapshot.receivers[0];
  expect(adapter.preflight({ type:'configure',request_id:'unavailable',receiver_uid:receiver.receiver_uid,
    expected_revision:receiver.revision,config:receiver.confirmed }).faults[0].code).toBe('NO_SOURCE');
  snapshot.capture = { ...snapshot.capture, parameters_state:'last_known', usable_bandwidth_hz:3_125_000,input_rate_hz:3_906_250 };
  fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh();
  expect(document.querySelector('[data-capture-status]')!.textContent).toContain('Last capture');
  expect(document.querySelector('[data-capture-status]')!.textContent).toContain('3906250 S/s');
  ui.dispose();session.dispose();
});
const uid = 'fixture-c1';
const output = () => adapter.dispatch({ type: 'output', request_id: 'subscribe', receiver_uid: uid, enabled: true });
function start(socket: Socket): void {
  socket.emit({ type: 'stream_start', receiver_uid: uid, stream: { source_id: 'primary', channel_id: '7', epoch: '9' },
    clock: { input_index: '9007199254740997', output_index: '100', fraction_num: '0', fraction_den: '1',
      input_rate_hz: 62500000, actual_rate_hz: 62500000 / 10368, delay_seconds: 0.012, utc_reliable: false } });
}
function block(magic = 'IQBF'): ArrayBuffer {
  const data = new ArrayBuffer(72), view = new DataView(data);
  [...magic].forEach((c, i) => view.setUint8(i, c.charCodeAt(0)));
  view.setUint32(4, 2, true); view.setUint32(8, 5, true); view.setUint32(12, 2, true);
  view.setFloat64(16, 62500000 / 10368, true); view.setUint32(32, 7, true); view.setUint32(36, 9, true);
  view.setBigUint64(40, 9007199254740997n + 20736n, true);
  new Float32Array(data, 56).set([0.5, -0.25, 0.125, 0.75]); return data;
}
describe('native receiver control and shared IQ', () => {
  it.each(['en', 'zh'] as const)('explains an out-of-capture FM rejection in %s and retries after an explicit shared-window change', async lang => {
    const previousLang = getLang(); setLang(lang);
    snapshot.capture = { ...snapshot.capture, center_hz: 101_700_000, usable_bandwidth_hz: 3_125_000,
      input_rate_hz: 3_906_250, parameters_state: 'live' };
    const target = snapshot.receivers[1];
    target.state = 'stopped'; target.stream = null;
    target.confirmed = { ...target.confirmed, enabled: false, purpose: 'listen',
      baseband: { frequency_hz: 105_000_000, passband_hz: 180_000, transition_hz: 1000, minimum_rate_hz: 300_000 },
      processors: [{ id: 'primary', kind: 'demodulator', mode: 'wfm', bandwidth_hz: 180_000, minimum_rate_hz: 300_000 }] };
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh(); fetchMock.mockClear();
    const session = new WorkbenchSession(adapter); session.select(target.receiver_uid);
    const host = document.createElement('div'); document.body.append(host);
    const ui = mountWorkbench(session, host);
    try {
      const before = adapter.snapshot(), attempted = { ...clone(target.confirmed), enabled: true };
      fetchMock.mockResolvedValue(response({ ...impact, accepted: false, faults: [{ scope: 'request', code: 'REJECTED',
        message: 'full passband and transition exceed shared capture', retryable: false }] }));
      await session.configure(target.receiver_uid, attempted);
      const alert = document.querySelector('[data-key=edit-fault]')!;
      expect(alert.textContent).toContain('100.1375–103.2625 MHz');
      expect(alert.textContent).toContain('104.909–105.091 MHz');
      expect(alert.textContent).toContain(lang === 'zh' ? '主 SDR 面板调整共享中心或采集带宽' : 'main SDR panel');
      expect(adapter.snapshot()).toEqual(before);
      expect(fetchMock).toHaveBeenCalledTimes(1);
      expect(String(fetchMock.mock.calls[0][0])).toMatch(/\/preflight$/);

      snapshot.capture = { ...snapshot.capture, center_hz: 103_350_000, usable_bandwidth_hz: 6_250_000, input_rate_hz: 7_812_500 };
      snapshot.revision = '11'; fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh();
      expect(document.querySelector('[data-key=edit-fault]')!.textContent).toContain('100.225–106.475 MHz');
      fetchMock.mockClear();
      fetchMock.mockImplementation(async (url, options) => {
        if (String(url).endsWith('/preflight')) return response(impact);
        const request = JSON.parse(options.body);
        const next = clone(snapshot); next.revision = '12'; next.receivers[1].confirmed = clone(request.config); next.receivers[1].state = 'running';
        return response({ request_id: request.request_id, snapshot: next, fault: null });
      });
      document.querySelector<HTMLButtonElement>('[data-action=retry]')!.click();
      await vi.advanceTimersByTimeAsync(1);
      expect(session.edits.get(target.receiver_uid)?.fault).toBeNull();
      expect(session.snapshot.receivers[1].confirmed).toEqual(attempted);
      expect(session.snapshot.receivers[0]).toEqual(snapshot.receivers[0]);
      expect(fetchMock).toHaveBeenCalledTimes(2);
      expect(document.querySelector('[data-key=edit-fault]')).toBeNull();
    } finally { ui.dispose(); session.dispose(); setLang(previousLang); }
  });

  it('disables inactive monitor sliders and updates their readouts during a held gesture', async () => {
    snapshot.receivers[0].confirmed.processors[0].mode = 'am'; snapshot.capabilities.monitor_mixing = true;
    snapshot.capabilities.mode_descriptors = [{ id: 'am', label: 'AM', kind: 'analog', provider: 'native', available: true,
      unavailableReason: null, minimumRateHz: 6000, defaultBandwidthHz: 6000, sideband: 'symmetric', audioCompanion: null, outputs: ['audio'] }];
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh(); fetchMock.mockClear();
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    let outputChannels = 2;
    const ui = mountWorkbench(session, host, { results: () => ({ entries: [], evicted: 0 }), status: () => '', retry: () => {}, playback: () => ({ outputChannels }) }), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    const gain = overview.querySelector<HTMLInputElement>('[data-gain]')!;
    expect(gain.disabled).toBe(true); expect(overview.querySelector('.mc-monitor [data-action=listen]')!.getAttribute('aria-pressed')).toBe('false');
    overview.querySelector<HTMLButtonElement>('.mc-row [data-action=listen]')!.click();
    expect(gain.disabled).toBe(false);
    gain.dispatchEvent(new Event('pointerdown', { bubbles: true })); gain.value = '0.25'; gain.dispatchEvent(new Event('input', { bubbles: true }));
    expect(overview.querySelector('[data-gain-value]')!.textContent).toBe('25%');
    expect(session.route(uid).gain).toBe(0.25); expect(session.listening(uid)).toBe(true);
    document.dispatchEvent(new Event('pointerup')); await vi.advanceTimersByTimeAsync(1);
    expect(overview.querySelector('[data-gain]')).toBe(gain); expect(fetchMock).not.toHaveBeenCalled();
    outputChannels = 1; session.notify();
    expect(overview.querySelector<HTMLInputElement>('[data-pan]')!.disabled).toBe(true);
    expect(overview.querySelector('[data-pan-value]')!.textContent).toBe('Mono');
    session.snapshot.receivers[0].fault = { scope: 'receiver', code: 'PROCESSING_FAILED', message: 'Audio failed', retryable: true }; session.notify();
    expect(gain.disabled).toBe(true); expect(overview.querySelector('[data-monitor-state]')!.textContent).toBe('Audio unavailable');
    ui.dispose(); session.dispose();
  });
  it('uses one mode choice for processing while monitoring and IQ recording remain independent', async () => {
    snapshot.receivers[0].confirmed.processors[0].mode = 'am';
    snapshot.capabilities.monitor_mixing = true;
    snapshot.capabilities.commands!.push('record_start', 'record_stop'); snapshot.capabilities.recording_kinds = ['iq'];
    snapshot.capabilities.mode_descriptors = [{ id: 'am', label: 'AM', kind: 'analog', provider: 'native', available: true,
      unavailableReason: null, minimumRateHz: 6000, defaultBandwidthHz: 6000, sideband: 'symmetric', audioCompanion: null, outputs: ['audio'] }];
    snapshot.capabilities.mode_descriptors.push({ ...snapshot.capabilities.mode_descriptors[0], id: 'drm', label: 'DRM', kind: 'digital' });
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh();
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const configure = vi.spyOn(session, 'configure').mockResolvedValue();
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    expect(overview.querySelector('[name=purpose]')).toBeNull();
    expect(overview.querySelectorAll('[name=mode]')).toHaveLength(1);
    expect(overview.querySelector('[data-action=tab-processing]')).toBeNull();
    expect(overview.querySelector('[data-action=tab-results]')).toBeNull();
    expect(overview.querySelectorAll('.mc-row [data-action=listen]')).toHaveLength(1);
    expect(overview.querySelectorAll('.mc-monitor [data-action=listen]')).toHaveLength(1);
    const form = overview.querySelector<HTMLFormElement>('[data-config]')!;
    const mode = form.querySelector<HTMLSelectElement>('[name=mode]')!;
    mode.value = 'drm'; mode.dispatchEvent(new Event('change', { bubbles: true }));
    expect(form.querySelector<HTMLElement>('[data-monitor-enhancements]')!.hidden).toBe(true);
    expect(form.querySelector<HTMLElement>('[data-shared-audio]')!.hidden).toBe(false);
    mode.value = ''; mode.dispatchEvent(new Event('change', { bubbles: true }));
    form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    expect(overview.querySelector('[data-preview] button')).toBeNull();
    expect(configure).toHaveBeenCalledWith(uid, expect.objectContaining({ purpose: 'iq', processors: [], enabled: true }));
    // Removing processing never mutes a route or stops an IQ recording.
    expect(fetchMock.mock.calls.every(call => !call[1]?.method)).toBe(true);
    expect([...overview.querySelectorAll<HTMLButtonElement>('button')].every(button => !!button.title)).toBe(true);
    ui.dispose(); session.dispose();
  });
  it('records published results by actual mode capability and downloads finished historical tasks directly', async () => {
    snapshot.capabilities.commands!.push('record_start', 'record_stop');
    snapshot.capabilities.recording_kinds = ['iq', 'results']; snapshot.capabilities.results_archive_export = true;
    snapshot.capabilities.mode_descriptors = [{ id: 'am', label: 'AM', kind: 'analog', provider: 'native', available: true,
      unavailableReason: null, minimumRateHz: 6000, defaultBandwidthHz: 6000, sideband: 'symmetric', audioCompanion: null, outputs: ['audio', 'text'] }];
    snapshot.receivers[0].confirmed.purpose = 'listen'; snapshot.receivers[0].confirmed.processors[0].mode = 'am';
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh(); fetchMock.mockClear();
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const send = vi.spyOn(session, 'send').mockResolvedValue({ request_id: 'result', snapshot, fault: null });
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    overview.querySelector<HTMLButtonElement>('.mc-row [data-action=rec-results]')!.click();
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ type: 'record_start', receiver_uid: uid, kinds: ['results'] }));
    expect(Socket.instances).toHaveLength(0); expect(fetchMock).not.toHaveBeenCalled();
    session.snapshot.recordings.push({ task_id: 'retired-task', receiver_uid: 'old-uid', state: 'failed', fault: null,
      tracks: [{ track_id: 'events', kind: 'results', format: 'jsonl', source: 'decoder_results', compression: 'zstd_auto_lossless',
        state: 'failed', raw_bytes: '5000', stored_bytes: '1000', segments: [] }] }); session.notify();
    const link = overview.querySelector<HTMLAnchorElement>('a.mc-results-download')!;
    expect(link.getAttribute('href')).toBe('/api/sdr/recordings/retired-task/results'); expect(link.hasAttribute('download')).toBe(true);
    session.snapshot.capabilities.results_archive_export = false; session.notify(); expect(overview.querySelector('a.mc-results-download')).toBeNull();
    session.snapshot.capabilities.mode_descriptors![0].outputs = ['audio']; session.notify();
    expect(overview.querySelector('[data-action=rec-results]')).toBeNull(); ui.dispose(); session.dispose();
  });
  it('records actual audio companions independently and exposes native WAV downloads for retired UIDs', async () => {
    snapshot.capabilities.commands!.push('record_start', 'record_stop');
    snapshot.capabilities.recording_kinds = ['iq', 'results', 'audio']; snapshot.capabilities.audio_archive_export = true;
    snapshot.capabilities.mode_descriptors = [{ id: 'ft8', label: 'FT8', kind: 'digital', provider: 'native', available: true,
      unavailableReason: null, minimumRateHz: 6000, defaultBandwidthHz: 3000, sideband: 'upper', audioCompanion: 'usb', outputs: ['audio', 'table'] }];
    snapshot.receivers[0].confirmed.purpose = 'digital'; snapshot.receivers[0].confirmed.processors[0].mode = 'ft8';
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh(); fetchMock.mockClear();
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const send = vi.spyOn(session, 'send').mockResolvedValue({ request_id: 'audio', snapshot, fault: null });
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    overview.querySelector<HTMLButtonElement>('.mc-row [data-action=rec-audio]')!.click();
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ type: 'record_start', receiver_uid: uid, kinds: ['audio'] }));
    expect(Socket.instances).toHaveLength(0); expect(fetchMock).not.toHaveBeenCalled();
    session.snapshot.recordings.push({ task_id: 'audio-history', receiver_uid: 'retired', state: 'failed', fault: null,
      tracks: [{ track_id: 'pcm', kind: 'audio', format: 'f32_le', source: 'published_audio', compression: 'zstd_auto_lossless',
        state: 'failed', raw_bytes: '400', stored_bytes: '200', segments: [] }] }); session.notify();
    const link = overview.querySelector<HTMLAnchorElement>('a.mc-audio-download')!;
    expect(link.getAttribute('href')).toBe('/api/sdr/recordings/audio-history/audio'); expect(link.hasAttribute('download')).toBe(true);
    expect(overview.querySelector('a.mc-audio-flac')).toBeNull();
    session.snapshot.capabilities.audio_flac_export = true; session.notify();
    const flac = overview.querySelector<HTMLAnchorElement>('a.mc-audio-flac')!;
    expect(flac.getAttribute('href')).toBe('/api/sdr/recordings/audio-history/audio/flac');
    expect(flac.title).toContain('quantized 24-bit'); expect(flac.hasAttribute('download')).toBe(true);
    session.notify(); expect(overview.querySelector('a.mc-audio-download')).toBe(link); expect(overview.querySelector('a.mc-audio-flac')).toBe(flac);
    expect(send).toHaveBeenCalledTimes(1); expect(fetchMock).not.toHaveBeenCalled();
    session.snapshot.capabilities.audio_flac_export = false; session.notify(); expect(overview.querySelector('a.mc-audio-flac')).toBeNull();
    session.snapshot.capabilities.audio_archive_export = false; session.snapshot.capabilities.recording_kinds = ['iq']; session.notify();
    expect(overview.querySelector('a.mc-audio-download')).toBeNull(); expect(overview.querySelector('[data-action=rec-audio]')).toBeNull();
    ui.dispose(); session.dispose();
  });
  it('uses service-confirmed replies and ignores a late lower revision', async () => {
    const config = clone(snapshot.receivers[0].confirmed); config.name = 'Requested';
    fetchMock.mockImplementation(async (_url, options) => {
      const request = JSON.parse(options.body);
      if (String(_url).endsWith('/preflight')) return response(impact);
      const next = clone(snapshot); next.revision = '12'; next.receivers[0].confirmed.name = 'Confirmed';
      return response({ request_id: request.request_id, snapshot: next, fault: null });
    });
    const reply = await adapter.dispatch({ type: 'configure', request_id: 'edit', receiver_uid: uid, expected_revision: '1', config });
    expect(reply.request_id).toBe('edit'); expect(adapter.snapshot().receivers[0].confirmed.name).toBe('Confirmed');
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh(); expect(adapter.snapshot().revision).toBe('12');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body).service_id).toBe('boot-A');
  });
  it('preserves confirmed state on admission rejection and rejects mismatched response IDs', async () => {
    fetchMock.mockResolvedValueOnce(response({ ...impact, accepted: false, faults: [{ code: 'RF_RANGE' }] }));
    const command = { type: 'retire' as const, request_id: 'retire', receiver_uid: uid };
    expect((await adapter.dispatch(command)).fault?.code).toBe('RF_RANGE'); expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(adapter.snapshot()).toEqual(snapshot);
    fetchMock.mockResolvedValueOnce(response(impact)).mockResolvedValueOnce(response({ request_id: 'other', snapshot: { ...snapshot, revision: '99' }, fault: null }));
    await expect(adapter.dispatch(command)).rejects.toThrow('request_id mismatch'); expect(adapter.snapshot().revision).toBe('10');
  });
  it('clears an old deletion rejection when polling confirms the target was retired', async () => {
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const ui = mountWorkbench(session, host);
    try {
      fetchMock.mockResolvedValueOnce(response({ ...impact, accepted: false,
        faults: [{ scope: 'request', code: 'REJECTED', message: 'receiver not found', retryable: false }] }));
      await session.send({ type: 'retire', request_id: 'gone', receiver_uid: uid });
      expect(document.querySelector('[data-key=session-fault]')!.textContent).toContain('receiver not found');
      const next = clone(snapshot); next.revision = '11'; next.receivers = next.receivers.filter(r => r.receiver_uid !== uid);
      fetchMock.mockResolvedValue(response(next)); await adapter.refresh();
      expect(session.fault).toBeNull(); expect(document.querySelector('[data-key=session-fault]')).toBeNull();
    } finally { ui.dispose(); session.dispose(); }
  });
  it('cannot restore a retired service with a delayed command reply', async () => {
    let finish!: (value: unknown) => void;
    fetchMock.mockResolvedValueOnce(response(impact)).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    const pending = adapter.dispatch({ type: 'retire', request_id: 'old', receiver_uid: uid });
    await vi.waitFor(() => expect(finish).toBeTypeOf('function'));
    const oldRequest = JSON.parse(fetchMock.mock.calls[1][1].body);
    fetchMock.mockResolvedValue(response({ ...snapshot, service_id: 'boot-B', revision: '1' })); await adapter.refresh();
    finish(response({ request_id: oldRequest.request_id, snapshot, fault: null }));
    await expect(pending).rejects.toThrow('restarted'); expect(adapter.snapshot().service_id).toBe('boot-B');
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh(); expect(adapter.snapshot().service_id).toBe('boot-B');
  });
  it('starts backend IQ tasks without receiver preflight or a browser IQ subscription and keeps service state on refresh', async () => {
    snapshot.capabilities.commands!.push('record_start', 'record_stop');
    snapshot.capabilities.recording = true; snapshot.capabilities.recording_kinds = ['iq'];
    fetchMock.mockResolvedValue(response(snapshot)); await adapter.refresh(); fetchMock.mockClear();
    fetchMock.mockImplementation(async (_url, options) => {
      const request = JSON.parse(options.body);
      const next = clone(snapshot); next.recordings = [{ task_id: 'task-A', service_id: 'boot-A', receiver_uid: uid,
        state: 'recording', fault: null, tracks: [{ track_id: 'iq-A', kind: 'iq', format: 'cf32_le', source: 'baseband',
          compression: 'zstd_auto_lossless', state: 'recording', segments: [], raw_bytes: '9007199254740999', stored_bytes: '5000' }] }];
      return response({ request_id: request.request_id, snapshot: next, fault: null });
    });
    const reply = await adapter.dispatch({ type: 'record_start', request_id: 'record', receiver_uid: uid, kinds: ['iq'] });
    expect(reply.snapshot.recordings[0].tracks[0].raw_bytes).toBe('9007199254740999');
    expect(fetchMock).toHaveBeenCalledOnce(); expect(fetchMock.mock.calls[0][0]).toBe('/api/sdr/receivers');
    expect(Socket.instances).toHaveLength(0);
    adapter.dispose(); expect(fetchMock).toHaveBeenCalledOnce();
  });
  it('keeps a pressed workbench control alive across status refreshes until its click is delivered', async () => {
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    const control = overview.querySelector<HTMLButtonElement>('[data-action=tab-output]')!;
    control.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    await adapter.refresh(); await adapter.refresh();
    expect(control.isConnected).toBe(true); control.click();
    expect(overview.querySelector('[data-action=tab-output]')?.getAttribute('aria-pressed')).toBe('true');
    expect(overview.querySelector('[data-action=output]')).not.toBeNull();
    ui.dispose(); session.dispose();
  });
  it('retains keyboard focus, search selection and list controls without requiring a pointer guard', async () => {
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    const tab = overview.querySelector<HTMLButtonElement>('[data-action=tab-output]')!;
    const row = overview.querySelector('.mc-row')!, details = row.querySelector<HTMLButtonElement>('[data-action=detail]')!;
    tab.focus(); await adapter.refresh();
    expect(document.activeElement).toBe(tab); expect(overview.querySelector('[data-action=tab-output]')).toBe(tab);
    const search = overview.querySelector<HTMLInputElement>('[data-search]')!;
    search.focus(); search.value = snapshot.receivers[0].confirmed.name; search.setSelectionRange(1, 3); search.dispatchEvent(new Event('input', { bubbles: true }));
    await adapter.refresh();
    expect(document.activeElement).toBe(search); expect(search.selectionStart).toBe(1); expect(search.selectionEnd).toBe(3);
    expect(row.isConnected).toBe(true); expect(row.querySelector('[data-action=detail]')).toBe(details);
    tab.click(); expect(overview.querySelector('[data-action=output]')).not.toBeNull();
    ui.dispose(); session.dispose();
  });
  it('updates receiver failures around a retained draft instead of freezing the whole editor', async () => {
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    const input = overview.querySelector<HTMLInputElement>('[name=frequency]')!;
    input.value = '7.123456'; input.dispatchEvent(new Event('input', { bubbles: true }));
    const next = clone(snapshot); next.revision = '11'; next.receivers[0].fault = { scope: 'receiver', code: 'INPUT_LOST', message: 'Input interrupted', retryable: false };
    fetchMock.mockResolvedValue(response(next)); await adapter.refresh();
    expect(overview.querySelector('[name=frequency]')).toBe(input); expect(input.value).toBe('7.123456');
    expect(overview.querySelector('[data-key=receiver-fault]')?.textContent).toContain('Input interrupted');
    ui.dispose(); session.dispose();
  });
  it('rejects a delayed task snapshot even when receiver configuration revision is unchanged', async () => {
    const newer = { ...clone(snapshot), state_revision: '9007199254740999' };
    newer.recordings = [{ task_id: 'finished', receiver_uid: uid, state: 'completed', tracks: [], fault: null }];
    fetchMock.mockResolvedValue(response(newer)); await adapter.refresh();
    const late = { ...clone(snapshot), state_revision: '9007199254740998' };
    late.recordings = [{ ...newer.recordings[0], state: 'recording' }];
    fetchMock.mockResolvedValue(response(late)); await adapter.refresh();
    expect(adapter.snapshot().recordings[0].state).toBe('completed');
    expect(adapter.snapshot().state_revision).toBe('9007199254740999');
  });
  it('preserves a native disclosure when release refresh runs before its queued toggle event', async () => {
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    const summary = overview.querySelector<HTMLElement>('[data-section=iq] summary')!;
    summary.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    summary.dispatchEvent(new Event('pointerup', { bubbles: true })); summary.click();
    await vi.advanceTimersByTimeAsync(0); await adapter.refresh();
    expect(overview.querySelector<HTMLDetailsElement>('[data-section=iq]')!.open).toBe(true);
    ui.dispose(); session.dispose();
  });
  it('shares one subscription, retains precise clocks and delivers actual CF32 samples', async () => {
    const received: ReceivedIq[] = []; adapter.onIq(value => received.push(value));
    await output(); await output(); expect(Socket.instances).toHaveLength(1); expect(adapter.snapshot().outputs[0].state).toBe('starting');
    const socket = Socket.instances[0]; start(socket); socket.emit(block());
    expect(adapter.snapshot().outputs[0]).toMatchObject({ state: 'active', samples: '2', bytes: '72', actual_rate_hz: 62500000 / 10368 });
    expect(received[0].clock).toMatchObject({ input_index: '9007199254761733', output_index: '102', fraction_den: '1', delay_seconds: 0.012 });
    expect([...received[0].iq]).toEqual([0.5, -0.25, 0.125, 0.75]);
    socket.emit({ type: 'stream_boundary', next: null, reason: 'stopped', drained: true }); socket.close();
    expect(adapter.snapshot().outputs[0].state).toBe('ended'); expect(fetchMock).not.toHaveBeenCalled();
  });
  it('isolates a throwing observer and rejects corrupted IQ without altering reception', async () => {
    const log = vi.spyOn(console, 'error').mockImplementation(() => {}), receive = vi.fn();
    adapter.onIq(() => { throw new Error('observer'); }); adapter.onIq(receive);
    await output(); const socket = Socket.instances[0]; start(socket); socket.emit(block());
    expect(receive).toHaveBeenCalledOnce(); expect(log).toHaveBeenCalledOnce(); expect(adapter.snapshot().outputs[0].state).toBe('active');
    socket.emit(block('AUD2')); expect(adapter.snapshot().outputs[0].state).toBe('failed');
    expect(adapter.snapshot().receivers).toEqual(snapshot.receivers); expect(fetchMock).not.toHaveBeenCalled();
  });
  it('reports a disconnect without a drained boundary as failure; disposing sends no stop', async () => {
    await output(); start(Socket.instances[0]); Socket.instances[0].close();
    expect(adapter.snapshot().outputs[0]).toMatchObject({ state: 'failed', fault: { code: 'DISCONNECTED' } });
    await output(); adapter.dispose(); expect(Socket.instances[1].close).toHaveBeenCalled(); expect(fetchMock).not.toHaveBeenCalled();
  });
  it('clears rejected validation when a form value changes, including a checkbox change', () => {
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const configure = vi.spyOn(session, 'configure').mockResolvedValue();
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    const form = overview.querySelector<HTMLFormElement>('[data-config]')!, input = form.querySelector<HTMLInputElement>('[name=frequency]')!;
    input.value = '99'; input.dispatchEvent(new Event('input', { bubbles: true }));
    form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    expect(overview.querySelector('[data-preview] [role=alert]')).not.toBeNull();
    input.value = '98'; input.dispatchEvent(new Event('input', { bubbles: true }));
    expect(overview.querySelector('[data-preview]')!.textContent).toBe('');
    form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    const agc = form.querySelector<HTMLInputElement>('[name=agc]')!; agc.checked = false; agc.dispatchEvent(new Event('change', { bubbles: true }));
    expect(overview.querySelector('[data-preview]')!.textContent).toBe('');
    expect(configure).not.toHaveBeenCalled();
    ui.dispose(); session.dispose();
  });
  it('preserves an unfocused draft across polling; closing views keeps IQ alive', async () => {
    const session = new WorkbenchSession(adapter), host = document.createElement('div'); document.body.append(host);
    const ui = mountWorkbench(session, host), overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    const form = overview.querySelector<HTMLFormElement>('[data-config]')!, input = form.querySelector<HTMLInputElement>('[name=frequency]')!;
    input.value = '7.123456'; input.dispatchEvent(new Event('input', { bubbles: true })); input.blur();
    await adapter.refresh(); expect(overview.querySelector<HTMLInputElement>('[name=frequency]')!.value).toBe('7.123456');
    await adapter.refresh(); expect(overview.querySelector('[data-draft-status]')!.textContent).toContain('Unapplied');
    expect(overview.querySelector('[data-action=rec-iq]')).toBeNull();
    expect(overview.querySelector('[data-action=scene]')).toBeNull();
    await output(); start(Socket.instances[0]); overview.querySelector<HTMLButtonElement>('[data-close]')!.click(); ui.recall(); ui.dispose(); session.dispose();
    expect(Socket.instances[0].close).not.toHaveBeenCalled(); expect(fetchMock.mock.calls.every(call => !call[1]?.method)).toBe(true);
  });
});
