import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clone } from '../core/multichannel';
import { setLang } from '../core/i18n';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { mountWorkbench } from '../ui/workbenchApp';
import { WorkbenchSession } from '../ui/workbenchSession';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession, ui: ReturnType<typeof mountWorkbench>;
let frequency: number;
const save = vi.fn<() => Promise<string | null>>();
const overview = () => document.querySelector<HTMLElement>('.mc-is-overview')!;
const cards = () => [...overview().querySelectorAll<HTMLElement>('.mc-overview-card')];
const rows = () => [...document.querySelectorAll<HTMLElement>('.mc-sidebar .mc-row')];
const settle = () => vi.advanceTimersByTimeAsync(100);
const add = async (mode = 'nfm') => {
  const config = clone(session.snapshot.receivers[0].confirmed);
  config.name = `Saved ${mode}`; config.processors[0].mode = mode;
  const creating = session.create(config); await settle();
  return (await creating).receiver_uid!;
};
beforeEach(() => {
  vi.useFakeTimers(); localStorage.clear(); setLang('en'); save.mockReset(); save.mockResolvedValue(null);
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value(this: HTMLDialogElement) { this.open = true; } });
  Object.defineProperty(HTMLDialogElement.prototype, 'close', { configurable: true, value(this: HTMLDialogElement) { this.open = false; } });
  adapter = new FixtureReceiverAdapter(16, 1); session = new WorkbenchSession(adapter);
  frequency = session.snapshot.receivers[0].confirmed.baseband.frequency_hz;
  const host = document.createElement('div'); document.body.append(host);
  ui = mountWorkbench(session, host, undefined, () => frequency, {
    receiverUid: () => session.snapshot.receivers.find(r => r.display_number === 1)?.receiver_uid ?? null,
    save,
  });
  ui.showChannels();
});
afterEach(() => {
  ui.dispose(); session.dispose(); adapter.dispose(); document.body.replaceChildren(); vi.restoreAllMocks(); vi.useRealTimers();
});

describe('Current and saved channels', () => {
  it('records the saved representative once when Current has no overview card', async () => {
    const uid = await add();
    expect(cards().some(card => card.dataset.tuningSource === 'true')).toBe(false);
    overview().querySelector<HTMLButtonElement>('[data-overview-record]')!.click();
    await vi.advanceTimersByTimeAsync(1000);
    expect(session.snapshot.recordings).toHaveLength(1);
    expect(session.snapshot.recordings.some(task => task.receiver_uid === uid)).toBe(true);
    expect(session.snapshot.recordings.some(task => task.receiver_uid === 'fixture-c1')).toBe(false);
  });
  it('gives the saved card sole speaker control while Current is hidden, including after deletion', async () => {
    session.monitor('fixture-c1', { enabled: true });
    const uid = await add(); session.monitor(uid, { enabled: true });
    expect(session.playbackActive('fixture-c1')).toBe(false);
    expect(session.playbackActive(uid)).toBe(true);
    cards().find(card => card.dataset.uid === uid)!.querySelector<HTMLButtonElement>('[data-action=listen]')!.click();
    expect(session.playbackActive(uid)).toBe(false);
    expect(session.playbackActive('fixture-c1')).toBe(false);
    const deleting = session.send({ type: 'retire', request_id: session.requestId(), receiver_uid: uid });
    await settle(); await deleting;
    expect(cards()[0].dataset.tuningSource).toBe('true');
    expect(session.playbackActive('fixture-c1')).toBe(false);
  });

  it.each(['current', 'saved'])('does not resume Current when deleting the representative in follow mode (%s selected)', async selected => {
    const uid = await add();
    session.monitorMode = 'follow'; session.select(selected === 'current' ? 'fixture-c1' : uid);
    expect(session.playbackActive(uid)).toBe(true);
    expect(session.playbackActive('fixture-c1')).toBe(false);
    const deleting = session.send({ type: 'retire', request_id: session.requestId(), receiver_uid: uid });
    await settle(); await deleting;
    expect(cards()[0].dataset.tuningSource).toBe('true');
    expect(session.playbackActive('fixture-c1')).toBe(false);
    session.monitor('fixture-c1', { enabled: true });
    expect(session.playbackActive('fixture-c1')).toBe(true);
  });

  it('keeps Current controllable when a saved representative stops, without reviving a silenced frequency', async () => {
    session.monitor('fixture-c1', { enabled: true });
    const uid = await add(); session.monitor(uid, { enabled: true }); session.monitor(uid, { enabled: false });
    const saved = session.snapshot.receivers.find(row => row.receiver_uid === uid)!;
    saved.state = 'failed'; session.notify();
    expect(cards().some(card => card.dataset.tuningSource === 'true')).toBe(true);
    expect(session.playbackActive('fixture-c1')).toBe(false);
    session.monitor('fixture-c1', { enabled: true }); expect(session.playbackActive('fixture-c1')).toBe(true);
  });

  it('uses confirmed Current tuning to decide which saved card represents its source', async () => {
    const uid = await add();
    frequency += 1000; await settle();
    expect(cards().some(card => card.dataset.tuningSource === 'true')).toBe(false);
    expect(session.snapshot.receivers.find(row => row.receiver_uid === uid)!.confirmed.baseband.frequency_hz).not.toBe(frequency);
  });

  it('requires confirmation to clear saved channels, stops Current and retains its tuning', async () => {
    const initial = clone(session.snapshot.receivers[0].confirmed);
    for (const row of session.snapshot.receivers) session.monitor(row.receiver_uid, { enabled: true });
    const clear = overview().querySelector<HTMLButtonElement>('[data-overview-clear]')!;
    clear.click(); expect(session.snapshot.receivers).toHaveLength(3);
    document.querySelector<HTMLButtonElement>('[data-clear-cancel]')!.click();
    await settle(); expect(session.snapshot.receivers).toHaveLength(3);
    clear.click(); document.querySelector<HTMLButtonElement>('[data-clear-confirm]')!.click();
    expect(session.clearingChannels).toBe(true);
    expect(session.snapshot.receivers.some(row => session.playbackActive(row.receiver_uid))).toBe(false);
    await vi.advanceTimersByTimeAsync(1000);
    expect(session.clearingChannels).toBe(false);
    expect(session.snapshot.receivers).toHaveLength(1);
    expect(session.snapshot.receivers[0].confirmed).toEqual({ ...initial, enabled: false });
    expect(session.playbackActive('fixture-c1')).toBe(false);
    expect(cards()).toHaveLength(1);
    expect(document.querySelector<HTMLDialogElement>('[data-clear-channels-dialog]')!.open).toBe(false);
  });
  it('pins the temporary source first in both views and offers saving instead of deletion or renaming', () => {
    expect(cards()[0].dataset.tuningSource).toBe('true');
    expect(cards()[0].textContent).toContain('Current');
    expect(cards()[0].querySelector('[data-card-rename]')).toBeNull();
    expect(cards()[0].querySelector('[data-action=retire]')).toBeNull();
    cards()[0].querySelector<HTMLButtonElement>('[data-action=save-current]')!.click();
    expect(save).toHaveBeenCalledOnce();
    const sort = overview().querySelector<HTMLSelectElement>('[data-overview-sort]')!;
    sort.value = 'frequency'; sort.dispatchEvent(new Event('change', { bubbles: true }));
    expect(cards()[0].dataset.uid).toBe('fixture-c1');
    expect(overview().querySelector('[data-card-capacity]')!.textContent).toContain('2 saved');
    ui.showOverview();
    expect(rows()[0].dataset.tuningSource).toBe('true');
    expect(rows()[0].querySelector('strong')!.textContent).toBe('Current');
    expect(rows()[0].querySelector('[data-action=retire]')).toBeNull();
    expect(document.querySelector<HTMLInputElement>('[data-channel-name]')!.readOnly).toBe(true);
  });

  it('hides Current after saving and marks the saved frequency independently of editor selection in both views', async () => {
    const uid = await add();
    expect(cards().some(card => card.dataset.tuningSource === 'true')).toBe(false);
    expect(cards().filter(card => card.dataset.frequencyMatch === 'true').map(card => card.dataset.uid)).toEqual([uid]);
    session.select('fixture-c3'); ui.showOverview();
    expect(rows().some(row => row.dataset.tuningSource === 'true')).toBe(false);
    expect(rows().filter(row => row.dataset.frequencyMatch === 'true').map(row => row.dataset.uid)).toEqual([uid]);
    expect(rows().find(row => row.dataset.uid === 'fixture-c3')!.classList.contains('mc-selected')).toBe(true);
    expect(rows().find(row => row.dataset.uid === uid)!.querySelector<HTMLElement>('[data-row-current]')!.hidden).toBe(false);
  });

  it('restores Current at a new frequency while preserving the saved channel and monitoring', async () => {
    const uid = await add(), saved = clone(session.snapshot.receivers.find(r => r.receiver_uid === uid)!);
    session.monitor(uid, { enabled: true, gain: .4 });
    const config = clone(session.snapshot.receivers[0].confirmed);
    frequency += 10_000; config.baseband.frequency_hz = frequency;
    const retuning = session.configure('fixture-c1', config); await settle(); await retuning;
    expect(cards()[0].dataset.tuningSource).toBe('true');
    expect(cards().find(card => card.dataset.uid === uid)!.dataset.frequencyMatch).toBe('false');
    expect(session.snapshot.receivers.find(r => r.receiver_uid === uid)).toEqual(saved);
    expect(session.route(uid)).toMatchObject({ enabled: true, gain: .4 });
    ui.showOverview();
    expect(rows()[0].dataset.tuningSource).toBe('true');
    expect(rows().find(row => row.dataset.uid === uid)!.dataset.frequencyMatch).toBe('false');
  });

  it('marks every same-frequency saved channel even with different modes or a stopped receiver', async () => {
    const am = await add('am'), usb = await add('usb');
    const stopped = clone(session.snapshot.receivers.find(r => r.receiver_uid === usb)!.confirmed); stopped.enabled = false;
    const stopping = session.configure(usb, stopped); await settle(); await stopping;
    expect(cards().some(card => card.dataset.tuningSource === 'true')).toBe(false);
    expect(cards().filter(card => card.dataset.frequencyMatch === 'true').map(card => card.dataset.uid)).toEqual([am, usb]);
    ui.showOverview();
    expect(rows().filter(row => row.dataset.frequencyMatch === 'true').map(row => row.dataset.uid)).toEqual([am, usb]);
    expect(rows().find(row => row.dataset.uid === usb)!.textContent).toContain('Stopped');
  });

  it('updates matching when main tuning changes without a workbench selection event', async () => {
    ui.showOverview();
    frequency = session.snapshot.receivers[1].confirmed.baseband.frequency_hz;
    session.snapshot.receivers[1].confirmed = { ...clone(session.snapshot.receivers[0].confirmed),
      baseband: { ...session.snapshot.receivers[0].confirmed.baseband, frequency_hz: frequency } };
    session.snapshot.receivers[0].confirmed.baseband.frequency_hz = frequency;
    await settle();
    expect(rows().some(row => row.dataset.tuningSource === 'true')).toBe(false);
    expect(rows().filter(row => row.dataset.frequencyMatch === 'true').map(row => row.dataset.uid)).toEqual(['fixture-c2']);
    ui.showChannels();
    expect(cards().filter(card => card.dataset.frequencyMatch === 'true').map(card => card.dataset.uid)).toEqual(['fixture-c2']);
  });
  it('keeps a same-frequency channel visible and highlighted while its wider passband is out of capture', async () => {
    const config = clone(session.snapshot.receivers[0].confirmed);
    config.name = 'Wide saved channel'; config.baseband.passband_hz = 300_000;
    config.baseband.minimum_rate_hz = 400_000;
    const creating = session.create(config); await settle(); const uid = (await creating).receiver_uid!;
    expect(uid).toBeTruthy();
    session.monitor(uid, { enabled: true, gain: .43, pan: -.2 });
    const saved = cards().find(card => card.dataset.uid === uid)!;
    expect(saved.dataset.frequencyMatch).toBe('true'); expect(saved.dataset.outOfBand).toBe('true');
    expect(saved.textContent).toContain('Out of band');
    expect(cards().some(card => card.dataset.tuningSource === 'true')).toBe(true);
    expect(session.audible(uid)).toBe(false);
    expect(session.route(uid)).toMatchObject({ enabled: true, gain: .43, pan: -.2 });
    const filter = overview().querySelector<HTMLSelectElement>('[data-overview-band]')!;
    filter.value = 'in-band'; filter.dispatchEvent(new Event('change', { bubbles: true }));
    expect(cards().some(card => card.dataset.uid === uid)).toBe(false);
    filter.value = 'all'; filter.dispatchEvent(new Event('change', { bubbles: true }));
    expect(cards().some(card => card.dataset.uid === uid)).toBe(true);
    ui.showOverview();
    const row = rows().find(row => row.dataset.uid === uid)!;
    expect(row.dataset.frequencyMatch).toBe('true'); expect(row.textContent).toContain('Out of band · waiting for coverage');
    document.querySelector<HTMLButtonElement>('[data-action=filter-in-band]')!.click();
    expect(rows().some(row => row.dataset.uid === uid)).toBe(false);
    document.querySelector<HTMLButtonElement>('[data-action=filter-all]')!.click();
    expect(rows().some(row => row.dataset.uid === uid)).toBe(true);
  });
});
