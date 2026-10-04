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
| Capture resampled to 48 kHz | lock, mode B, occupancy SO3, FAC SNR -18.9 dB, 14 of 14 FAC blocks fail their CRC, no station label, no MSC frame |
| Capture resampled, carrier offset removed | FAC SNR -13.6 dB, and the FAC still fails |
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
- **Carrier offset**: the receiver now removes it. See the section below.
- **Frame offset**: every candidate offset 0..14 was forced, with the offset removed. No
  candidate decodes the FAC on the live capture.
- **Whole-carrier anchor**: every shift from -6 to +6 carrier spacings, with the fractional
  offset removed. No shift decodes the FAC.
- **Resampler quality and rate accuracy**: a windowed-sinc resample at the published rate and
  at the measured rate behave like the pipeline's linear resampler, and a double resample
  changes nothing.
- **Clipping**: the receiver decodes the fixture after hard clipping at 1.0 times its rms
  level, so the loud capture is not the cause.
- **Spectrum sense**: the conjugate of the capture behaves the same.
- **DC offset**: the capture's mean is 0.002, and removing it changes nothing.
- **Sample rate**: the measured rate is 48833.85 Hz, and a resample at that exact rate
  changes nothing.

## Carrier offset: the FAC failure explained

The receiver corrects no carrier offset. It assumes that the signal sits exactly on the DRM
carrier grid. A real signal does not.

Measured on the live capture:

- The carrier offset is +19.7 Hz. The cyclic-prefix phase gives this value, and it stays
  within 0.8 Hz over the capture. The same method gives 0.0 Hz on the synthesised fixture.
- +19.7 Hz is 42 % of the DRM carrier spacing (46.875 Hz). Every carrier then falls between
  two FFT bins.
- The DRM core tolerates only a few Hz. The table below applies an offset to the synthesised
  fixture and runs the receiver.

| Offset applied to the fixture | FAC SNR | FAC result |
| --- | --- | --- |
| 0 Hz | 36.6 dB | 0 of 15 blocks fail |
| +5 Hz | 12.6 dB | 0 of 15 blocks fail |
| +10 Hz | 6.3 dB | 0 of 15 blocks fail, the SDC fails |
| +20 Hz | -1.7 dB | 15 of 15 blocks fail |
| +23.4 Hz (half the carrier spacing) | -14.4 dB | 14 of 15 blocks fail |

The live capture behaves like the fixture at about +20 Hz. The FAC failure is therefore a
carrier-offset failure, and not a channel, level or noise problem.

The receiver now estimates the offset from the guard correlation and removes it before the
FFT. `timesync::acquire` reports the value in `Acquired::freq_offset_hz`, and
`DrmReceiver::remove_carrier_offset` applies it. The estimate is unambiguous up to
`+/- fs/(2*nu)`, about +/-23 Hz at 48 kHz.

The fix is verified on the fixture: an injected offset of -19.7, -10, -5, +5, +10 and
+19.7 Hz now decodes with no FAC error and the correct station label
(`wasm/tests/drm_phy_robustness.rs::removes_a_carrier_offset_before_the_fft`). Before the fix
the same offsets failed from about +20 Hz.

The live capture still fails the FAC after the correction (-13.6 dB). The carrier offset was
therefore necessary, but it is not the only cause. The remaining defect is in the cell
extraction of a real signal, and the ruled-out list below records what it is not.

## Open defects

1. **Rate**: the DRM core is fixed at 48 kHz (`wasm/src/digital/drm/params.rs`). The
   channelizer delivers 48828.125 Hz. Without a resample the receiver cannot acquire.
   The pipeline has a resampler, and the worker must give it the live rate.
2. **Carrier anchor and super-frame phase**: fixed. The receiver resolves the whole-carrier
   part of the offset from the detected band edges and the fraction from the guard
   correlation, and it tries each super-frame phase and keeps the one whose SDC passes its
   CRC. The live capture now locks, decodes the FAC and the SDC, shows the station, its
   robustness mode, its bandwidth, its bit rate, its codec and its FAC SNR, and passes the
   MSC CRC. `wasm/tests/drm_live_fixture.rs` covers all of it.
3. **Audio decode of a real stream**: RESOLVED (2026-10-04). The bench stream — the
   standard DRM HE-AAC configuration, a 12 kHz core with SBR, 208-byte access units,
   five per 400 ms frame — decodes to non-silent 24 kHz PCM in the wasm module, both
   from a whole-buffer push (76 800 samples) and from the worker's 3 248-sample block
   feed (48 000 samples). Three root causes, all fixed:

   - **The aligned allocator did not zero its memory.** FDK's own `genericStds` backs
     `FDKaalloc`/`FDKaalloc_L` with `FDKcalloc` — "malloc and clear" — because the
     persistent channel info it allocates is read before it is fully written (the HCR
     side-info sort reads it first). Our shim used plain `malloc`, so the memory held
     whatever the heap had there. On a fresh wasm heap that is zeros and the decode
     passes; on a recycled heap it is garbage, and the HCR decoder turns it into an
     out-of-bounds index that wasm's exact bounds check turns into the trap. That is
     the whole "depends on the heap layout" story. The proof came from a native build
     of the same FDK sources with MemorySanitizer: `use-of-uninitialized-value` at
     `aacdec_hcr.cpp:782` in `HcrSortCodebookAndNumCodewordInSection`, memory created
     by `FDKaalloc_L` in `CAacDecoder_Init`. The shim now mirrors FDK: `calloc`, align
     up, stash the raw pointer for `FDKafree`.
   - **The SBR flag is no longer stripped.** With the allocator fixed, FDK decodes the
     real SBR payload (bit-reversed at the end of each access unit, per DRM syntax) and
     outputs at the SBR rate. The readout reports the SBR rate too (core x 2), which
     moved the synthesised fixture's expected rate from 24 000 to 48 000 Hz — its SDC
     claims SBR over a 24 kHz core, though its payload carries none.
   - **Two streaming defects kept the post-lock passes silent** (both invisible to a
     whole-buffer push): the MSC `CellDeinterleaver` was recreated per pass, so the
     long interleaver's five-frame fill consumed the whole 3 s window before any frame
     came out — it now persists across passes with an absolute frame index that skips
     the overlap between windows; and `remove_carrier_offset` replaced `mix_w` instead
     of accumulating it, so after the whole-carrier anchor correction every new sample
     was under-rotated by the first correction (the ~120 Hz fraction) and the pass SNR
     fell from 20 dB to 1.7 dB frame by frame. A whole-buffer push never saw it,
     because the lock pass rotates the entire buffer in place and nothing arrives after
     the lock.

   The decisive cause was a fourth one, found by diffing the access units against the
   reference receiver's byte for byte: **the DRM text message was left inside the audio
   super frame.** When the SDC audio descriptor sets its text flag (ours does), the LAST
   FOUR BYTES of each audio logical frame are the text message, not audio. Our last
   access unit swallowed them, which shifted FDK's SBR payload — read BACKWARDS from the
   frame end — by four bytes; FDK then filled the missing SBR with deterministic noise.
   That noise is the hiss a listener heard, and it is why a core-only decode (SBR flag
   cleared) played a clean tone while the full HE-AAC decode did not. Our access units
   now match the reference byte for byte (34/35 aligned samples identical) and the
   decoded audio is the bench transmitter's 1 kHz tone (top frequency 1000.0 Hz,
   high-band energy fraction 0.002). Ported from the reference: Dream's
   `AACSuperFrame`/`CDataDecoder` and DecDRM's `split_text_message` both strip those four
   bytes before deframing; `wasm/src/digital/drm/audio.rs::split_text_message` does now.

   One more delivery defect: the worker's audio read was a capped head window plus an
   offset, so once the receiver's buffer outgrew the cap (about 1.4 s of audio) it
   delivered nothing more. The ABI now DRAINS: `websa_dsp_drm_audio_pcm` returns what
   accumulated since the previous call, the worker forwards all of it, and the receiver's
   buffer stays bounded.

   Regression gates: `live_capture_streams_audio_after_the_lock` (native, block feed),
   `audio::tests::text_message_is_not_part_of_the_audio_super_frame` (the four bytes), and
   `frontend/src/__tests__/drmLiveAudio.test.ts` (wasm: non-silent 24 kHz PCM whose peak
   is at 1 kHz). The byte-level cross-check is reusable: `wasm/examples/dump_drm_au.rs`
   writes the access units, and DecDRM's `crates/decdrm-codecs/examples/decode_au_dump.rs`
   decodes them.
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

The cause was found later: the wasm DRM audio decode traps on the bench stream, and a trap
kills the worker instance. The page then held a reference to a dead worker, so every later
mode change went nowhere and only a page refresh helped. `worker.onerror` now drops that
worker, and the next mode change starts a fresh one
(`frontend/src/__tests__/workerRecovery.test.ts`; it failed before the change).

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

# 5. Or decode it through the wasm path exactly as the worker does (non-silent PCM).
cd frontend && node --experimental-strip-types scripts/drm_live_audio.mjs /tmp/live.f32

# 6. Browser end-to-end: DRM from the panel, the readout, away to AM and back —
#    no page refresh (Firefox on purpose; headless Chromium crashes here).
python3 tools/e2e/drm_switch.py --url http://127.0.0.1:8080
```

## Verified live (2026-10-04): audio from the bench loop

The session that fixed the audio defects ended with the whole chain green on the bench,
transmitting `drm_iq_15s.wav` at TX gain -5 dB, the app at ref -40 dBm:

- A 12 s capture decoded natively to 35 access units and 22 FAC blocks (au=35, facs=22);
  after the text-message fix the same path reaches au=70 with the FAC error count at zero.
- The same capture through the wasm block-fed path (`scripts/drm_live_audio.mjs`) produced
  230 400 non-silent 24 kHz PCM samples whose dominant frequency is the bench tone:
  1000.0 Hz, with 3 % of the energy above 5 kHz (before the fix: none of the samples were
  a tone and the band was mostly noise).
- The reference receiver (Dream's console build) decodes the same capture's audio from the
  transmitted signal, and DecDRM's receiver writes a clean 1 kHz tone from it; the byte
  comparison against both is the tool that found the text-message defect.
- `tools/e2e/drm_switch.py`: DRM locks in the browser (readout: `locked: B, 10 kHz ...`
  `station: SAN90 DRM BENCH`, `FAC SNR 12 dB`), a switch to AM keeps the baseband running,
  and DRM locks again — all without a page refresh.

Two bench facts this session added to the list above:

- **The DDC's digital gain differs per service instance.** One start delivered the capture
  at rms +34 dBFS (clipped, locks in ~1.4 s everywhere); the next, at the same TX gain,
  delivered -14 dBFS and nothing locked — native, wasm and browser alike. Restarting the
  service restored the hot level. So: if FACs refuse to decode on the bench, re-capture and
  check `rms_dbfs` in the capture JSON before blaming the receiver.
- **With no lock, the worker can starve the browser stream.** A weak-signal page froze the
  browser's baseband at ~30 blocks (the receiver re-runs acquisition over the whole buffer
  on every block). With a locking signal the same page runs indefinitely. Keep this in mind
  when reading a frozen `blocks=` counter in the IQ diagnostics.
