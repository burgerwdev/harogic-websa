import { afterEach, beforeEach, expect, it } from 'vitest';
import { clone } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { loadWorkbenchPreferences, resetWorkbenchPreferences } from '../ui/workbenchPreferences';

beforeEach(() => localStorage.clear());
afterEach(() => localStorage.clear());
const session = (): WorkbenchSession => {
  const adapter = new FixtureReceiverAdapter(), snapshot = clone(adapter.snapshot()); adapter.dispose();
  snapshot.transport = 'native';
  return new WorkbenchSession({ snapshot: () => snapshot, subscribe: () => () => {},
    preflight: () => { throw new Error('unused'); }, dispatch: async () => { throw new Error('unused'); } });
};
it('restores speaker level and monitor rule across sessions and resets the mounted owner after Preset', () => {
  const first = session(); first.masterGain = 0.25; first.monitorMode = 'follow'; first.dispose();
  const restored = session();
  expect(restored.masterGain).toBe(0.25); expect(restored.monitorMode).toBe('follow');
  resetWorkbenchPreferences(); document.dispatchEvent(new Event('websa:preset-applied'));
  expect(restored.masterGain).toBe(0.7); expect(restored.monitorMode).toBe('manual');
  expect(loadWorkbenchPreferences()).toEqual({ masterGain: 0.7, monitorMode: 'manual' });
  restored.dispose();
});
it('rejects malformed speaker preferences and out-of-range values', () => {
  for (const saved of ['broken', '{"masterGain":-1,"monitorMode":"unknown"}', '{"masterGain":"2"}']) {
    localStorage.setItem('websa-workbench-audio-v1', saved);
    expect(loadWorkbenchPreferences()).toEqual({ masterGain: 0.7, monitorMode: 'manual' });
  }
});
