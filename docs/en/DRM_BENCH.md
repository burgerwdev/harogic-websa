# DRM bench loop (PlutoSDR → SAN-90) — measured record

This page records the bench that tests the DRM receiver on a live signal. It gives the
commands, the measured numbers, and the open defects. Use it to reproduce a failure
without a shortwave broadcast.

## Why a bench loop

A lab cannot receive a shortwave DRM broadcast. Shortwave propagation changes by the hour.
A bench loop replaces the propagation path with a cable or a short distance. The signal is
then repeatable, and its content is known.

## Bench setup

The transmitter and the receiver are 2 m apart. The PlutoSDR transmits. The SAN-90 receives
through its external antenna.

- The AD9363 transmitter cannot tune HF. It tunes from about 325 MHz. This is not a limit:
  a DRM signal is an OFDM waveform, so the carrier frequency does not matter to the
  decoder. Transmit at 400 MHz.
- The DRM baseband sits at `+100 kHz` from the transmitter's local oscillator. This keeps
  the direct-conversion leakage away from the signal. Tune the analyzer to the local
  oscillator plus 100 kHz.
- Analyzer settings: SDR mode, center 400.1 MHz, IQ capture bandwidth 195 kHz
  (decimate 256), ref level -40 dBm.

## Generate and transmit the DRM signal

Run these commands in order.

```
# 1. Build the DRM IQ file (in the DecDRM checkout).
decdrm tx tools/drm_bench/station_iq.toml --output ~/drm-bench/drm_iq_15s.wav --duration 15

# 2. Transmit it from the PlutoSDR.
python3 tools/pluto_drm_tx.py --iq ~/drm-bench/drm_iq_15s.wav --lo 400e6 --gain -20 --seconds 600
```

Two notes from the bench session:

- Do not transmit a 60 s file. That file makes a 250 MB cyclic buffer. The Pluto rejects
  the buffer push with `OSError: [Errno 14] Bad address`, and the radio then stays silent.
  A 15 s file (63 MB) works.
- Check the panadapter before you debug the receiver. A dead transmitter and a broken
  receiver look the same in the DRM readout. The signal appears as a plateau about 10 kHz
  wide at 400.1 MHz. Its peak was -110 dBm at 2 m in this session.

## Capture the baseband the browser receives

The DRM receiver runs in the browser. It reads the channelized baseband from the `?iq=1`
WebSocket. The capture tool subscribes to the same stream, so the file is the decoder's
exact input.

```
python3 tools/drm_capture.py --seconds 10 \
    --out tests/fixtures/drm/drm_live_modeB_so3_48828.f32 --json /tmp/drm_live.json
```

The tool drains 1 s before it counts. A capture that starts at once counts the frames the
backend queued while the socket opened. That burst inflates the sample count by about
1.8x, and the rate then looks wrong.

## Measured facts (2026-10-03, one session)

| Item | Value | How it was measured |
| --- | --- | --- |
| Baseband rate in the frame header | 48828.125 Hz | the `rate` field of the IQBF frame |
| True baseband rate | 48833.85 Hz | cyclic-prefix period: 1041.79 samples per 1024-sample useful part |
| Frame header rate accuracy | 0.012 % low | the two rows above |
| Level | rms +15.6 dBFS, peak +29.2 dBFS | full scale is 1.0, so the level is about 30x full scale |
| Occupied bandwidth | 9.82 kHz | at the published rate, so the signal is DRM mode B / 10 kHz |
| Delivery | 50.1k samples/s in frames of 66.5 ms | 40 frames recorded with arrival times |
| Content duplication | none | no frame is identical to its neighbour |

## Oracle check: Dream decodes the same samples

The other worktree (`feature/drm-dream-decoder`) runs the Dream decoder. Dream is the
oracle for this bench: it answers one question. Is the bench signal decodable at all?

```
python3 tools/drm_oracle_check.py tests/fixtures/drm/drm_live_modeB_so3_48828.f32 48828.125
```

The result:

```
messages=25  snr=20.9 dB  robustness=1  bandwidth=10.0 kHz
status={'io': 0, 'time': 0, 'frame': 0, 'fac': 0, 'sdc': 0, 'msc': 0}
service: label='SAN90 DRM BENCH' bitrate=20.96 audio_mode=Mono protection=EEP
```

Dream decodes the metadata and every channel. The bench signal is therefore good. The
failing part is the receiver in this branch.

## Receiver behaviour on the captured baseband

`cargo test --release --test drm_live_fixture` runs the checks below.

| Input | Result |
| --- | --- |
| Capture as delivered (48828.125 Hz samples into the 48 kHz core) | no lock, `timesync::acquire` returns `None` |
| Capture resampled to 48 kHz | lock, mode B, occupancy SO3, FAC SNR -18.9 dB, 19 of 19 FAC blocks fail their CRC, no station label, no MSC frame |
| `tests/fixtures/drm/drm_modeB_so3_48k.f32` (synthesised reference) | FAC SNR 36.6 dB, 0 FAC errors, 5 SDC blocks pass, label decoded |
| Reference + white noise at 15 dB SNR | decodes, FAC SNR 26.2 dB |
| Reference + one echo at 200 us and -6 dB | decodes, FAC SNR 21.7 dB |
| Reference at 400x its level | decodes, FAC SNR 36.6 dB |

The last three rows rule out level, noise and multipath as the cause. The live signal has
about 21 dB SNR, and the reference decodes at 15 dB.

## Factors already ruled out

Each factor below was tested against the captured live file. None of them explains the
failure.

- **Level and scaling**: the reference fixture decodes at 400x its own level, and the live
  capture gives the same result at 1/256 of its level.
- **Carrier offset**: a scan from -400 Hz to +400 Hz in 20 Hz steps. No offset decodes the
  FAC. The cyclic-prefix phase gives an offset of about 19 Hz after resampling.
- **Frame offset**: every candidate offset 0..14 was forced. No candidate decodes the FAC.
- **Spectrum sense**: the conjugate of the capture behaves the same.
- **DC offset**: the capture's mean is 0.002, and removing it changes nothing.
- **Sample rate**: the measured rate is 48833.85 Hz, and a resample at that exact rate
  changes nothing.

## Open defects

1. **Rate**: the DRM core is fixed at 48 kHz (`wasm/src/digital/drm/params.rs`). The
   channelizer delivers 48828.125 Hz. Without a resample the receiver cannot acquire.
   The pipeline has a resampler, and the worker must give it the live rate.
2. **FAC decode on a live signal**: not explained yet. Every FAC block fails, and the
   factors above do not explain it. This is the blocking defect.
3. **Super-frame phase**: `DrmReceiver::decode` assumes the buffer starts at symbol 0 of a
   super frame. A live capture starts anywhere. The SDC and the MSC therefore use the
   wrong cells.
4. **Super-frame assertion**: a partial super frame makes the SDC cell count assert
   (`wasm/src/digital/drm/fec/mlc.rs:335`). An assertion inside wasm can trap the module
   and break every later mode in the session.
5. **Blind readout**: the decode window shows nothing until a station label decodes. The
   operator cannot see the lock state or the FAC SNR without a label, so a lock looks like
   a dead receiver.

## Mode-switch regression: first observation

The report is: after DRM, another demodulator gives no audio until a page refresh, and a
preset does not help. The first bench attempt did not reproduce it. Record the attempt here,
because the conditions matter.

Steps used (Chromium, a fresh page, service on port 8080):

1. Set the mode to SDR with `SET_MODE`.
2. Load the page and switch the SDR audio switch on.
3. Select DRM in the demodulator group and open the decode window. Wait 12 s.
4. Select AM and wait 8 s.

Observed: the DRM readout stayed empty (no lock at that sample rate), and AM produced audio
again at once (`dsp_pcm_blocks` 0 to 241, `dsp_pcm_rms` 0.05..0.08, `dsp_ratio` 1.0005).
The switch worked.

Two conditions still need a test, and both are likely to matter:

- **A DRM lock in the browser.** At this rate the DRM core never reaches its decode path.
  An assertion on that path (`wasm/src/digital/drm/fec/mlc.rs:335`) can trap the wasm
  module. A trapped module breaks every later mode in the session, which fits the report
  exactly. Fix the rate first, then retry this sequence.
- **No audio-switch toggle before the first DRM selection.** The steps above switch the
  audio on early. Repeat the sequence without that step, and with a preset switch instead
  of a demodulator click.

## Reproduce the session

```
# 1. Start the service and confirm the device.
./status.sh

# 2. Transmit the DRM signal (see above) and confirm the signal in the panadapter.

# 3. Tune the analyzer to the bench signal.
#    SDR mode, center 400.1 MHz, decimate 256, listen 400.1 MHz, demod DRM, ref -40 dBm.

# 4. Capture the baseband, then decode it offline.
python3 tools/drm_capture.py --seconds 10 --out /tmp/live.f32
cd wasm && cargo test --release --test drm_live_fixture -- --nocapture
```
