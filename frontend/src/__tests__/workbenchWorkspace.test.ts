import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clone, type ServiceSnapshot } from '../core/multichannel';
import { getLang, setLang } from '../core/i18n';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountWorkbench } from '../ui/workbenchApp';
import { receiverPlugin } from './pluginFixtures';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession, ui: ReturnType<typeof mountWorkbench>, overview: HTMLElement;
let language: ReturnType<typeof getLang>;
const element = <T extends HTMLElement = HTMLElement>(selector: string): T => overview.querySelector<T>(selector)!;
const click = (action: string): void => element<HTMLButtonElement>(`[data-action="${action}"]`).click();
const edit = (name: string, value: string): void => {
  const input = element<HTMLInputElement>(`[data-config] [name="${name}"], [data-channel-name][name="${name}"]`);
  input.value = value; input.dispatchEvent(new Event('input', { bubbles: true }));
};
const change = (name: string, value: string): void => {
  const select = element<HTMLSelectElement>(`[data-config] [name="${name}"]`);
  select.value = value; select.dispatchEvent(new Event('change', { bubbles: true }));
};
const submit = (): void => { element<HTMLFormElement>('[data-config]').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); };
const settle = async (): Promise<void> => { await vi.advanceTimersByTimeAsync(1000); };

beforeEach(() => {
  vi.useFakeTimers(); localStorage.clear(); language = getLang(); setLang('en');
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.open = true; } });
  Object.defineProperty(HTMLDialogElement.prototype, 'close', { configurable: true, value: function(this: HTMLDialogElement) { this.open = false; } });
  adapter = new FixtureReceiverAdapter(8, 10); session = new WorkbenchSession(adapter);
  const host = document.createElement('div'); document.body.append(host); ui = mountWorkbench(session, host);
  overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
});
afterEach(() => { ui.dispose(); session.dispose(); adapter.dispose(); document.body.replaceChildren(); setLang(language); vi.useRealTimers(); vi.restoreAllMocks(); });

function nativeWorkspace(quota?: ServiceSnapshot['recording_resources']): void {
  ui.dispose(); session.dispose(); document.body.replaceChildren();
  const snapshot = adapter.snapshot.bind(adapter);
  vi.spyOn(adapter, 'snapshot').mockImplementation(() => {
    const state = snapshot(); state.transport = 'native'; state.service_id = 'native-test';
    state.capabilities.commands = ['configure', 'create', 'retire']; state.capabilities.monitor_mixing = true;
    if (quota) {
      state.capabilities.commands.push('record_start', 'record_stop');
      state.capabilities.recording_kinds = ['iq', 'audio', 'results'];
      state.capabilities.max_recording_tasks = 8; state.recording_resources = quota;
    }
    state.capabilities.mode_descriptors = ['nfm', 'am', 'usb', 'ft8', 'drm', 'drmplus'].map(id =>
      receiverPlugin(id, ['drm', 'drmplus'].includes(id) ? { outputs: ['audio'] } : {}));
    return state;
  });
  session = new WorkbenchSession(adapter); ui = mountWorkbench(session, document.body);
  overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
}

describe('per-channel audio settings', () => {
  it('shows the service recording quota and keeps existing stops usable when all slots are held', async () => {
    const started = adapter.dispatch({ type: 'record_start', request_id: 'existing-iq', receiver_uid: 'fixture-c1', kinds: ['iq'] });
    await settle();
    await started;
    nativeWorkspace({ active_tasks: 8, max_tasks: 8, available_tasks: 0 });
    click('tab-recording');
    expect(element('[data-recording-capacity]').textContent).toContain('8 / 8');
    expect(element('[data-recording-capacity]').textContent).toContain('0 available');
    expect(element<HTMLButtonElement>('[data-editor] [data-action=rec-iq]').disabled).toBe(false);
    expect(element<HTMLButtonElement>('[data-editor] [data-action=rec-audio]').disabled).toBe(true);
    expect(element<HTMLButtonElement>('[data-editor] [data-action=rec-audio]').title).toContain('slots are full');
  });
  it('edits the name in the heading and submits it with the existing settings form', async () => {
    nativeWorkspace();
    const input = element<HTMLInputElement>('[data-channel-name]');
    expect(input.closest('[data-advanced-settings]')).toBeNull();
    expect(input.form).toBe(element('[data-config]'));
    edit('name', 'My receiver'); input.blur(); session.notify();
    expect(element<HTMLInputElement>('[data-channel-name]').value).toBe('My receiver');
    expect(new FormData(element<HTMLFormElement>('[data-config]')).get('name')).toBe('My receiver');
    const frequency = session.snapshot.receivers[0].confirmed.baseband.frequency_hz;
    submit(); await settle();
    expect(session.snapshot.receivers[0].confirmed.name).toBe('My receiver');
    expect(session.snapshot.receivers[0].confirmed.baseband.frequency_hz).toBe(frequency);
    click('tab-output'); element<HTMLInputElement>('[data-channel-name]').focus();
    expect(element('[data-action=tab-receive]').getAttribute('aria-pressed')).toBe('true');
    expect(element<HTMLInputElement>('[data-channel-name]').form).toBe(element('[data-config]'));
  });

  it('uses mode bandwidth limits and presets without applying until requested', async () => {
    nativeWorkspace(); const configure = vi.spyOn(session, 'configure');
    change('mode', 'am');
    click('advanced-settings');
    element<HTMLButtonElement>('[data-bandwidth="3000"]').click();
    expect(element<HTMLInputElement>('[name=processor-bandwidth]').value).toBe('3000');
    expect(element('[data-bandwidth="3000"]').getAttribute('aria-pressed')).toBe('true');
    expect(element('[name=processor-bandwidth]').closest('.mc-parameter-input')!.textContent).toBe('Hz');
    expect(configure).not.toHaveBeenCalled();
    submit(); await settle();
    expect(session.snapshot.receivers[0].confirmed.processors[0].bandwidth_hz).toBe(3000);
    change('mode', 'usb');
    expect(element('[data-bandwidth="2700"]')).toBeTruthy();
    expect(element<HTMLInputElement>('[name=processor-bandwidth]').min).toBe('101');
    change('mode', 'drm');
    expect(element<HTMLInputElement>('[name=processor-bandwidth]').readOnly).toBe(true);
    expect(element('[data-bandwidth-presets]').children).toHaveLength(1);
  });

  it('applies NR, squelch, AGC and gain only to the selected channel, retaining separate drafts', async () => {
    nativeWorkspace();
    const other = clone(session.snapshot.receivers[1]);
    expect(element('[name=nr]').closest('[data-advanced-settings]')).toBeNull();
    const nr = element<HTMLInputElement>('[name=nr]'); nr.checked = true; nr.dispatchEvent(new Event('change', { bubbles: true }));
    expect(element<HTMLInputElement>('[name=pcm-gain]').type).toBe('range');
    change('nr-algorithm', 'dfn'); edit('nr-atten', '14'); edit('nr-strength', '0.25'); edit('squelch', '-92.3'); edit('pcm-gain', '0.8');
    const agc = element<HTMLInputElement>('[name=agc]'); agc.checked = false; agc.dispatchEvent(new Event('change', { bubbles: true }));
    expect(element('[data-squelch-value]').textContent).toBe('-92.3 dBm');
    expect(element('[data-pcm-gain-value]').textContent).toBe('0.80×');
    expect(element('[data-nr-atten-value]').textContent).toBe('14 dB');
    expect(element('[data-nr-strength]').hidden).toBe(true); expect(element('[data-nr-atten]').hidden).toBe(false);
    session.select('fixture-c2');
    expect(element<HTMLInputElement>('[name=nr]').checked).toBe(false);
    session.select('fixture-c1');
    expect(element<HTMLInputElement>('[name=nr]').checked).toBe(true);
    expect(element<HTMLSelectElement>('[name=nr-algorithm]').value).toBe('dfn');
    expect(element<HTMLInputElement>('[name=nr-atten]').value).toBe('14');
    submit(); await settle();
    expect(session.snapshot.receivers[0].confirmed.processors[0]).toMatchObject({
      nr: true, nr_algorithm: 'dfn', nr_strength: 0.25, nr_atten_db: 14, squelch_dbm: -92.3, agc: false, pcm_gain: 0.8,
    });
    expect(session.snapshot.receivers[1]).toEqual(other);
    expect(element('[data-draft-status]').textContent).toBe('Settings accepted');
    change('mode', 'usb'); submit(); await settle();
    change('mode', 'nfm');
    expect(element<HTMLInputElement>('[name=nr]').checked).toBe(true);
    expect(element<HTMLSelectElement>('[name=nr-algorithm]').value).toBe('dfn');
    expect(element<HTMLInputElement>('[name=nr-atten]').value).toBe('14');
    expect(element('[data-squelch-value]').textContent).toBe('-92.3 dBm');
  });

  it('shows Wiener strength and squelch Off, and hides enhancements for DRM and IQ', () => {
    nativeWorkspace();
    expect(element('[data-squelch-value]').textContent).toBe('Off');
    expect(element('[data-nr-strength]').hidden).toBe(false); expect(element('[data-nr-atten]').hidden).toBe(true);
    edit('nr-strength', '0.75'); expect(element('[data-nr-strength-value]').textContent).toBe('75%');
    change('mode', 'drm');
    expect(element('[data-shared-audio]').hidden).toBe(false); expect(element('[data-monitor-enhancements]').hidden).toBe(true);
    change('mode', ''); expect(element('[data-shared-audio]').hidden).toBe(true);
    change('mode', 'am'); expect(element('[data-monitor-enhancements]').hidden).toBe(false);
  });
});

describe('channel workspace workflows', () => {
  it('disables every delete button for a pending receiver, including independent windows', async () => {
    const dispatch = vi.spyOn(adapter, 'dispatch');
    element<HTMLButtonElement>('[data-uid="fixture-c1"] [data-action=detail]').click();
    const detail = document.querySelector<HTMLElement>('[data-view-id="detail-fixture-c1"]')!;
    const remove = detail.querySelector<HTMLButtonElement>('[data-action=retire]')!;
    remove.click(); remove.click();
    expect(remove.disabled).toBe(true); expect(remove.textContent).toContain('Deleting');
    expect(element<HTMLButtonElement>('[data-uid="fixture-c1"] [data-action=retire]').disabled).toBe(true);
    expect(element<HTMLButtonElement>('.mc-channel-actions [data-action=retire]').disabled).toBe(true);
    expect(element<HTMLButtonElement>('[data-uid="fixture-c2"] [data-action=retire]').disabled).toBe(false);
    element<HTMLButtonElement>('[data-uid="fixture-c2"] [data-action=retire]').click();
    await settle();
    expect(dispatch.mock.calls.filter(([command]) => command.type === 'retire')).toHaveLength(2);
    expect(session.snapshot.receivers.map(r => r.receiver_uid)).toEqual(['fixture-c3']);
    expect(session.fault).toBeNull(); expect(element('[data-key=session-fault]')).toBeNull();
  });

  it('allows a failed deletion to be retried or its error dismissed without refreshing', async () => {
    adapter.inject('reject', 'fixture-c1');
    element<HTMLButtonElement>('.mc-channel-actions [data-action=retire]').click(); await settle();
    expect(element('[data-key=session-fault]').textContent).toContain('FIXTURE_REJECTED');
    expect(element<HTMLButtonElement>('.mc-channel-actions [data-action=retire]').disabled).toBe(false);
    click('dismiss-fault');
    expect(session.fault).toBeNull(); expect(element('[data-key=session-fault]')).toBeNull();
    element<HTMLButtonElement>('.mc-channel-actions [data-action=retire]').click(); await settle();
    expect(session.snapshot.receivers.some(r => r.receiver_uid === 'fixture-c1')).toBe(false);
    expect(element('[data-key=session-fault]')).toBeNull();
  });

  it('groups the reception switch, duplicate and delete, and offers row-specific management shortcuts', async () => {
    const actions = element('.mc-channel-actions');
    expect([...actions.querySelectorAll('[data-action]')].map(button => button.getAttribute('data-action')))
      .toEqual(['toggle-receiver', 'duplicate', 'retire']);
    const toggle = actions.querySelector<HTMLButtonElement>('[role=switch]')!;
    expect(toggle.getAttribute('aria-checked')).toBe('true');
    toggle.click(); await settle();
    expect(element('[role=switch]').getAttribute('aria-checked')).toBe('false');
    element<HTMLButtonElement>('[role=switch]').click(); await settle();
    expect(element('[role=switch]').getAttribute('aria-checked')).toBe('true');
    const row = element('[data-uid="fixture-c2"]');
    expect(row.querySelector('[data-action=duplicate]')).not.toBeNull();
    expect(row.querySelector('[data-action=retire]')).not.toBeNull();
    row.querySelector<HTMLButtonElement>('[data-action=duplicate]')!.click(); await settle();
    const added = session.snapshot.receivers.at(-1)!;
    expect(added.confirmed.baseband.frequency_hz).toBe(7_074_000);
    expect(session.snapshot.receivers).toHaveLength(4);
    element<HTMLButtonElement>('[data-uid="fixture-c2"] [data-action=retire]').click(); await settle();
    expect(session.snapshot.receivers.some(r => r.receiver_uid === 'fixture-c2')).toBe(false);
    expect(session.snapshot.receivers.some(r => r.receiver_uid === 'fixture-c1')).toBe(true);
  });
  it('shows live decoding alongside tuning without opening an extra window', () => {
    session.select('fixture-c2');
    expect(element('[data-inline-decode] .mc-message-table').textContent).toContain('FIXTURE CQ TEST AA00');
    expect(document.querySelectorAll('[data-view-id]')).toHaveLength(1);
    expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.074000');
    expect(element<HTMLButtonElement>('[data-config] [type=submit]').disabled).toBe(true);
  });

  it('converts frequency units without changing the frequency and applies in one submit', async () => {
    const configure = vi.spyOn(session, 'configure');
    edit('frequency', '7.123456');
    change('frequency-unit', 'kHz'); expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7123.456');
    change('frequency-unit', 'Hz'); expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7123456');
    change('frequency-unit', 'MHz'); expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.123456');
    submit();
    expect(configure).toHaveBeenCalledOnce();
    expect(element<HTMLButtonElement>('[data-config] [type=submit]').disabled).toBe(true);
    await settle();
    expect(session.snapshot.receivers[0].confirmed.baseband.frequency_hz).toBe(7123456);
    expect(element('[data-draft-status]').textContent).toBe('Settings accepted');
    expect(element('[data-preview]').textContent).toBe('');
  });

  it('keeps separate drafts across channels and tabs and resets only the current channel', () => {
    edit('frequency', '7.123456'); edit('name', 'My draft'); change('frequency-unit', 'kHz');
    session.select('fixture-c2'); edit('frequency', '7.08');
    click('tab-recording'); click('tab-receive');
    expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.08');
    session.select('fixture-c1');
    expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7123.456');
    expect(element<HTMLSelectElement>('[name=frequency-unit]').value).toBe('kHz');
    expect(element<HTMLInputElement>('[name=name]').value).toBe('My draft');
    change('frequency-unit', 'MHz'); expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.123456');
    click('cancel'); expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.100000');
    session.select('fixture-c2'); expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.08');
    expect(adapter.snapshot().receivers.map(r => r.confirmed.baseband.frequency_hz)).toEqual([7100000, 7074000, 7150000]);
  });

  it('validates locally and preserves rejected settings for correction', async () => {
    const configure = vi.spyOn(session, 'configure');
    edit('frequency', '100'); submit();
    expect(configure).not.toHaveBeenCalled(); expect(element('[data-preview] [role=alert]')).toBeTruthy();
    edit('frequency', '7.12'); expect(element('[data-preview]').textContent).toBe('');
    adapter.inject('reject', 'fixture-c1'); submit(); await settle();
    expect(session.snapshot.receivers[0].confirmed.baseband.frequency_hz).toBe(7100000);
    expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.120000');
    expect(element('[data-key=edit-fault]').textContent).toContain('FIXTURE_REJECTED');
    expect(element<HTMLButtonElement>('[data-config] [type=submit]').disabled).toBe(false);
  });

  it('does not erase edits made while an earlier configuration is being applied', async () => {
    edit('frequency', '7.11'); submit(); edit('frequency', '7.12'); await settle();
    expect(session.snapshot.receivers[0].confirmed.baseband.frequency_hz).toBe(7110000);
    expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.12');
    expect(element('[data-draft-status]').textContent).toContain('updated elsewhere');
    session.select('fixture-c2'); session.select('fixture-c1');
    expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.12');
  });

  it('opens recording controls for a row without applying its unapplied tuning', async () => {
    edit('frequency', '7.12');
    element<HTMLButtonElement>('[data-uid="fixture-c1"] [data-action=recordings]').click();
    expect(element('[data-action=tab-recording]').getAttribute('aria-pressed')).toBe('true');
    expect(element('.mc-record-options').textContent).toContain('Raw IQ signal');
    element<HTMLButtonElement>('[data-editor] [data-action=rec-iq]').click(); await settle();
    expect(session.snapshot.recordings[0].state).toBe('recording');
    expect(session.snapshot.receivers[0].confirmed.baseband.frequency_hz).toBe(7100000);
    click('tab-receive'); expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.12');
    expect(element('[data-draft-status]').textContent).toContain('recording segments');
  });

  it('searches displayed MHz and modes and explains empty filters', () => {
    const search = element<HTMLInputElement>('[data-search]');
    search.value = '7.074'; search.dispatchEvent(new Event('input', { bubbles: true }));
    expect(overview.querySelectorAll('.mc-row')).toHaveLength(1);
    expect(element('.mc-row').dataset.uid).toBe('fixture-c2');
    search.value = 'NFM'; search.dispatchEvent(new Event('input', { bubbles: true }));
    expect(element('.mc-row').dataset.uid).toBe('fixture-c1');
    click('filter-listening'); expect(element('.mc-list-empty').textContent).toContain('No matching');
    click('clear-search'); expect(overview.querySelectorAll('.mc-row')).toHaveLength(3);
  });

  it('collects frequency and mode before creating and listening in one step', async () => {
    click('create');
    const dialog = overview.querySelector<HTMLDialogElement>('[data-create-channel]')!;
    const frequency = dialog.querySelector<HTMLInputElement>('[name=frequency]')!;
    frequency.value = '99'; frequency.dispatchEvent(new Event('input', { bubbles: true }));
    expect(dialog.querySelector<HTMLButtonElement>('[type=submit]')!.disabled).toBe(true);
    expect(dialog.querySelector('[data-create-error]')!.textContent).toContain('capture range');
    frequency.value = '7.11'; frequency.dispatchEvent(new Event('input', { bubbles: true }));
    dialog.querySelector<HTMLButtonElement>('[type=submit]')!.click();
    dialog.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await settle();
    expect(session.snapshot.receivers).toHaveLength(4);
    const added = session.snapshot.receivers[3];
    expect(added.confirmed.baseband.frequency_hz).toBe(7110000);
    expect(added.confirmed.processors[0].mode).toBe('nfm');
    expect(session.selected).toBe(added.receiver_uid); expect(session.listening(added.receiver_uid)).toBe(true);
    expect(dialog.open).toBe(false);
  });

  it('keeps a draft based on an earlier revision for explicit review', async () => {
    edit('frequency', '7.11');
    const config = clone(adapter.snapshot().receivers[0].confirmed); config.name = 'Changed elsewhere';
    const pending = session.configure('fixture-c1', config); await settle(); await pending;
    const configure = vi.spyOn(session, 'configure');
    submit(); expect(configure).not.toHaveBeenCalled();
    expect(element('[data-preview] button').textContent).toContain('Apply changes');
    edit('frequency', '7.12'); expect(element('[data-preview]').textContent).toBe('');
  });
  it('coalesces telemetry for visible receivers without rebuilding the tuning form', async () => {
    ui.dispose(); localStorage.clear();
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
    session.snapshot.receivers[0].confirmed.processors[0].mode = 'am';
    let level = -20;
    const results = vi.fn(() => ({ entries: [{ id: 1, at: 1, source: 'test', provider: 'native', mode: 'am',
      kind: 'status' as const, bytes: 100, data: { type: 'am-info', peakDbFs: level, scope: [0, 0.2, -0.2, 0] } }], evicted: 0 }));
    ui = mountWorkbench(session, document.querySelector('#workbench-app') ?? document.body, { results, status: () => '', retry: () => {} });
    overview = document.querySelector<HTMLElement>('[data-view-id="overview-follow"]')!;
    const form = element('[data-config]'), states = vi.fn(); session.subscribe(states);
    results.mockClear(); edit('frequency', '7.123456');
    for (let index = 0; index < 100; index++) { level = -index; session.notifyResults('fixture-c1'); session.notifyResults('fixture-c2'); }
    expect(results).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(50);
    expect(results).toHaveBeenCalledOnce(); expect(results).toHaveBeenCalledWith('fixture-c1');
    expect(states).not.toHaveBeenCalled();
    expect(element('[data-config]')).toBe(form);
    expect(element<HTMLInputElement>('[name=frequency]').value).toBe('7.123456');
    element<HTMLButtonElement>('[data-close]').click(); results.mockClear();
    session.notifyResults('fixture-c1'); await vi.advanceTimersByTimeAsync(50);
    expect(results).not.toHaveBeenCalled();
    ui.showOverview(); expect(results).toHaveBeenCalledWith('fixture-c1');
  });
});

it('refreshes an undrafted form after a confirmed update while an action button retains focus', async () => {
  nativeWorkspace();
  const row=session.snapshot.receivers[0];
  element<HTMLButtonElement>('[data-action=radiofax_reset_correction]').focus();
  const config=clone(row.confirmed);config.processors[0].radiofax_phase_pixels=0;config.processors[0].radiofax_clock_ppm=0;
  const operation=adapter.dispatch({type:'configure',request_id:'external-action-confirmed',receiver_uid:row.receiver_uid,expected_revision:row.revision,config});
  await settle();await operation;
  expect(element<HTMLFormElement>('[data-config]').dataset.revision).toBe(session.snapshot.receivers[0].revision);
});
