import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { currentChannelConfig, mountSdrQuickChannel } from '../ui/sdrQuickChannel';
import { sdrDemod, sdrIfbw, sdrListenHz, sdrNr, sdrNrAlgo, sdrNrAtten, sdrNrStrength } from '../ui/sdrState';
import { currentSdrChannel, setCurrentSdrChannel } from '../ui/sdrChannels';
import { receiverPlugin } from './pluginFixtures';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession, ui: ReturnType<typeof mountSdrQuickChannel>;
const settle = () => vi.advanceTimersByTimeAsync(100);
const submit = () => document.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true, bubbles: true }));
const choose = (uid: string) => {
  const input = document.querySelector<HTMLInputElement>(`input[value="${uid}"]`)!;
  input.checked = true; input.dispatchEvent(new Event('change', { bubbles: true }));
};
function mount(capacity = 3) {
  adapter = new FixtureReceiverAdapter(capacity, 1);
  const snapshot = adapter.snapshot.bind(adapter);
  vi.spyOn(adapter, 'snapshot').mockImplementation(() => {
    const state = snapshot(); state.capabilities.mode_descriptors = ['nfm', 'am', 'usb', 'ft8'].map(id => receiverPlugin(id)); return state;
  });
  session = new WorkbenchSession(adapter); ui = mountSdrQuickChannel(session, document.body);
  vi.spyOn(adapter, 'dispatch');
}
beforeEach(() => {
  vi.useFakeTimers(); localStorage.clear(); setCurrentSdrChannel(1);
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.open = true; } });
  Object.defineProperty(HTMLDialogElement.prototype, 'close', { configurable: true, value: function(this: HTMLDialogElement) { this.open = false; } });
  sdrListenHz.confirm(7_110_000); sdrDemod.confirm('am'); sdrIfbw.confirm(6000);
  sdrNr.confirm(true); sdrNrAlgo.confirm('dfn'); sdrNrStrength.confirm(0.25); sdrNrAtten.confirm(12);
});
afterEach(() => { ui.dispose(); session.dispose(); adapter.dispose(); document.body.replaceChildren(); vi.useRealTimers(); vi.restoreAllMocks(); });

describe('main spectrum quick channel', () => {
  it.each(['add', 'listen', 'audio', 'iq'] as const)('creates a channel with only the requested %s action', async action => {
    mount(4);
    const previous = structuredClone(session.snapshot.receivers);
    const added = ui.add(action); await settle(); await added;
    const receiver = session.snapshot.receivers[3];
    expect(session.snapshot.receivers.slice(0, 3)).toEqual(previous);
    expect(session.listening(receiver.receiver_uid)).toBe(action === 'listen');
    expect(session.snapshot.recordings.flatMap(task => task.tracks.map(track => track.kind)))
      .toEqual(action === 'audio' || action === 'iq' ? [action] : []);
  });

  it('toggles listening, audio and IQ independently by frequency even when tuning settings change', async () => {
    mount(4);
    const added = ui.add('add'); await settle(); await added;
    const saved = structuredClone(session.snapshot.receivers[3]), uid = saved.receiver_uid;
    sdrNrAtten.confirm(19); sdrIfbw.confirm(3000);
    const action = async (kind: 'add' | 'listen' | 'audio' | 'iq') => {
      const pending = ui.add(kind); await settle(); await pending;
    };
    await action('listen'); expect(session.listening(uid)).toBe(true);
    await action('audio'); await action('iq');
    expect(session.snapshot.recordings.filter(task => task.state === 'recording')).toHaveLength(2);
    expect(session.listening(uid)).toBe(true);
    await action('add');
    expect(session.listening(uid)).toBe(true);
    expect(session.snapshot.recordings.filter(task => task.state === 'recording')).toHaveLength(2);
    await action('audio');
    expect(session.snapshot.recordings.find(task => task.tracks.some(track => track.kind === 'iq'))!.state).toBe('recording');
    expect(session.listening(uid)).toBe(true);
    await action('listen'); expect(session.listening(uid)).toBe(false);
    await action('iq');
    expect(session.snapshot.recordings.every(task => task.state !== 'recording')).toBe(true);
    expect(session.snapshot.receivers).toHaveLength(4);
    expect(session.snapshot.receivers[3]).toEqual(saved);
    expect(session.selected).toBe('fixture-c1');
  });

  it('retains the requested recording action through a capacity replacement dialog', async () => {
    mount(); await ui.add('iq'); choose('fixture-c2'); submit(); await settle();
    expect(session.listening('fixture-c2')).toBe(false);
    expect(session.snapshot.recordings).toMatchObject([{ receiver_uid: 'fixture-c2', tracks: [{ kind: 'iq' }] }]);
  });

  it.each(['listen', 'audio', 'iq'] as const)('re-enables a stopped saved frequency for %s', async action => {
    mount(4);
    const added = ui.add('add'); await settle(); await added;
    const receiver = session.snapshot.receivers[3], config = structuredClone(receiver.confirmed); config.enabled = false;
    const stopped = session.configure(receiver.receiver_uid, config); await settle(); await stopped;
    const result = ui.add(action); await settle(); await result;
    expect(session.snapshot.receivers).toHaveLength(4);
    expect(session.snapshot.receivers[3].confirmed.enabled).toBe(true);
    expect(session.listening(receiver.receiver_uid)).toBe(action === 'listen');
  });

  it.each(['Preset', 'service replacement'])('invalidates an open replacement candidate on %s', async change => {
    mount(); await ui.add(); choose('fixture-c2');
    if (change === 'Preset') document.dispatchEvent(new Event('websa:preset-applied'));
    else { session.snapshot = { ...structuredClone(session.snapshot), service_id: 'replacement-service' }; session.notify(); }
    submit(); await settle();
    expect(adapter.dispatch).not.toHaveBeenCalled();
    expect(session.routes.size).toBe(0);
    expect(document.querySelector('dialog')!.open).toBe(false);
  });

  it('does not enable listening or announce an in-flight addition after its owner is disposed', async () => {
    mount(4);
    let complete!: () => void;
    vi.mocked(adapter.dispatch).mockImplementation(command => new Promise(resolve => {
      const snapshot = structuredClone(session.snapshot);
      if (command.type !== 'create') throw new Error('Expected a create command');
      snapshot.receivers.push({ ...structuredClone(snapshot.receivers[0]), receiver_uid: 'created-after-close',
        display_number: 4, confirmed: command.config });
      complete = () => resolve({ request_id: command.request_id, snapshot, fault: null });
    }));
    const changed = vi.fn(); document.addEventListener('websa:sdr-channels-changed', changed);
    try {
      const added = ui.add(); await settle(); ui.dispose(); complete();
      await added;
      expect(session.snapshot.receivers).toHaveLength(4);
      expect(session.routes.size).toBe(0);
      expect(changed).not.toHaveBeenCalled();
    } finally { document.removeEventListener('websa:sdr-channels-changed', changed); }
  });

  it('fills all sixteen slots and replaces C16 without changing the other receivers', async () => {
    mount(16);
    for (let channel = 4; channel <= 16; channel++) {
      sdrListenHz.confirm(7_100_000 + channel * 1000);
      const added = ui.add(); await settle(); await added;
      expect(session.snapshot.receivers).toHaveLength(channel);
    }
    const before = structuredClone(session.snapshot.receivers);
    const target = before.find(receiver => receiver.display_number === 16)!;
    setCurrentSdrChannel(1); sdrListenHz.confirm(7_130_000);
    vi.mocked(adapter.dispatch).mockClear();
    await ui.add();
    expect(document.querySelectorAll('[name=replace]')).toHaveLength(15);
    expect(adapter.dispatch).not.toHaveBeenCalled();
    choose(target.receiver_uid); submit(); await settle();
    expect(session.snapshot.receivers).toHaveLength(16);
    expect(session.snapshot.receivers.filter(receiver => receiver.receiver_uid !== target.receiver_uid))
      .toEqual(before.filter(receiver => receiver.receiver_uid !== target.receiver_uid));
    expect(session.snapshot.receivers.find(receiver => receiver.receiver_uid === target.receiver_uid)).toMatchObject({ receiver_uid: target.receiver_uid,
      display_number: 16, confirmed: { baseband: { frequency_hz: 7_130_000 } } });
  });
  it('creates independently when space is available, preserving existing receivers', async () => {
    mount(4); const previous = structuredClone(session.snapshot.receivers);
    const added = ui.add(); await settle(); await added;
    expect(session.snapshot.receivers.slice(0, 3)).toEqual(previous);
    const receiver = session.snapshot.receivers[3];
    expect(receiver.confirmed).toMatchObject({ purpose: 'listen', baseband: { frequency_hz: 7_110_000 }, processors: [{ mode: 'am', bandwidth_hz: 6000,
      nr: true, nr_algorithm: 'dfn', nr_strength: 0.25, nr_atten_db: 12 }] });
    expect(session.route(receiver.receiver_uid).enabled).toBe(true);
    expect(document.querySelector('dialog')!.open).toBe(false);
  });
  it.each([3000, 6000])('copies FT8 with a sufficient input passband at %i Hz without changing C1', async bandwidth => {
    mount(4); sdrDemod.confirm('ft8'); sdrIfbw.confirm(bandwidth);
    const before = structuredClone(session.snapshot.receivers);
    const config = currentChannelConfig(session);
    expect(config.baseband.passband_hz).toBe(Math.max(3200, bandwidth) * 2);
    expect(config.baseband.minimum_rate_hz).toBeGreaterThanOrEqual(config.baseband.passband_hz + 2000);
    const added = ui.add(); await settle(); await added;
    expect(session.snapshot.receivers.slice(0, 3)).toEqual(before);
    expect(adapter.dispatch).toHaveBeenCalledTimes(1);
    expect(session.snapshot.receivers[3].confirmed.processors[0]).toMatchObject({ mode: 'ft8', bandwidth_hz: bandwidth });
  });
  it('requires an explicit replacement when full and cancels without a command', async () => {
    mount(); await ui.add();
    expect(document.querySelectorAll('[name=replace]')).toHaveLength(2);
    expect(document.querySelector<HTMLButtonElement>('[type=submit]')!.disabled).toBe(true);
    expect(document.querySelector('[data-targets]')!.textContent).toContain('FT8');
    expect(document.querySelector('[data-targets]')!.textContent).toContain('IQ');
    document.querySelector<HTMLButtonElement>('[data-dismiss]')!.click();
    expect(document.querySelector('dialog')!.open).toBe(false);
    expect(adapter.dispatch).not.toHaveBeenCalled();
  });
  it('replaces only the chosen UID with the frequency frozen when the dialog opened', async () => {
    mount(); const previous = structuredClone(session.snapshot.receivers);
    await ui.add(); choose('fixture-c2');
    sdrListenHz.confirm(7_130_000); submit(); await settle();
    expect(session.snapshot.receivers[0]).toEqual(previous[0]);
    expect(session.snapshot.receivers[2]).toEqual(previous[2]);
    expect(session.snapshot.receivers[1]).toMatchObject({ receiver_uid: 'fixture-c2', confirmed: { baseband: { frequency_hz: 7_110_000 }, purpose: 'listen' } });
    expect(session.route('fixture-c2').enabled).toBe(true);
    expect(document.querySelector('dialog')!.open).toBe(false);
  });
  it('keeps the previous channel and the dialog when the service rejects replacement', async () => {
    mount(); const previous = structuredClone(session.snapshot.receivers);
    await ui.add(); choose('fixture-c2'); adapter.inject('reject', 'fixture-c2'); submit(); await settle();
    expect(session.snapshot.receivers).toEqual(previous);
    expect(session.route('fixture-c2').enabled).toBe(false);
    expect(document.querySelector('dialog')!.open).toBe(true);
    expect(document.querySelector('[data-error]')!.textContent).not.toBe('');
  });
  it('reuses an independent copy on repeated clicks without consuming another channel', async () => {
    mount(4); const first = ui.add(); await settle(); await first;
    vi.mocked(adapter.dispatch).mockClear();
    await ui.add(); expect(adapter.dispatch).not.toHaveBeenCalled();
    expect(document.querySelector('dialog')!.open).toBe(false);
  });
  it('keeps different noise reduction settings as independent channel copies', async () => {
    mount(5); const first = ui.add(); await settle(); await first;
    sdrNrAtten.confirm(18);
    const second = ui.add(); await settle(); await second;
    expect(session.snapshot.receivers).toHaveLength(5);
    expect(session.snapshot.receivers.slice(3).map(r => r.confirmed.processors[0].nr_atten_db)).toEqual([12, 18]);
  });
  it.each([1, 2])('saves the live tuning source C%i independently and reuses its saved copy on repeat', async channel => {
    mount(4);
    setCurrentSdrChannel(channel);
    const sourceUid = `fixture-c${channel}`;
    session.select(sourceUid);
    const config = currentChannelConfig(session);
    const configured = session.configure(sourceUid, config); await settle(); await configured;
    const before = structuredClone(session.snapshot.receivers);
    vi.mocked(adapter.dispatch).mockClear();
    const added = ui.add(); await settle(); await added;
    expect(document.querySelector('dialog')!.open).toBe(false);
    expect(adapter.dispatch).toHaveBeenCalledTimes(1);
    expect(vi.mocked(adapter.dispatch).mock.calls[0][0].type).toBe('create');
    expect(session.snapshot.receivers.slice(0, 3)).toEqual(before);
    const saved = session.snapshot.receivers[3];
    expect(saved.receiver_uid).not.toBe(sourceUid);
    expect(saved.confirmed.baseband).toEqual(config.baseband);
    expect(session.route(saved.receiver_uid).enabled).toBe(true);
    expect(currentSdrChannel()).toBe(channel);
    expect(session.selected).toBe(sourceUid);

    vi.mocked(adapter.dispatch).mockClear();
    expect(await ui.add()).toContain(`C${saved.display_number}`);
    expect(adapter.dispatch).not.toHaveBeenCalled();
    expect(session.snapshot.receivers).toHaveLength(4);
    expect(currentSdrChannel()).toBe(channel);
    expect(session.selected).toBe(saved.receiver_uid);

    const retuned = structuredClone(config); retuned.baseband.frequency_hz += 10_000;
    const tuning = session.configure(sourceUid, retuned); await settle(); await tuning;
    expect(session.snapshot.receivers.find(r => r.receiver_uid === saved.receiver_uid)).toEqual(saved);
  });
  it('offers explicit replacement at capacity even when the tuning source already matches', async () => {
    mount();
    const configured = session.configure('fixture-c1', currentChannelConfig(session)); await settle(); await configured;
    const before = structuredClone(session.snapshot.receivers);
    vi.mocked(adapter.dispatch).mockClear();
    await ui.add();
    expect(document.querySelector('dialog')!.open).toBe(true);
    expect(document.querySelectorAll('[name=replace]')).toHaveLength(2);
    expect(adapter.dispatch).not.toHaveBeenCalled();
    expect(session.snapshot.receivers).toEqual(before);
  });
});
