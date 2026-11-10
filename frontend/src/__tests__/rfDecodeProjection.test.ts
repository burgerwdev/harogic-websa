import { expect, it } from 'vitest';
import { DecodeSequence, type DecodeEnvelope } from '../sdr/decodeProtocol';
import { claimNativeDecode, publishNativeDecode } from '../sdr/decodeOwnership';
import { decoderLog, forgetDecoderLog } from '../sdr/decoderLog';
import { ft8Spots, clearFt8Spots } from '../sdr/ft8Log';
import { sdrIqStats, setSdrIqEnabled } from '../sdr/iqStream';
import { consumeReceiverResult } from '../ui/nativeWorkbenchStreams';

function frame(seq: number, event: DecodeEnvelope['event']): ArrayBuffer {
  const envelope: DecodeEnvelope = { version: 1, channel: 1, channel_id: 17, epoch: 9, seq,
    input_index: '145429679', input_rate: 3906250, input_fraction_num: '0', input_fraction_den: '1',
    delay_seconds: .005033, delay_known: true, causal_presentation: false, mode: 'ft8', event };
  const text = new TextEncoder().encode(JSON.stringify(envelope));
  const buffer = new ArrayBuffer(8 + text.length);
  new Uint8Array(buffer).set([83, 68, 69, 50]);
  new DataView(buffer).setUint32(4, text.length, true);
  new Uint8Array(buffer, 8).set(text);
  return buffer;
}

it.each([-7, null])('projects RF telemetry (%s) and subsequent FT8 events to the main panel without failing decoding', value => {
  document.body.innerHTML = '<div id="sdr-receiver-error" hidden></div><div id="decode-source-timing"></div>';
  const uid = 'rf-projection';
  const release = claimNativeDecode();
  const sequence = new DecodeSequence();
  clearFt8Spots(1);
  try {
    setSdrIqEnabled(true);
    const before = sdrIqStats().blocks;
    const envelopes = [
      frame(0, { type: 'start' }),
      frame(1, { type: 'rf-measurement', measurement: { metric_type: 'rf_snr', unit: 'dB', value,
        state: value === null ? 'unavailable' : 'conditional', reason: value === null ? 'occupied_or_contaminated_noise_reference' : null,
        reference_bandwidth_hz: 3000, averaging_seconds: 1.5, location: 'channel_iq_before_demod_agc_squelch_nr_eq', method: 'welch' } }),
      frame(2, { type: 'ft8-state', bufferedSamples: 12000, bufferedSeconds: 1 }),
      frame(3, { type: 'ft8', text: 'CQ W1ABC FN42', centerHz: 14074000, frequencyHz: 1000, timeOffsetS: .5, snrDb: -12 }),
    ].map(data => sequence.accept(data));
    for (const envelope of envelopes) consumeReceiverResult(uid, envelope);
    publishNativeDecode(envelopes);
    expect(sdrIqStats()).toMatchObject({ lastError: '', pipelineReady: true, blocks: before + 4 });
    expect(ft8Spots()[0]).toMatchObject({ text: 'CQ W1ABC FN42', snrDb: -12 });
    expect(document.getElementById('sdr-receiver-error')?.hidden).toBe(true);
    expect(document.getElementById('decode-source-timing')?.textContent).toBe('');
    expect(decoderLog(uid).entries.find(entry => (entry.data as { type?: string }).type === 'rf-measurement')?.data)
      .toMatchObject({ measurement: { value } });
  } finally {
    setSdrIqEnabled(false); release(); forgetDecoderLog(uid); clearFt8Spots(1);
    document.body.innerHTML = '';
  }
});
