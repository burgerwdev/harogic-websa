import { afterEach, describe, expect, it } from 'vitest';
import { channelState } from '../core/channelState';
import { CHANNEL_COLORS } from '../core/channelColors';
import { confirmSdrChannels, confirmSdrReceiverNumbers, setCurrentSdrChannel, sdrChannelMarks } from '../ui/sdrChannels';
import { addCwText, clearCwLog, cwLog, setCwChannel } from '../sdr/cwLog';
import { addFt8Spot, clearFt8Spots, ft8Spots, setFt8Channel } from '../sdr/ft8Log';

afterEach(() => {
  for (let channel = 1; channel <= 8; channel++) { clearCwLog(channel); clearFt8Spots(channel); }
  setCwChannel(1); setFt8Channel(1); setCurrentSdrChannel(1); confirmSdrReceiverNumbers(null); confirmSdrChannels([]);
});
describe('eight receiver contexts', () => {
  it('allocates sparse contexts on consumption and keeps their data independent', () => {
    let count = 0;
    const state = channelState(() => { count++; return { values: [] as number[] }; });
    expect(count).toBe(0);
    state[8].values.push(88);
    state[3].values.push(33);
    expect(state[8].values).toEqual([88]); expect(state[3].values).toEqual([33]);
    expect(count).toBe(2);
  });
  it('retains stopped and IQ-only definitions in the actual selection set', () => {
    confirmSdrChannels(Array.from({ length: 8 }, (_, index) => ({ channel: index + 1, freq: 100e6 + index * 20000,
      mode: 'am', if_bw: 6000, enabled: index < 6, processing_enabled: index !== 5 })));
    confirmSdrReceiverNumbers([1, 2, 3, 4, 5, 6, 7, 8]);
    expect(sdrChannelMarks().map(row => row.channel)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
    confirmSdrReceiverNumbers([1, 2, 4, 5, 6, 7, 8]);
    expect(sdrChannelMarks().map(row => row.channel)).toEqual([1, 2, 4, 5, 6, 7, 8]);
    expect(new Set(CHANNEL_COLORS).size).toBe(8);
  });
  it('keeps the eighth Morse and weak-signal log separate from the first', () => {
    addCwText('EIGHT', true, 1, 8); addCwText('ONE', true, 1, 1);
    setCwChannel(8); expect(cwLog().lines[0].text).toBe('EIGHT');
    setCwChannel(1); expect(cwLog().lines[0].text).toBe('ONE');
    addFt8Spot({ text: 'CQ EIGHT', centerHz: 100e6, frequencyHz: 1200 } as never, 1, 8);
    setFt8Channel(8); expect(ft8Spots()[0].text).toBe('CQ EIGHT');
    setFt8Channel(1); expect(ft8Spots()).toEqual([]);
  });
});
