import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clone, type CommandReply, type ReceiverAdapter, type ServiceCommand, type ServiceSnapshot } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { createFloatingWindow } from '../ui/floatingWindow';
import { setTheme } from '../core/theme';
import { setLang } from '../core/i18n';
import { setUiScale } from '../core/uiScale';
import capacity from '../../../contracts/service-capacity.json';
import { Ajv2020 } from 'ajv/dist/2020';
import portableSchema from '../../../tests/fixtures/multichannel/draft-v1.schema.json';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession;
beforeEach(() => { vi.useFakeTimers(); localStorage.clear(); adapter = new FixtureReceiverAdapter(3, 10); session = new WorkbenchSession(adapter); });
afterEach(() => { session.dispose(); adapter.dispose(); vi.useRealTimers(); vi.restoreAllMocks(); document.body.replaceChildren(); });
const settle = async <T>(promise: Promise<T>): Promise<T> => { await vi.runAllTimersAsync(); return promise; };

it('derives advertised capacity from the shared contract and enforces the recording pool across kinds', async () => {
  const defaults = new FixtureReceiverAdapter();
  expect(defaults.snapshot().capabilities.max_channels).toBe(capacity.MAX_CHANNELS);
  expect(defaults.snapshot().capabilities.max_recording_tasks).toBe(capacity.MAX_CHANNELS);
  defaults.dispose();
  for (const [uid, kind] of [['fixture-c1', 'iq'], ['fixture-c1', 'audio'], ['fixture-c2', 'results']] as const) {
    expect((await settle(adapter.dispatch({ type: 'record_start', request_id: `${uid}-${kind}`, receiver_uid: uid, kinds: [kind] }))).fault).toBeNull();
  }
  expect(adapter.snapshot().recording_resources).toEqual({ active_tasks: 3, max_tasks: 3, available_tasks: 0 });
  expect((await settle(adapter.dispatch({ type: 'record_start', request_id: 'over', receiver_uid: 'fixture-c3', kinds: ['iq'] }))).fault?.code).toBe('LIMIT');
  await settle(adapter.dispatch({ type: 'record_stop', request_id: 'stop', task_id: adapter.snapshot().recordings[0].task_id }));
  expect(adapter.snapshot().recording_resources?.available_tasks).toBe(1);
});

function controlledSession() {
  session.dispose();
  const initial = clone(adapter.snapshot()); initial.service_id = 'service-a';
  let accept!: (snapshot: ServiceSnapshot) => void;
  const replies: ((reply: CommandReply) => void)[] = [];
  const dispatch = vi.fn((_command: ServiceCommand) => new Promise<CommandReply>(resolve => replies.push(resolve)));
  const controlled: ReceiverAdapter = { snapshot: () => clone(initial), subscribe: listener => { accept = listener; return () => {}; },
    preflight: command => adapter.preflight(command), dispatch };
  session = new WorkbenchSession(controlled);
  return { initial, dispatch, replies, accept: (snapshot: ServiceSnapshot) => accept(clone(snapshot)) };
}

describe('workbench state generations and command ordering', () => {
  it.each(['restart', 'preset'])('releases an interrupted clear on %s without letting its late completion unlock a newer clear', async reason => {
    const c = controlledSession();
    const old = session.clearChannels(['fixture-c1', 'fixture-c2'], 'fixture-c1');
    await vi.advanceTimersByTimeAsync(0);
    expect(session.clearingChannels).toBe(true);
    const replacement = { ...clone(c.initial), service_id: reason === 'restart' ? 'service-b' : 'service-a' };
    if (reason === 'restart') c.accept(replacement);
    else document.dispatchEvent(new Event('websa:preset-applied'));
    expect(session.clearingChannels).toBe(false);
    const current = session.clearChannels(['fixture-c1'], 'fixture-c1');
    await vi.advanceTimersByTimeAsync(0);
    expect(c.dispatch).toHaveBeenCalledTimes(2);
    c.replies[0]({ request_id: c.dispatch.mock.calls[0][0].request_id, snapshot: c.initial, fault: null });
    await old;
    expect(session.clearingChannels).toBe(true);
    expect(c.dispatch).toHaveBeenCalledTimes(2);
    c.replies[1]({ request_id: c.dispatch.mock.calls[1][0].request_id, snapshot: replacement, fault: null });
    await current;
    expect(session.clearingChannels).toBe(false);
    expect(session.snapshot.service_id).toBe(replacement.service_id);
  });

  it('does not guess a created receiver when external additions make the reply ambiguous', async () => {
    const c = controlledSession();
    const pending = session.create(c.initial.receivers[0].confirmed);
    await vi.advanceTimersByTimeAsync(0);
    const snapshot = clone(c.initial); snapshot.revision = '2';
    snapshot.receivers.push(...['external-a', 'external-b'].map(uid => ({ ...clone(c.initial.receivers[0]), receiver_uid: uid })));
    c.replies[0]({ request_id: c.dispatch.mock.calls[0][0].request_id, snapshot, fault: null });
    const result = await pending;
    expect(result).toMatchObject({ receiver_uid: null, fault: { code: 'IDENTITY', retryable: false } });
    expect(session.snapshot.receivers).toHaveLength(5);
    expect(session.selected).toBe('fixture-c1'); expect(session.routes.size).toBe(0);
    expect(session.fault?.code).toBe('IDENTITY');
    expect(c.dispatch).toHaveBeenCalledOnce();
  });

  it('cancels a queued creation after Preset without dispatching it or selecting a receiver', async () => {
    const c = controlledSession();
    const first = session.configure('fixture-c1', c.initial.receivers[0].confirmed);
    const queued = session.create(c.initial.receivers[0].confirmed);
    await vi.advanceTimersByTimeAsync(0);
    document.dispatchEvent(new Event('websa:preset-applied'));
    c.replies[0]({ request_id: c.dispatch.mock.calls[0][0].request_id, snapshot: c.initial, fault: null });
    await first;
    expect(await queued).toMatchObject({ receiver_uid: null, fault: { code: 'SOURCE_RESTARTED' } });
    expect(c.dispatch).toHaveBeenCalledOnce();
    expect(session.selected).toBe('fixture-c1'); expect(session.routes.size).toBe(0);
  });

  it('serializes configuration and deletion against the shared commit revision', async () => {
    const dispatch = vi.spyOn(adapter, 'dispatch'), config = clone(session.snapshot.receivers[0].confirmed);
    config.name = 'Updated';
    const edit = session.configure('fixture-c1', config);
    const retire = session.send({ type: 'retire', request_id: 'retire-other', receiver_uid: 'fixture-c2' });
    await vi.advanceTimersByTimeAsync(0); expect(dispatch).toHaveBeenCalledOnce();
    expect(dispatch.mock.calls[0][0].type).toBe('configure');
    await settle(Promise.all([edit, retire]));
    expect(dispatch.mock.calls.map(([command]) => command.type)).toEqual(['configure', 'retire']);
    expect(session.snapshot.receivers[0].confirmed.name).toBe('Updated');
    expect(session.snapshot.receivers.some(r => r.receiver_uid === 'fixture-c2')).toBe(false);
  });

  it('rejects a configuration after deletion is requested, retaining its rejected draft', async () => {
    const config = clone(session.snapshot.receivers[0].confirmed), dispatch = vi.spyOn(adapter, 'dispatch');
    config.name = 'Too late';
    const retiring = session.send({ type: 'retire', request_id: 'retire', receiver_uid: 'fixture-c1' });
    await session.configure('fixture-c1', config);
    expect(session.edits.get('fixture-c1')?.fault?.code).toBe('BUSY');
    await settle(retiring);
    expect(dispatch).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ type: 'retire' }));
  });

  it('invalidates old edits and monitor routes on restart, including a reused UID', async () => {
    const c = controlledSession(), config = clone(c.initial.receivers[0].confirmed);
    config.name = 'Old intent'; session.monitor('fixture-c1', { enabled: true });
    const pending = session.configure('fixture-c1', config);
    await vi.advanceTimersByTimeAsync(0);
    void session.configure('fixture-c1', { ...config, name: 'Old queued intent' });
    const restarted = { ...clone(c.initial), service_id: 'service-b' }; c.accept(restarted);
    c.replies[0]({ request_id: 'old', snapshot: c.initial, fault: null }); await pending;
    expect(c.dispatch).toHaveBeenCalledOnce(); expect(session.snapshot.service_id).toBe('service-b');
    expect(session.edits.size).toBe(0); expect(session.routes.size).toBe(0); expect(session.fault).toBeNull();
    c.accept(c.initial); expect(session.snapshot.service_id).toBe('service-b');
  });

  it.each(['restart', 'preset'])('keeps new retirement ownership when a callback predating %s completes', async reason => {
    const c = controlledSession();
    const first = session.send({ type: 'retire', request_id: 'old-first', receiver_uid: 'fixture-c1' });
    const queued = session.send({ type: 'retire', request_id: 'old-queued', receiver_uid: 'fixture-c2' });
    await vi.advanceTimersByTimeAsync(0);
    const restarted = { ...clone(c.initial), service_id: reason === 'restart' ? 'service-b' : 'service-a' };
    if (reason === 'restart') c.accept(restarted);
    else document.dispatchEvent(new Event('websa:preset-applied'));
    const current = session.send({ type: 'retire', request_id: 'new', receiver_uid: 'fixture-c2' });
    await vi.advanceTimersByTimeAsync(0); expect(c.dispatch).toHaveBeenCalledTimes(2);
    c.replies[0]({ request_id: 'old-first', snapshot: c.initial, fault: null });
    const old = await Promise.all([first, queued]);
    expect(old.every(reply => reply.fault?.code === 'SOURCE_RESTARTED')).toBe(true);
    expect(session.retiring('fixture-c2')).toBe(true);
    c.replies[1]({ request_id: 'new', snapshot: { ...restarted, receivers: restarted.receivers.filter(r => r.receiver_uid !== 'fixture-c2') }, fault: null });
    await current; expect(session.retiring('fixture-c2')).toBe(false);
  });

  it('ignores late completions after disposal and does not replay queued edits', async () => {
    const c = controlledSession(), config = clone(c.initial.receivers[0].confirmed);
    const pending = session.configure('fixture-c1', config); await vi.advanceTimersByTimeAsync(0);
    void session.configure('fixture-c1', { ...config, name: 'Queued' });
    const notify = vi.fn(); session.subscribe(notify); session.dispose();
    c.replies[0]({ request_id: 'old', snapshot: c.initial, fault: null }); await pending;
    expect(c.dispatch).toHaveBeenCalledOnce(); expect(notify).not.toHaveBeenCalled();
    expect((await session.send({ type: 'retire', request_id: 'closed', receiver_uid: 'fixture-c2' })).fault?.code).toBe('CLOSED');
  });

  it('rejects older telemetry snapshots within the same configuration revision', () => {
    const c = controlledSession(); c.accept({ ...c.initial, state_revision: '20' });
    c.accept({ ...c.initial, state_revision: '19', receivers: [] });
    expect(session.snapshot.state_revision).toBe('20'); expect(session.snapshot.receivers).toHaveLength(3);
  });

  it('keeps a newer command error when an older independent command completes later', async () => {
    const c = controlledSession();
    const older = session.send({ type: 'output', request_id: 'older', receiver_uid: 'fixture-c1', enabled: true });
    const newer = session.send({ type: 'record_start', request_id: 'newer', receiver_uid: 'fixture-c2', kinds: ['iq'] });
    c.replies[1]({ request_id: 'newer', snapshot: c.initial,
      fault: { scope: 'request', code: 'REJECTED', message: 'Newer failure', retryable: true } });
    await newer; c.replies[0]({ request_id: 'older', snapshot: c.initial, fault: null }); await older;
    expect(session.fault?.message).toBe('Newer failure');
  });
});

describe('receiver deletion requests', () => {
  it('shares duplicate requests and serializes deletion of different receivers', async () => {
    const dispatch = vi.spyOn(adapter, 'dispatch');
    const first = session.send({ type: 'retire', request_id: 'first', receiver_uid: 'fixture-c1' });
    const duplicate = session.send({ type: 'retire', request_id: 'duplicate', receiver_uid: 'fixture-c1' });
    const second = session.send({ type: 'retire', request_id: 'second', receiver_uid: 'fixture-c2' });
    expect(session.retiring('fixture-c1')).toBe(true); expect(session.retiring('fixture-c2')).toBe(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(dispatch).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ receiver_uid: 'fixture-c1' }));
    const replies = await settle(Promise.all([first, duplicate, second]));
    expect(replies.map(r => r.request_id)).toEqual(['first', 'duplicate', 'second']);
    expect(replies.every(r => r.fault === null)).toBe(true);
    expect(dispatch.mock.calls.map(([command]) => 'receiver_uid' in command && command.receiver_uid)).toEqual(['fixture-c1', 'fixture-c2']);
    expect(session.snapshot.receivers.map(r => r.receiver_uid)).toEqual(['fixture-c3']);
    expect(session.retiring('fixture-c1')).toBe(false); expect(session.retiring('fixture-c2')).toBe(false);
    await session.send({ type: 'retire', request_id: 'late-click', receiver_uid: 'fixture-c1' });
    expect(dispatch).toHaveBeenCalledTimes(2); expect(session.fault).toBeNull();
  });

  it('releases a rejected deletion for retry and clears errors once that receiver is gone', async () => {
    adapter.inject('reject', 'fixture-c1');
    await settle(session.send({ type: 'retire', request_id: 'reject', receiver_uid: 'fixture-c1' }));
    expect(session.fault).toMatchObject({ code: 'FIXTURE_REJECTED', receiver_uid: 'fixture-c1' });
    expect(session.retiring('fixture-c1')).toBe(false);
    // A snapshot from another view/client confirms that the target no longer exists.
    await settle(adapter.dispatch({ type: 'retire', request_id: 'external', receiver_uid: 'fixture-c1' }));
    expect(session.fault).toBeNull();
    session.fault = { scope: 'capture', code: 'CAPTURE_FAILED', message: 'Capture failure', retryable: true };
    await settle(adapter.dispatch({ type: 'retire', request_id: 'other', receiver_uid: 'fixture-c2' }));
    expect(session.fault?.code).toBe('CAPTURE_FAILED');
  });

  it('skips a queued receiver already removed by another client', async () => {
    const dispatch = vi.spyOn(adapter, 'dispatch');
    adapter.inject('late', 'fixture-c1');
    const first = session.send({ type: 'retire', request_id: 'slow-first', receiver_uid: 'fixture-c1' });
    const second = session.send({ type: 'retire', request_id: 'queued-second', receiver_uid: 'fixture-c2' });
    await vi.advanceTimersByTimeAsync(0);
    const external = adapter.dispatch({ type: 'retire', request_id: 'external', receiver_uid: 'fixture-c2' });
    await settle(Promise.all([first, second, external]));
    expect(dispatch.mock.calls.map(([command]) => command.request_id)).toEqual(['slow-first', 'external']);
    expect(session.snapshot.receivers.map(r => r.receiver_uid)).toEqual(['fixture-c3']);
    expect(session.fault).toBeNull();
  });
});

describe('independent receiver, monitor, recording and view lifetimes', () => {
  it('turns a direct Listen action during follow into a manual mix without moving the editing target', () => {
    const before = adapter.snapshot();
    session.monitorMode = 'follow';
    session.select('fixture-c1');
    session.monitor('fixture-c2', { enabled: !session.listening('fixture-c2') });
    expect(session.monitorMode).toBe('manual'); expect(session.selected).toBe('fixture-c1');
    expect(session.audible('fixture-c1')).toBe(true); expect(session.listening('fixture-c2')).toBe(true);
    session.monitorMode = 'follow';
    session.monitor('fixture-c1', { enabled: !session.listening('fixture-c1') });
    expect(session.monitorMode).toBe('manual'); expect(session.audible('fixture-c1')).toBe(false);
    expect(adapter.snapshot()).toEqual(before);
  });
  it('records while editing another channel; view close and local volume cannot alter task or server config', async () => {
    const id = 'fixture-c1';
    await settle(session.send({ type: 'record_start', request_id: 'record', receiver_uid: id, kinds: ['audio', 'iq'] }));
    const recorded = clone(session.snapshot.recordings), server = clone(adapter.snapshot());
    const surface = createFloatingWindow('details-test', () => 'C1', { x: 0, y: 0, w: 480, h: 320 });
    surface.hide(); session.select('fixture-c2'); session.monitor(id, { enabled: true, gain: 0.1, pan: -1, solo: true });
    expect(session.audible(id)).toBe(true);
    expect(session.resolve({ type: 'fixed', receiver_uid: id })).toBe(id);
    expect(session.resolve({ type: 'follow' })).toBe('fixture-c2');
    expect(adapter.snapshot()).toEqual(server);
    const c2 = clone(session.snapshot.receivers[1].confirmed); c2.name = 'Edited C2';
    await settle(session.configure('fixture-c2', c2));
    expect(session.snapshot.recordings).toEqual(recorded);
    expect(session.snapshot.receivers[0]).toEqual(server.receivers[0]);
    surface.dispose();
  });
  it('stops one recording source without closing another source segment', async () => {
    await settle(session.send({ type: 'record_start', request_id: 'audio', receiver_uid: 'fixture-c1', kinds: ['audio'] }));
    await settle(session.send({ type: 'record_start', request_id: 'iq', receiver_uid: 'fixture-c1', kinds: ['iq'] }));
    const [audio, iq] = session.snapshot.recordings;
    await settle(session.send({ type: 'record_stop', request_id: 'stop-audio', task_id: audio.task_id }));
    expect(session.snapshot.recordings[0].state).toBe('completed');
    expect(session.snapshot.recordings[1].state).toBe('recording');
    expect(session.snapshot.recordings[1].tracks[0].segments).toEqual(iq.tracks[0].segments);
  });
  it('removes an audio processor without changing the IQ stream or reviving its completed track', async () => {
    await settle(session.send({ type: 'record_start', request_id: 'mixed', receiver_uid: 'fixture-c1', kinds: ['audio', 'iq'] }));
    const stream = clone(session.snapshot.receivers[0].stream);
    const config = clone(session.snapshot.receivers[0].confirmed); config.purpose = 'iq'; config.processors = [];
    await settle(session.configure('fixture-c1', config));
    const task = session.snapshot.recordings[0];
    expect(task.state).toBe('recording'); expect(task.tracks[0].state).toBe('completed');
    expect(task.tracks[0].segments[0].end_reason).toBe('processor_removed');
    expect(task.tracks[1].state).toBe('recording'); expect(task.tracks[1].segments).toHaveLength(1);
    expect(session.snapshot.receivers[0].stream).toEqual(stream);
  });
  it('exposes preparation/finalization; disk failure stays local and cannot be overwritten by delayed completion', async () => {
    const command = session.send({ type: 'record_start', request_id: 'start', receiver_uid: 'fixture-c1', kinds: ['iq'] });
    await vi.advanceTimersByTimeAsync(10); await command;
    expect(session.snapshot.recordings[0].state).toBe('preparing');
    await vi.advanceTimersByTimeAsync(10);
    expect(session.snapshot.recordings[0].state).toBe('recording');
    const stop = session.send({ type: 'record_stop', request_id: 'stop', task_id: session.snapshot.recordings[0].task_id });
    await vi.advanceTimersByTimeAsync(10); await stop;
    expect(session.snapshot.recordings[0].state).toBe('finalizing');
    adapter.inject('disk', 'fixture-c1'); await vi.runAllTimersAsync();
    expect(session.snapshot.recordings[0].state).toBe('failed');
    expect(session.snapshot.receivers.every(r => r.state === 'running')).toBe(true);
  });
  it('queues latest desired config under delayed replies, preserves rejection, and previews scenes without partial commit', async () => {
    const c = clone(session.snapshot.receivers[0].confirmed); c.baseband.frequency_hz += 1000;
    adapter.inject('late', 'fixture-c1'); const pending = session.configure('fixture-c1', c);
    const newer = clone(c); newer.baseband.frequency_hz += 1000;
    await session.configure('fixture-c1', newer); session.select('fixture-c2');
    await settle(pending);
    expect(session.snapshot.receivers[0].confirmed.baseband.frequency_hz).toBe(newer.baseband.frequency_hz);
    expect(session.snapshot.receivers[1].revision).toBe('1');
    const before = clone(adapter.snapshot()); adapter.inject('reject', 'fixture-c1');
    await settle(session.configure('fixture-c1', c)); expect(adapter.snapshot()).toEqual(before);
    expect(session.edits.get('fixture-c1')?.desired).toEqual(before.receivers[0].confirmed);
    expect(session.edits.get('fixture-c1')?.fault?.code).toBe('FIXTURE_REJECTED');
    const scene = { type: 'scene' as const, request_id: 'scene', expected_revision: before.revision,
      capture_center_hz: 1, configs: before.receivers.map(r => ({ receiver_uid: r.receiver_uid, config: clone(r.confirmed) })) };
    expect(adapter.preflight(scene).accepted).toBe(false);
    await settle(session.send(scene)); expect(adapter.snapshot()).toEqual(before);
  });
  it('commits a whole scene at one boundary without replaying recording tasks and ignores stale snapshots', async () => {
    await settle(session.send({ type: 'record_start', request_id: 'one', receiver_uid: 'fixture-c1', kinds: ['iq'] }));
    await settle(session.send({ type: 'record_start', request_id: 'three', receiver_uid: 'fixture-c3', kinds: ['iq'] }));
    const before = adapter.snapshot();
    const scene = { type: 'scene' as const, request_id: 'scene-ok', expected_revision: before.revision,
      capture_center_hz: before.capture.center_hz + 1000, configs: before.receivers.map(r => ({ receiver_uid: r.receiver_uid,
        config: { ...clone(r.confirmed), baseband: { ...r.confirmed.baseband, frequency_hz: r.confirmed.baseband.frequency_hz + 1000 } } })) };
    const impact = adapter.preflight(scene);
    expect(impact.accepted).toBe(true); expect(impact.affected_tracks).toHaveLength(2);
    expect(adapter.snapshot()).toEqual(before);
    await settle(session.send(scene));
    const current = clone(session.snapshot);
    expect(current.capture.epoch).toBe('2');
    expect(current.recordings.map(t => t.task_id)).toEqual(before.recordings.map(t => t.task_id));
    current.recordings.forEach(t => {
      expect(t.tracks[0].segments).toHaveLength(2);
      expect(t.tracks[0].segments[0].end_reason).toBe('baseband_change');
      expect(t.tracks[0].segments[1].stream.epoch).toBe('2');
    });
    vi.spyOn(adapter, 'dispatch').mockResolvedValue({ request_id: 'stale', snapshot: before, fault: null });
    await session.send({ type: 'output', request_id: 'stale', receiver_uid: 'fixture-c1', enabled: false });
    expect(session.snapshot).toEqual(current);
  });
  it('ends a gapped segment and requires a new stream; no silent recording continuation', async () => {
    await settle(session.send({ type: 'record_start', request_id: 'record', receiver_uid: 'fixture-c3', kinds: ['iq'] }));
    const old = session.snapshot.receivers[2].stream;
    adapter.inject('gap', 'fixture-c3'); await vi.runAllTimersAsync();
    expect(session.snapshot.receivers[2].stream).toBeNull();
    expect(session.snapshot.recordings[0].state).toBe('completed');
    expect(session.snapshot.recordings[0].tracks[0].segments[0].end_reason).toBe('gap');
    adapter.inject('recover', 'fixture-c3');
    expect(session.snapshot.receivers[2].stream).not.toEqual(old);
    expect(session.snapshot.recordings[0].state).toBe('completed');
  });
  it('accepts dynamic counts independent of display position and isolates IQ capabilities', async () => {
    const expanded = new FixtureReceiverAdapter(128, 1);
    const iq = clone(expanded.snapshot().receivers[2].confirmed);
    await settle(expanded.dispatch({ type: 'create', request_id: 'create', config: iq }));
    expect(expanded.snapshot().receivers).toHaveLength(4);
    expect(expanded.snapshot().receivers[3].receiver_uid).not.toBe('fixture-c4');
    const reply = await settle(adapter.dispatch({ type: 'record_start', request_id: 'bad', receiver_uid: 'fixture-c3', kinds: ['audio'] }));
    expect(reply.fault?.code).toBe('NO_CAPABILITY'); expanded.dispose();
  });
});

describe('multiple surface portals', () => {
  it.each([1, 1.5])('drags collapsed strips to the viewport bottom at scale %s and restores expanded bounds', scale => {
    setUiScale(scale);
    const surface = createFloatingWindow('collapsed-bounds', () => 'C1', { x: 30, y: 40, w: 500, h: 320 });
    let height = 60 * scale;
    vi.spyOn(surface.root, 'getBoundingClientRect').mockImplementation(() => ({ height } as DOMRect));
    const head = surface.root.querySelector<HTMLElement>('.mc-head')!;
    const pointer = (target: HTMLElement, type: string, x: number, y: number): void => {
      target.dispatchEvent(new MouseEvent(type, { bubbles: true, clientX: x, clientY: y }));
    };
    surface.root.querySelector<HTMLButtonElement>('[data-collapse]')!.click();
    pointer(head, 'pointerdown', 50, 50); pointer(head, 'pointermove', 50, 2000); pointer(head, 'pointerup', 50, 2000);
    expect(Number.parseFloat(surface.root.style.top) * scale).toBe(window.innerHeight - height);
    expect(surface.root.style.height).toBe('auto');
    const resize = surface.root.querySelector<HTMLElement>('[data-resize=e]')!;
    height += 20; // A wrapped text row can grow as width / content changes.
    pointer(resize, 'pointerdown', 500, 0); pointer(resize, 'pointermove', 600, 0); pointer(resize, 'pointerup', 600, 0);
    expect(Number.parseFloat(surface.root.style.top) * scale).toBe(window.innerHeight - height);
    expect(JSON.parse(localStorage.getItem('websa-workbench-layout:collapsed-bounds')!).geometry.h).toBe(320);
    surface.root.querySelector<HTMLButtonElement>('[data-collapse]')!.click();
    expect(Number.parseFloat(surface.root.style.height) * scale).toBe(320);
    expect(Number.parseFloat(surface.root.style.top) * scale).toBe(window.innerHeight - 320);
    surface.dispose(); setUiScale(1);
  });

  it('synchronizes both documents, docks only the closed child and disposes callbacks', () => {
    const frames = [document.createElement('iframe'), document.createElement('iframe')];
    frames.forEach(frame => document.body.appendChild(frame));
    const children = frames.map(f => f.contentWindow!);
    children.forEach(child => { vi.spyOn(child, 'close').mockImplementation(() => {}); vi.spyOn(child, 'focus').mockImplementation(() => {}); });
    vi.spyOn(window, 'open').mockReturnValueOnce(children[0]).mockReturnValueOnce(children[1]);
    const windows = ['one', 'two'].map(id => createFloatingWindow(id, () => id, { x: 30, y: 40, w: 500, h: 320 }));
    windows.forEach(w => w.portal.popout()); setTheme('light'); setLang('zh'); setUiScale(1.25);
    for (const child of children) {
      expect(child.document.documentElement.dataset.theme).toBe('light');
      expect(child.document.documentElement.lang).toBe('zh-CN');
      expect(child.document.documentElement.dataset.uiScale).toBe('1.25');
    }
    children[0].dispatchEvent(new Event('beforeunload'));
    expect(windows[0].root.ownerDocument).toBe(document);
    expect(windows[1].root.ownerDocument).toBe(children[1].document);
    expect(windows[0].root.style.left).toBe('24px');
    windows.forEach(w => w.dispose()); setTheme('dark'); setLang('en'); setUiScale(1);
    expect(document.querySelector('[data-view-id]')).toBeNull();
  });
});

// Exercise actual fixture output with the full 2020-12 contract, including its
// conditional processor rules; TypeScript alone cannot enforce those rules.
it('publishes snapshots and commands compatible with the portable backend contract', async () => {
  const validator = new Ajv2020({ allErrors: true }).addSchema(portableSchema);
  const validateSnapshot = validator.compile({ $ref: `${portableSchema.$id}#/$defs/snapshot` });
  const validateCommand = validator.compile({ $ref: `${portableSchema.$id}#/$defs/command` });
  expect(validateSnapshot(adapter.snapshot()), validator.errorsText(validateSnapshot.errors)).toBe(true);
  await settle(adapter.dispatch({ type: 'record_start', request_id: 'contract', receiver_uid: 'fixture-c3', kinds: ['iq'] }));
  expect(validateSnapshot(adapter.snapshot()), validator.errorsText(validateSnapshot.errors)).toBe(true);
  await settle(adapter.dispatch({ type: 'record_start', request_id: 'audio-contract', receiver_uid: 'fixture-c1', kinds: ['audio'] }));
  expect(validateSnapshot(adapter.snapshot()), validator.errorsText(validateSnapshot.errors)).toBe(true);
  const command = { type: 'configure', request_id: 'contract-edit', expected_revision: '1', receiver_uid: 'fixture-c1', config: adapter.snapshot().receivers[0].confirmed };
  expect(validateCommand(command), validator.errorsText(validateCommand.errors)).toBe(true);
  expect(validateCommand({ ...command, expected_revision: 9007199254740992 })).toBe(false);
  expect(validateCommand({ ...command, expected_revision: '18446744073709551615' })).toBe(true);
  for (const expected_revision of ['18446744073709551616', '1\n', '01']) {
    expect(validateCommand({ ...command, expected_revision }), expected_revision).toBe(false);
  }
  for (const config of [
    { ...command.config, purpose: 'iq' },
    { ...command.config, processors: [] },
    { ...command.config, processors: [...command.config.processors, ...command.config.processors] },
  ]) {
    expect(validateCommand({ ...command, config }), JSON.stringify(config)).toBe(false);
  }
});

it('coalesces resume submissions while keeping backend preparation separate from receiving',async()=>{
  const c=controlledSession(),uid=c.initial.receivers[0].receiver_uid;
  const a=session.recover(uid),b=session.recover(uid);await Promise.resolve();
  expect(session.recovering(uid)).toBe(true);expect(c.dispatch).toHaveBeenCalledOnce();
  c.replies[0]({request_id:'recover',snapshot:c.initial,fault:null});await Promise.all([a,b]);
  expect(session.recovering(uid)).toBe(false);expect(c.dispatch).toHaveBeenCalledOnce();
});
