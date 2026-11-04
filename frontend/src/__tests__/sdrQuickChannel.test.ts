import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountSdrQuickChannel } from '../ui/sdrQuickChannel';
import { sdrDemod, sdrIfbw, sdrListenHz, sdrNr, sdrNrAlgo, sdrNrAtten, sdrNrStrength } from '../ui/sdrState';
import { setCurrentSdrChannel } from '../ui/sdrChannels';
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
  it('requires an explicit replacement when full and cancels without a command', async () => {
    mount(); await ui.add();
    expect(document.querySelectorAll('[name=replace]')).toHaveLength(3);
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
});
