import { afterEach, expect, it } from 'vitest';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import type { DecoderLogEntry } from '../sdr/decoderLog';
import { ReceiverMeter, receiverLevel, receiverLevelScale } from '../ui/receiverLevel';

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
  expect(receiverLevel(receiver, [], { enabled: true, playing: true, rms: .1, peak: .2, meterAt: Date.now() })?.db).toBeCloseTo(-20);
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
  expect(receiverLevel(receiver, [{ ...entry, data: { inputDbFs: -38 } }])).toBeNull();
  expect(receiverLevelScale(null).percent).toBe(0);
  expect(receiverLevelScale({ db: -160, source: 'audio' }).percent).toBe(0);
});

it('uses rendered PCM ahead of newer telemetry and clears paused, muted and expired playback', () => {
  const receiver = adapter.snapshot().receivers[0];
  const playback = { enabled: true, playing: true, rms: .1, peak: .5, meterAt: Date.now(), contextState: 'running' };
  const entry: DecoderLogEntry = { id: 1, at: Date.now(), mode: receiver.confirmed.processors[0].mode,
    source: `${receiver.stream!.epoch}:${receiver.stream!.channel_id}`, kind: 'status', provider: 'native', bytes: 40,
    data: { rmsDbFs: -1 } };
  expect(receiverLevel(receiver, [entry], playback)).toMatchObject({ db: -20, source: 'playback' });
  expect(receiverLevel(receiver, [entry], playback)?.peakDb).toBeCloseTo(-6.02, 2);
  for (const changed of [{ enabled: false }, { playing: false }, { muted: true },
    { contextState: 'suspended' }, { meterAt: Date.now() - 600 }, { rms: NaN }, { peak: -1 }])
    expect(receiverLevel(receiver, [entry], { ...playback, ...changed })).toBeNull();
});

it.each([{ localGain: 0 }, { masterGain: 0 }, { masterMuted: true },
  { localGain: 0, masterGain: 0, masterMuted: true }])('keeps rendered PCM levels before local output controls %j', output => {
  const receiver = adapter.snapshot().receivers[0];
  const playback = { enabled: true, playing: true, rms: .125, peak: .25, meterAt: Date.now(), contextState: 'running', ...output };
  const entry: DecoderLogEntry = { id: 1, at: Date.now(), mode: receiver.confirmed.processors[0].mode,
    source: `${receiver.stream!.epoch}:${receiver.stream!.channel_id}`, kind: 'status', provider: 'native', bytes: 40,
    data: { rmsDbFs: -1 } };
  const level = receiverLevel(receiver, [entry], playback);
  expect(level?.source).toBe('playback');
  expect(level?.db).toBeCloseTo(20 * Math.log10(.125), 12);
  expect(level?.peakDb).toBeCloseTo(20 * Math.log10(.25), 12);
});

it('holds peaks for a second, releases by elapsed time and resets between streams', () => {
  const meter = new ReceiverMeter();
  const loud = { db: -12, peakDb: 0, source: 'playback' as const };
  const quiet = { db: -48, peakDb: -40, source: 'playback' as const };
  expect(meter.update(loud, 'a', 0)).toEqual({ level: -12, peak: 0, clipped: true });
  expect(meter.update(quiet, 'a', 500)).toEqual({ level: -21, peak: 0, clipped: true });
  const released = meter.update(quiet, 'a', 1100);
  expect(released.level).toBeCloseTo(-31.8); expect(released.peak).toBeCloseTo(-2.4); expect(released.clipped).toBe(false);
  expect(meter.update(null, 'a', 1150)).toEqual({ level: -60, peak: -60, clipped: false });
  expect(meter.update(quiet, 'b', 1200)).toEqual({ level: -48, peak: -40, clipped: false });
});
