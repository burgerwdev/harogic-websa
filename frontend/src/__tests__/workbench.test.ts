import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clone } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { createFloatingWindow } from '../ui/floatingWindow';
import { setTheme } from '../core/theme';
import { setLang } from '../core/i18n';
import { setUiScale } from '../core/uiScale';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession;
beforeEach(() => { vi.useFakeTimers(); localStorage.clear(); adapter = new FixtureReceiverAdapter(3, 10); session = new WorkbenchSession(adapter); });
afterEach(() => { session.dispose(); adapter.dispose(); vi.useRealTimers(); vi.restoreAllMocks(); document.body.replaceChildren(); });
const settle = async <T>(promise: Promise<T>): Promise<T> => { await vi.runAllTimersAsync(); return promise; };

describe('independent receiver, monitor, recording and view lifetimes', () => {
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

// Validate the portable draft independently of TypeScript's compile-time structural checks.
// This intentionally implements only the JSON Schema vocabulary used by this local schema.
it('publishes snapshots and commands compatible with the portable backend contract', async () => {
  const { readFileSync } = await import('node:fs');
  const schema = JSON.parse(readFileSync('../tests/fixtures/multichannel/draft-v1.schema.json', 'utf8'));
  const validate = (value: unknown, rule: any): boolean => {
    if (rule.$ref) return validate(value, schema.$defs[rule.$ref.split('/').pop()]);
    if (rule.oneOf) return rule.oneOf.filter((r: any) => validate(value, r)).length === 1;
    if (rule.anyOf) return rule.anyOf.some((r: any) => validate(value, r));
    if (rule.const !== undefined && value !== rule.const) return false;
    if (rule.enum && !rule.enum.includes(value)) return false;
    const types = Array.isArray(rule.type) ? rule.type : [rule.type];
    const type = value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value;
    if (rule.type && !types.includes(type) && !(types.includes('integer') && Number.isInteger(value))) return false;
    if (typeof value === 'string' && ((rule.pattern && !new RegExp(rule.pattern).test(value)) || (rule.maxLength && value.length > rule.maxLength))) return false;
    if (typeof value === 'number' && ((rule.minimum !== undefined && value < rule.minimum) || (rule.exclusiveMinimum !== undefined && value <= rule.exclusiveMinimum))) return false;
    if (Array.isArray(value)) return (!rule.minItems || value.length >= rule.minItems) && (!rule.uniqueItems || new Set(value).size === value.length) && value.every(v => validate(v, rule.items));
    if (type === 'object') {
      const object = value as Record<string, unknown>;
      return (rule.required ?? []).every((k: string) => k in object)
        && Object.entries(object).every(([k, v]) => rule.properties[k] ? validate(v, rule.properties[k]) : rule.additionalProperties !== false);
    }
    return true;
  };
  expect(validate(adapter.snapshot(), schema.$defs.snapshot)).toBe(true);
  await settle(adapter.dispatch({ type: 'record_start', request_id: 'contract', receiver_uid: 'fixture-c3', kinds: ['iq'] }));
  expect(validate(adapter.snapshot(), schema.$defs.snapshot)).toBe(true);
  const command = { type: 'configure', request_id: 'contract-edit', expected_revision: '1', receiver_uid: 'fixture-c1', config: adapter.snapshot().receivers[0].confirmed };
  expect(validate(command, schema.$defs.command)).toBe(true);
  expect(validate({ ...command, expected_revision: 9007199254740992 }, schema.$defs.command)).toBe(false);
});
