import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { clone, type ServiceSnapshot } from '../core/multichannel';
import { receiverPlugin } from './pluginFixtures';

const mocks = vi.hoisted(() => ({ connect: vi.fn(), views: vi.fn(), quick: vi.fn(), streams: vi.fn() }));
vi.mock('../core/nativeReceiverAdapter', () => ({ NativeReceiverAdapter: { connect: mocks.connect } }));
vi.mock('../ui/workbenchApp', () => ({ mountWorkbench: mocks.views }));
vi.mock('../ui/sdrQuickChannel', () => ({ mountSdrQuickChannel: mocks.quick }));
vi.mock('../ui/nativeWorkbenchStreams', () => ({ mountNativeWorkbenchStreams: mocks.streams }));
vi.mock('../ui/sdrChannels', () => ({ confirmSdrReceiverNumbers: vi.fn(), currentSdrChannel: () => 1 }));
vi.mock('../ui/sdrState', () => ({ sdrAudioOn: { get: () => true } }));

const adapters: FixtureReceiverAdapter[] = [];
function adapter(service: string): FixtureReceiverAdapter {
  const value = new FixtureReceiverAdapter(), snapshot = value.snapshot();
  snapshot.transport = 'native'; snapshot.service_id = service; snapshot.capabilities.monitor_mixing = true;
  snapshot.capabilities.mode_descriptors = snapshot.capabilities.modes.map(id => receiverPlugin(id));
  vi.spyOn(value, 'snapshot').mockImplementation(() => structuredClone(snapshot));
  vi.spyOn(value, 'dispatch'); vi.spyOn(value, 'dispose'); adapters.push(value); return value;
}
const leave = (): void => { window.dispatchEvent(new Event('pagehide')); };
beforeEach(() => {
  vi.useFakeTimers(); vi.resetModules(); localStorage.clear(); sessionStorage.clear();
  Object.values(mocks).forEach(mock => mock.mockReset());
  mocks.views.mockImplementation(() => ({ showOverview: vi.fn(), showChannels: vi.fn(), dispose: vi.fn(), recall: vi.fn() }));
  mocks.quick.mockImplementation(() => ({ add: vi.fn().mockResolvedValue(null), dispose: vi.fn() }));
  mocks.streams.mockImplementation(() => ({ dispose: vi.fn() }));
});
afterEach(() => {
  leave(); for (const value of adapters.splice(0)) value.dispose();
  document.body.replaceChildren(); vi.restoreAllMocks(); vi.useRealTimers();
});

describe('native workbench page ownership', () => {
  it('shares one session for workbench, overview and quick channel actions', async () => {
    const current = adapter('shared-service'); mocks.connect.mockResolvedValue(current);
    const native = await import('../ui/nativeWorkbench');
    await native.openNativeChannelOverview(); await native.openNativeWorkbench(); await native.openNativeChannelOverview();
    expect(mocks.connect).toHaveBeenCalledOnce(); expect(mocks.views).toHaveBeenCalledOnce();
    expect(mocks.views.mock.results[0].value.showChannels).toHaveBeenCalledTimes(2);
    expect(mocks.views.mock.results[0].value.showOverview).toHaveBeenCalledOnce();
    expect(current.dispatch).not.toHaveBeenCalled();
  });
  it.each(['manual', 'follow'].flatMap(mode => [false, true].map(muted => ({ mode: mode as 'manual' | 'follow', muted }))))(
    'restores Current playback after service replacement with $mode monitoring and master mute=$muted', async ({ mode, muted }) => {
    const current = adapter('first-service'); mocks.connect.mockResolvedValue(current);
    let publish!: (snapshot: ServiceSnapshot) => void;
    vi.spyOn(current, 'subscribe').mockImplementation(listener => { publish = listener; return vi.fn(); });
    const native = await import('../ui/nativeWorkbench');
    const session = (await native.nativeWorkbenchSession())!;
    const oldUid = session.snapshot.receivers[0].receiver_uid;
    session.monitorMode = mode; session.masterMuted = muted; session.masterGain = .35;
    session.monitor(oldUid, { gain: .4, pan: .6 });
    const replacement = clone(session.snapshot);
    replacement.service_id = 'second-service';
    replacement.receivers.forEach(row => { row.receiver_uid = `new-${row.receiver_uid}`; });
    const uid = replacement.receivers[0].receiver_uid;
    publish(replacement);
    expect(session.routes.has(oldUid)).toBe(false);
    expect(session.selected).toBe(uid);
    expect(session.route(uid)).toEqual({ enabled: true, gain: .7, pan: 0, solo: false });
    expect(session.playbackActive(uid)).toBe(true);
    expect(session.routes.size).toBe(1);
    expect(session.monitorMode).toBe(mode);
    expect(session.masterMuted).toBe(muted); expect(session.masterGain).toBe(.35);
    // Live snapshots must not overwrite an explicit listening choice after recovery.
    session.monitor(uid, { enabled: false }); publish(clone(replacement));
    expect(session.route(uid).enabled).toBe(false);
    expect(current.dispatch).not.toHaveBeenCalled();
  });
  it.each([false, true])('waits for Current after restart and preserves an explicit monitor-off choice=%s', async monitorOff => {
    const current = adapter('first-service'); mocks.connect.mockResolvedValue(current);
    let publish!: (snapshot: ServiceSnapshot) => void;
    vi.spyOn(current, 'subscribe').mockImplementation(listener => { publish = listener; return vi.fn(); });
    const native = await import('../ui/nativeWorkbench');
    const session = (await native.nativeWorkbenchSession())!;
    const replacement = clone(session.snapshot); replacement.service_id = 'second-service';
    replacement.receivers.forEach(row => { row.receiver_uid = `new-${row.receiver_uid}`; });
    publish({ ...replacement, receivers: [] });
    expect(session.routes.size).toBe(0);
    const uid = replacement.receivers[0].receiver_uid;
    if (monitorOff) session.monitor(uid, { enabled: false });
    publish(replacement);
    expect(session.route(uid).enabled).toBe(!monitorOff);
    // Retiring Current in this same service must not re-enable a later replacement.
    publish({ ...replacement, receivers: [] });
    replacement.receivers[0].receiver_uid = 'recreated-current'; publish(replacement);
    expect(session.route('recreated-current').enabled).toBe(false);
    expect(current.dispatch).not.toHaveBeenCalled();
  });
  it('recreates disposed owners only after a new action following pagehide', async () => {
    const first = adapter('first-service'), second = adapter('second-service');
    mocks.connect.mockResolvedValueOnce(first).mockResolvedValueOnce(second);
    const native = await import('../ui/nativeWorkbench');
    await native.openNativeWorkbench();
    const firstView = mocks.views.mock.results[0].value, firstQuick = mocks.quick.mock.results[0].value;
    leave();
    expect(first.dispose).toHaveBeenCalledOnce();
    expect(firstView.dispose).toHaveBeenCalledOnce(); expect(firstQuick.dispose).toHaveBeenCalledOnce();
    window.dispatchEvent(new Event('pageshow'));
    expect(mocks.connect).toHaveBeenCalledOnce();
    await native.openNativeWorkbench();
    expect(mocks.connect).toHaveBeenCalledTimes(2);
    expect(mocks.views).toHaveBeenCalledTimes(2); expect(mocks.quick).toHaveBeenCalledTimes(2);
    expect(mocks.views.mock.results[1].value.showOverview).toHaveBeenCalledOnce();
    expect(first.dispatch).not.toHaveBeenCalled(); expect(second.dispatch).not.toHaveBeenCalled();
  });

  it('retires a connection that completes after pagehide without replacing the new owner', async () => {
    const late = adapter('late-service'), current = adapter('current-service');
    let complete!: (value: FixtureReceiverAdapter) => void;
    mocks.connect.mockImplementationOnce(() => new Promise(resolve => { complete = resolve; })).mockResolvedValueOnce(current);
    const native = await import('../ui/nativeWorkbench');
    const oldOpening = native.openNativeWorkbench(); leave(); window.dispatchEvent(new Event('pageshow'));
    const newOpening = native.openNativeWorkbench();
    try { expect(mocks.connect).toHaveBeenCalledTimes(2); }
    finally { complete(late); await Promise.allSettled([oldOpening, newOpening]); }
    expect(late.dispose).toHaveBeenCalledOnce(); expect(current.dispose).not.toHaveBeenCalled();
    expect(mocks.views).toHaveBeenCalledOnce(); expect(mocks.quick).toHaveBeenCalledOnce();
    expect(mocks.views.mock.calls[0][0].snapshot.service_id).toBe('current-service');
    expect(late.dispatch).not.toHaveBeenCalled(); expect(current.dispatch).not.toHaveBeenCalled();
  });
});
