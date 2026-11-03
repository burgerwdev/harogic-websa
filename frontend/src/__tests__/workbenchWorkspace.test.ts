import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clone } from '../core/multichannel';
import { getLang, setLang } from '../core/i18n';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountWorkbench } from '../ui/workbenchApp';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession, ui: ReturnType<typeof mountWorkbench>, overview: HTMLElement;
let language: ReturnType<typeof getLang>;
const element = <T extends HTMLElement = HTMLElement>(selector: string): T => overview.querySelector<T>(selector)!;
const click = (action: string): void => element<HTMLButtonElement>(`[data-action="${action}"]`).click();
const edit = (name: string, value: string): void => {
  const input = element<HTMLInputElement>(`[data-config] [name="${name}"]`);
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

describe('channel workspace workflows', () => {
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
    expect(element('[data-draft-status]').textContent).toBe('Settings applied');
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
