import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { clone } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { receiverPlugin } from './pluginFixtures';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession;
beforeEach(() => {
  adapter = new FixtureReceiverAdapter(); session = new WorkbenchSession(adapter);
  const first = session.snapshot.receivers[0];
  for (const receiver of session.snapshot.receivers.slice(1)) {
    receiver.confirmed = clone(first.confirmed);
    receiver.confirmed.processors[0].id = `processor-${receiver.display_number}`;
    session.monitor(receiver.receiver_uid, { enabled: true });
  }
  session.snapshot.receivers[2].confirmed.baseband.frequency_hz += 10_000;
  session.monitor(first.receiver_uid, { enabled: true });
});
afterEach(() => { session.dispose(); adapter.dispose(); });

describe('identical receiver monitoring', () => {
  it('plays a saved frequency once, then restores independent mixing when the scratch receiver moves', () => {
    expect(session.audible('fixture-c1')).toBe(true);
    expect(session.audible('fixture-c2')).toBe(false);
    expect(session.duplicateMonitor('fixture-c2')).toBe('fixture-c1');
    expect(session.audible('fixture-c3')).toBe(true);
    session.snapshot.receivers[0].confirmed.baseband.frequency_hz += 1000;
    expect(session.audible('fixture-c1')).toBe(true);
    expect(session.audible('fixture-c2')).toBe(true);
    session.snapshot.receivers[0].confirmed.baseband.frequency_hz -= 1000;
    expect(session.audible('fixture-c2')).toBe(false);
  });
  it('promotes the saved channel when the duplicate stops or its monitor is disabled', () => {
    session.monitor('fixture-c1', { enabled: false });
    expect(session.audible('fixture-c2')).toBe(true);
    session.monitor('fixture-c1', { enabled: true });
    session.snapshot.receivers[0].state = 'stopped';
    expect(session.audible('fixture-c2')).toBe(true);
  });
  it('keeps Solo and follow-selection usable on a duplicate channel', () => {
    session.monitor('fixture-c2', { solo: true });
    expect(session.audible('fixture-c1')).toBe(false);
    expect(session.audible('fixture-c2')).toBe(true);
    session.monitorMode = 'follow'; session.select('fixture-c2');
    expect(session.audible('fixture-c2')).toBe(true);
  });
  it.each(['bandwidth_hz', 'pcm_gain', 'squelch_dbm', 'agc'] as const)('keeps different %s settings independent', field => {
    const processor = session.snapshot.receivers[1].confirmed.processors[0];
    if (field === 'agc') processor.agc = false;
    else processor[field] = 1234;
    expect(session.audible('fixture-c1')).toBe(true);
    expect(session.audible('fixture-c2')).toBe(true);
  });
  it('compares the native service audio signature, including settings absent from editable config', () => {
    session.snapshot.transport = 'native';
    session.snapshot.capabilities.mode_descriptors = [receiverPlugin('nfm')];
    const [first, second] = session.snapshot.receivers;
    first.audio_signature = 'same-settings'; second.audio_signature = 'same-settings';
    expect(session.audible(second.receiver_uid)).toBe(false);
    second.audio_signature = 'different-nr-settings';
    expect(session.audible(second.receiver_uid)).toBe(true);
  });
  it('respects deliberately different local volume and pan', () => {
    session.monitor('fixture-c2', { pan: 1 });
    expect(session.audible('fixture-c2')).toBe(true);
    session.monitor('fixture-c2', { pan: 0, gain: 0.3 });
    expect(session.audible('fixture-c2')).toBe(true);
  });
});
