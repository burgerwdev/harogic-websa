import { afterEach, expect, it } from 'vitest';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import type { DecoderLogEntry } from '../sdr/decoderLog';
import { receiverLevel, receiverLevelScale } from '../ui/receiverLevel';

const adapter = new FixtureReceiverAdapter();
afterEach(() => adapter.dispose());
it('rejects stale and foreign stream readings and clears levels when reception stops', () => {
  const receiver = adapter.snapshot().receivers[0];
  const entry: DecoderLogEntry = { id: 1, at: Date.now(), mode: receiver.confirmed.processors[0].mode,
    source: `${receiver.stream!.epoch}:${receiver.stream!.channel_id}`, kind: 'status', provider: 'native', bytes: 40,
    data: { inputDbFs: -24, rmsDbFs: -12 } };
  expect(receiverLevel(receiver, [entry])).toEqual({ db: -12, source: 'audio' });
  expect(receiverLevel(receiver, [{ ...entry, source: 'other' }])).toBeNull();
  const processed = { ...entry, source: '38:38', timing: { input_index: '100', input_rate: 48000,
    fraction_num: '0', fraction_den: '1', delay_seconds: 0, delay_known: true, causal_presentation: false,
    baseband_stream: receiver.stream } };
  expect(receiverLevel(receiver, [processed])).toEqual({ db: -12, source: 'audio' });
  expect(receiverLevel(receiver, [{ ...processed, timing: { ...processed.timing,
    baseband_stream: { ...receiver.stream, channel_id: 'retired-stream' } } }])).toBeNull();
  expect(receiverLevel(receiver, [{ ...entry, at: Date.now() - 4000 }])).toBeNull();
  expect(receiverLevel({ ...receiver, state: 'stopped' }, [entry], { enabled: true, playing: true, rms: .5 })).toBeNull();
  expect(receiverLevel(receiver, [], { enabled: true, playing: true, rms: .1 })?.db).toBeCloseTo(-20);
  expect(receiverLevel(receiver, [], { enabled: true, playing: false, rms: .1 })).toBeNull();
});

it('follows AM programme level despite a steady carrier and uses a fixed audio scale', () => {
  const receiver = adapter.snapshot().receivers[0];
  receiver.confirmed.processors[0].mode = 'am';
  const entry: DecoderLogEntry = { id: 1, at: Date.now(), mode: 'am',
    source: `${receiver.stream!.epoch}:${receiver.stream!.channel_id}`, kind: 'status', provider: 'native', bytes: 40,
    data: { inputDbFs: -38, rmsDbFs: -48, peakDbFs: -40 } };
  const quiet = receiverLevel(receiver, [entry]);
  const loud = receiverLevel(receiver, [{ ...entry, data: { inputDbFs: -38, rmsDbFs: -12, peakDbFs: -6 } }]);
  expect(quiet).toEqual({ db: -48, source: 'audio' });
  expect(loud).toEqual({ db: -12, source: 'audio' });
  expect(receiverLevelScale(quiet)).toEqual({ min: -60, value: -48, percent: 20 });
  expect(receiverLevelScale(loud)).toEqual({ min: -60, value: -12, percent: 80 });
  expect(receiverLevel(receiver, [{ ...entry, data: { inputDbFs: -38 } }])).toEqual({ db: -38, source: 'input' });
  expect(receiverLevelScale(null).percent).toBe(0);
  expect(receiverLevelScale({ db: -160, source: 'audio' }).percent).toBe(0);
});
