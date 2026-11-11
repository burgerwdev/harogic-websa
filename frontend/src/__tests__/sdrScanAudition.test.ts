import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { ScanAudition } from '../ui/sdrScanAudition';
import { clone, type ReceiverConfig } from '../core/multichannel';
let adapter: FixtureReceiverAdapter, session: WorkbenchSession, preview: ScanAudition;
const guard = () => ({ capture_epoch: session.snapshot.capture.epoch, signal_id: 's1', signal_generation: '1' });
const settle = async <T>(pending: Promise<T>): Promise<T> => { const outcome = pending.then(value => ({ value }), error => ({ error })); await vi.advanceTimersByTimeAsync(1000); const result = await outcome; if ('error' in result) throw result.error; return result.value; };
const config = (offset = 50000): ReceiverConfig => { const config = clone(session.snapshot.receivers[0].confirmed); config.name = 'Preview test'; config.tags = []; config.baseband.frequency_hz += offset; config.enabled = true; return config; };
beforeEach(() => {
  vi.useFakeTimers(); adapter = new FixtureReceiverAdapter(16, 1); session = new WorkbenchSession(adapter); preview = new ScanAudition(session);
  session.monitorMode = 'manual'; for (const r of session.snapshot.receivers) session.monitor(r.receiver_uid, { enabled: true });
});
afterEach(async () => { await settle(preview.stop()); session.dispose(); adapter.dispose(); vi.useRealTimers(); vi.restoreAllMocks(); });
describe('signal audition ownership and mixing', () => {
  it('uses one receiver while switching, releases it on stop and restores saved routes', async () => {
    const original = clone(session.snapshot.receivers), audible = session.snapshot.receivers.map(r => session.audible(r.receiver_uid)), routes = [...session.routes].map(([uid, r]) => [uid, { ...r }]);
    await settle(preview.start('s1', config(), guard(), true)); const uid = preview.playing!.uid;
    expect(session.snapshot.receivers).toHaveLength(original.length + 1); expect(session.audible(uid)).toBe(true);
    expect(original.every(r => !session.audible(r.receiver_uid))).toBe(true); expect([...session.routes]).toEqual(routes);
    await settle(preview.start('s2', config(80000), guard(), true)); expect(preview.playing!.uid).toBe(uid); expect(session.snapshot.receivers).toHaveLength(original.length + 1);
    await settle(preview.stop()); expect(session.snapshot.receivers).toEqual(original); expect([...session.routes]).toEqual(routes); expect(original.map(r => session.audible(r.receiver_uid))).toEqual(audible);
  });
  it('temporarily overrides follow, Solo and mute without rewriting preferences', async () => {
    session.monitorMode = 'follow'; session.masterMuted = true; const selected = session.selected;
    await settle(preview.start('s1', config(), guard(), true)); const uid = preview.playing!.uid;
    expect(session.audible(uid)).toBe(true); expect(session.monitorMode).toBe('follow'); expect(session.selected).toBe(selected);
    preview.setExclusive(false); expect(session.audible(selected)).toBe(true);
    await settle(preview.stop()); expect(session.masterMuted).toBe(true); expect(session.monitorMode).toBe('follow'); expect(session.selected).toBe(selected);
  });
  it('borrows an existing saved receiver without editing its config or deleting it', async () => {
    const saved = clone(session.snapshot.receivers[0]); const dispatch = vi.spyOn(adapter, 'dispatch');
    await settle(preview.start('s1', config(0), guard(), true)); expect(preview.playing!.uid).toBe(saved.receiver_uid); expect(session.snapshot.receivers).toHaveLength(3);
    await settle(preview.stop()); expect(dispatch).not.toHaveBeenCalled(); expect(session.snapshot.receivers[0]).toEqual(saved);
  });
  it('restores a borrowed disabled receiver after temporary enabling', async () => {
    const row = session.snapshot.receivers[0]; await settle(session.configure(row.receiver_uid, { ...clone(row.confirmed), enabled: false }));
    await settle(preview.start('s1', config(0), guard(), true)); expect(session.snapshot.receivers[0].confirmed.enabled).toBe(true);
    await settle(preview.stop()); expect(session.snapshot.receivers[0].confirmed.enabled).toBe(false); expect(session.snapshot.receivers).toHaveLength(3);
  });
  it('promotes the preview in place and restores the mix while keeping the new channel audible', async () => {
    const requested = config(); await settle(preview.start('s1', requested, guard(), true)); const uid = preview.playing!.uid, stream = clone(session.snapshot.receivers.find(r => r.receiver_uid === uid)!.stream);
    const added = await settle(preview.add(requested, guard())); expect(added!.receiver_uid).toBe(uid); expect(added!.stream).toEqual(stream); expect(added!.confirmed.tags).toEqual([]); expect(preview.playing).toBeNull();
    expect(session.audible(uid)).toBe(true); expect(session.audible(session.snapshot.receivers[0].receiver_uid)).toBe(true);
    await settle(preview.stop()); expect(session.snapshot.receivers.some(r => r.receiver_uid === uid)).toBe(true);
  });
  it('waits for an in-flight creation before releasing it after cancellation', async () => {
    const start = preview.start('s1', config(), guard(), true); await vi.advanceTimersByTimeAsync(1); const stop = preview.stop(); await settle(Promise.all([start, stop]));
    expect(session.snapshot.receivers).toHaveLength(3); expect(preview.playing).toBeNull();
  });
  it('preserves a preview receiver explicitly edited by the user', async () => {
    await settle(preview.start('s1', config(), guard(), true)); const row = session.snapshot.receivers.find(r => r.receiver_uid === preview.playing!.uid)!;
    await settle(session.configure(row.receiver_uid, { ...clone(row.confirmed), name: 'User kept this' })); await settle(preview.stop());
    expect(session.snapshot.receivers.some(r => r.receiver_uid === row.receiver_uid && r.confirmed.name === 'User kept this')).toBe(true);
  });
  it('does not create a second receiver at full capacity, but can borrow compatible IQ/audio', async () => {
    session.snapshot.capabilities.max_channels = 3;
    expect(preview.canAllocate()).toBe(false);
    await settle(preview.start('s1', config(0), guard(), true)); expect(session.snapshot.receivers).toHaveLength(3);
  });
});
