import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { clone } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { applyListen, defaultListenTarget, planListen } from '../ui/workbenchListening';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession;
beforeEach(() => { vi.useFakeTimers(); adapter = new FixtureReceiverAdapter(3, 10); session = new WorkbenchSession(adapter); });
afterEach(() => { session.dispose(); adapter.dispose(); vi.restoreAllMocks(); vi.useRealTimers(); });
const settle = async <T>(work: Promise<T>): Promise<T> => { await vi.runAllTimersAsync(); return work; };
const request = (frequency_hz = 7_120_000, target = 'fixture-c1') => ({ frequency_hz, target, mode: 'nfm' });

it('reuses an existing frequency without reconfiguring streams or recordings or stealing selection', async () => {
  await settle(session.send({ type: 'record_start', request_id: 'record', receiver_uid: 'fixture-c1', kinds: ['iq'] }));
  const before = adapter.snapshot(); const dispatch = vi.spyOn(adapter, 'dispatch');
  session.select('fixture-c2'); session.monitorMode = 'follow';
  const result = await applyListen(session, planListen(session, request(7_100_000)));
  expect(result.fault).toBeNull(); expect(dispatch).not.toHaveBeenCalled();
  expect(adapter.snapshot()).toEqual(before); expect(session.selected).toBe('fixture-c2');
  expect(session.monitorMode).toBe('manual'); expect(session.audible('fixture-c1')).toBe(true);
});

it('previews recording boundaries and retunes only the explicitly chosen listening receiver', async () => {
  for (const uid of ['fixture-c1', 'fixture-c3']) await settle(session.send({ type: 'record_start', request_id: `record-${uid}`, receiver_uid: uid, kinds: ['iq'] }));
  session.select('fixture-c2'); const before = adapter.snapshot();
  expect(defaultListenTarget(session, 7_120_000)).toBe('fixture-c1');
  const plan = planListen(session, request());
  expect(plan.affected_tracks).toEqual([before.recordings[0].tracks[0].track_id]);
  expect(adapter.snapshot()).toEqual(before);
  const pending = applyListen(session, plan);
  expect(session.route('fixture-c1').enabled).toBe(false);
  const result = await settle(pending);
  expect(result.fault).toBeNull(); expect(session.selected).toBe('fixture-c2');
  expect(session.snapshot.receivers.slice(1)).toEqual(before.receivers.slice(1));
  expect(session.snapshot.capture).toEqual(before.capture);
  expect(session.snapshot.recordings[1]).toEqual(before.recordings[1]);
  expect(session.snapshot.recordings[0].task_id).toBe(before.recordings[0].task_id);
  expect(session.snapshot.recordings[0].tracks[0].segments).toHaveLength(2);
  expect(session.audible('fixture-c1')).toBe(true);
});

it('does not enable listening or change confirmed state after a rejected request', async () => {
  const before = adapter.snapshot(); const plan = planListen(session, request());
  adapter.inject('reject', 'fixture-c1');
  const result = await settle(applyListen(session, plan));
  expect(result.fault?.code).toBe('FIXTURE_REJECTED');
  expect(adapter.snapshot()).toEqual(before); expect(session.routes.size).toBe(0);
});

it('creates a separate channel when admitted and suppresses a second simultaneous apply', async () => {
  session.dispose(); adapter.dispose(); adapter = new FixtureReceiverAdapter(4, 10); session = new WorkbenchSession(adapter);
  const before = adapter.snapshot(); session.select('fixture-c2');
  expect(defaultListenTarget(session, 7_120_000)).toBe('new');
  const plan = planListen(session, { ...request(7_120_000, 'new'), mode: 'usb' });
  const first = applyListen(session, plan);
  const second = await applyListen(session, plan); expect(second.fault?.code).toBe('BUSY');
  const result = await settle(first);
  expect(result.fault).toBeNull(); expect(result.receiver_uid).not.toBeNull();
  expect(session.snapshot.receivers).toHaveLength(4);
  expect(session.snapshot.receivers.slice(0, 3)).toEqual(before.receivers);
  expect(session.selected).toBe('fixture-c2'); expect(session.audible(result.receiver_uid!)).toBe(true);
  expect(session.snapshot.receivers[3].confirmed.processors[0].mode).toBe('usb');
});

it('rejects occupied capacity, digital/IQ targets, invalid frequencies and out-of-capture bandwidth', () => {
  expect(planListen(session, request(7_120_000, 'new')).fault?.code).toBe('LIMIT');
  for (const uid of ['fixture-c2', 'fixture-c3']) expect(planListen(session, request(7_120_000, uid)).fault?.code).toBe('TARGET');
  expect(planListen(session, request(NaN)).fault?.code).toBe('FREQUENCY');
  expect(planListen(session, request(7_225_000)).fault?.code).toBe('RF_RANGE');
  expect(planListen(session, { ...request(), mode: 'ft8' }).fault?.code).toBe('MODE');
  expect(session.routes.size).toBe(0);
});

it('does not overwrite a pending edit or silently apply an obsolete preview', async () => {
  const stale = planListen(session, request());
  const config = clone(session.snapshot.receivers[0].confirmed); config.name = 'Edited';
  const edit = session.configure('fixture-c1', config);
  expect(planListen(session, request()).fault?.code).toBe('BUSY');
  await settle(edit);
  expect((await applyListen(session, stale)).fault?.code).toBe('STALE_PREVIEW');
  expect(session.route('fixture-c1').enabled).toBe(false);
});

it('rejects a preview from another service even when its revision, UID and frequency match', async () => {
  session.snapshot = { ...clone(session.snapshot), service_id: 'service-a' };
  const plan = planListen(session, request(7_100_000));
  session.snapshot = { ...clone(session.snapshot), service_id: 'service-b' };
  const result = await applyListen(session, plan);
  expect(result.fault?.code).toBe('STALE_PREVIEW');
  expect(session.routes.size).toBe(0);
});

it('does not enable listening after Preset invalidates an in-flight configuration', async () => {
  const confirmed = clone(session.snapshot);
  let complete!: (reply: { request_id: string; snapshot: typeof confirmed; fault: null }) => void;
  vi.spyOn(adapter, 'dispatch').mockImplementation(command => new Promise(resolve => {
    complete = resolve; confirmed.receivers[0].confirmed.baseband.frequency_hz = 7_120_000;
    confirmed.revision = '2'; confirmed.receivers[0].revision = '2';
    expect(command.type).toBe('configure');
  }));
  const pending = applyListen(session, planListen(session, request()));
  await vi.advanceTimersByTimeAsync(0);
  session.snapshot = clone(confirmed);
  document.dispatchEvent(new Event('websa:preset-applied'));
  complete({ request_id: 'invalidated', snapshot: confirmed, fault: null });
  const result = await pending;
  expect(result.fault).not.toBeNull();
  expect(session.routes.size).toBe(0);
});

it('respects local mute and Solo and never claims to recover an interrupted stream', async () => {
  session.masterGain = 0;
  expect((await applyListen(session, planListen(session, request(7_100_000)))).fault).toBeNull();
  expect(session.route('fixture-c1').enabled).toBe(true); expect(session.audible('fixture-c1')).toBe(false);
  adapter.inject('gap', 'fixture-c1');
  expect(planListen(session, request()).fault?.code).toBe('NO_SOURCE');
});

it('uses the common per-receiver queue and does not listen to a different frequency chosen in another window', async () => {
  adapter.inject('late', 'fixture-c1');
  const apply = applyListen(session, planListen(session, request()));
  expect(session.edits.get('fixture-c1')?.pending).toBe(true);
  const newer = clone(session.snapshot.receivers[0].confirmed); newer.baseband.frequency_hz = 7_130_000;
  await session.configure('fixture-c1', newer);
  const result = await settle(apply);
  expect(result.fault?.code).toBe('CONFIRMATION');
  expect(session.snapshot.receivers[0].confirmed.baseband.frequency_hz).toBe(7_130_000);
  expect(session.route('fixture-c1').enabled).toBe(false);
});

it('reports a receiver retired in another window without leaving the request pending', async () => {
  adapter.inject('late', 'fixture-c1');
  const apply = applyListen(session, planListen(session, request()));
  const retire = session.send({ type: 'retire', request_id: 'retire', receiver_uid: 'fixture-c1' });
  await settle(retire);
  const result = await apply;
  expect(result.fault).not.toBeNull(); expect(session.edits.get('fixture-c1')?.pending).not.toBe(true);
  expect(session.snapshot.receivers.some(r => r.receiver_uid === 'fixture-c1')).toBe(false);
  expect(session.routes.size).toBe(0);
});
