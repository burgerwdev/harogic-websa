# DRM demodulator — handoff: state, evidence, next steps

This page is for whoever continues the work, including a later session with a different model.
`docs/en/DRM_BENCH.md` holds the measurements; this page holds the state and the plan.

## Where the work stands
The receiver is the ported chain in `wasm/src/digital/drm/` (following Dream's stage order);
the previous receiver is deleted, so `digital::drm` is the only DRM demodulator.

Working, verified by tests and by the bench:

- **The receiver decodes the committed bench capture end to end.** Fed in the worker's
  3248-sample blocks it locks, reads `SAN90 DRM BENCH`, decodes FAC 13 blocks / 0 CRC errors,
  8 MSC multiplex frames and deframes 40 HE-AAC access units; the wasm path produces 76 800
  non-silent 24 kHz samples carrying the bench 1 kHz tone (the `drmLiveAudio` gate). The same
  decode runs natively in `wasm/tests/drm_receiver.rs`.
- **Streaming-correct.** The block-fed decode matches the one-shot whole-buffer decode on the
  committed capture — frame phase, FAC blocks, station label, MSC frames and audio access
  units all agree. The earlier wasm-vs-native gap (0 FAC blocks in wasm) was the streaming path
  committing the frame phase too early and assembling the MSC super frames with the batch-only
  frame-index lookup; both are fixed.
- **Channel estimation is the exact linear time interpolation**, the reference's default until
  its timing/SRO loop is closed. The Doppler-adapted time-Wiener is ported and switchable but
  is not used by default: on the clean bench capture its (correct) channel still lets the
  frequency Wiener over-smooth the residual timing ramp, which corrupts the 64-QAM MSC.
- **The readout** shows the station, mode, bandwidth, the FAC/MSC/audio counts and the
  estimated SNR; a retune resets the digital demodulator, so the previous channel's label and
  metadata do not leak into the next.
- **Suites green:** `cargo test` (lib + `tests/drm_receiver.rs`), the frontend DRM wasm tests
  (`drmLiveAudio`, `drmAudio`, `drmXaac`, `drmAudioEndToEnd`) and `make ci`.

Open:

- **Bench loop, this session.** The Pluto transmitted `drm_iq_15s.wav` at 400.1 MHz and the
  SAN-90 fed the browser's baseband. Fresh 6 s captures at ref -42 / -46 / -48 dBm decode end to
  end through the wasm receiver: `SAN90 DRM BENCH`, FAC 13-14 blocks / 0 errors, 8 MSC frames,
  40 audio access units, and 76 800 non-silent 24 kHz PCM samples whose spectral peak is exactly
  the bench 1 kHz tone (99.7 % of the energy in 900-1100 Hz). The level window is narrow: ref -40
  and ref -50 dBm do not decode (the handoff's "hot level" quirk), which is why the committed
  capture — taken at the same bench — is the reproducible acceptance.
- The browser e2e (`tools/e2e/drm_switch.py`) is blocked in this session by the baseband
  delivery to the browser: the DSP worker only receives about one 3248-sample block per second
  (the backend logs `acquisition step took 65 ms`), so the DRM readout never fills even though
  the same wasm receiver decodes the identical capture offline in 0.5 s. The audio acceptance
  therefore runs through the `drmLiveAudio` wasm gate (76 800 non-silent samples).
- **Real shortwave.** Four HF frequencies were captured (9755, 11620, 5875, 3955 kHz, 6 s each,
  ref -40 dBm); none carried a DRM signal (no lock). Reception depends on propagation and a
  broadcast being on air.
- xHE-AAC (coding 3) decodes end to end on the committed bench fixture
  `drm_live_xhe_modeB_so3_48828.f32`. The stateful super-frame deframer recovers the USAC
  access units (50, against the reference's 51) and they go through FDK's `TT_DRM` decoder —
  the reference receiver's own path (`open_decoder(DrmAudioCoding::XheAac)`); libxaac is the
  encoder only, and its mp4-mode decoder rejects the compact DRM Static Config, which is why
  that path was dropped. The `drmCodecs` wasm test drives the receiver through the shipped ABI
  and produces 102 400 non-silent 24 kHz PCM samples with the 1 kHz tone (99.6 % of the energy
  in 900-1100 Hz).
- HE-AAC v2 (AAC + SBR + parametric stereo) is verified on the live bench: the Pluto
  transmitted a DecDRM-generated `heaacv2` stream (12 kHz core) and the fresh capture
  decodes to 153 600 stereo 24 kHz PCM samples, both channels a 1 kHz tone (99.7 % of the
  energy in 900-1100 Hz, L/R correlation 1.0), 40 audio AUs.
- **All four DRM30 modes decode end to end.** Committed fixtures from real DecDRM
  transmissions (HE-AAC mono, 12 kHz core, mode A/C/D, SO3) join mode B; the MSC assembly is
  now mode-generic (it used mode B's 15 symbols/frame and 45 buckets, which broke modes C and
  D) and each mode locks, decodes the FAC with no CRC error and deframes 55 audio AUs.
- **Mode E / DRM+ is now implemented through the same chain.** The current branch has a separate
  `drmplus` plugin entry: 96 kHz/100 kHz channelizer input, mode-E FAC (116-bit, 1/4 code, two
  service sets and four identity/toggle positions), mode-E SDC/MSC coding and depth-6 interleaving,
  AFS-aware cell mapping, paired 200 ms audio logical frames, and AAC/xHE audio handoff. The
  deterministic `drm_modeE_so0_96k_aac.f32` fixture decodes through native FAC/SDC/MSC/AU checks
  and the shipped wasm ABI at both 48 kHz and 44.1 kHz output. ±100 Hz injected carrier offsets
  also pass. This is synthetic-signal evidence; no real VHF DRM+ recording or transmitter is
  available here, so real propagation and reconfiguration coverage remain open.
- The timing/SRO loop is now closed: the impulse-response tracker's `timing_adjust` and
  `sro_delta_hz` are fed back into the `TimeSync` window (`adjust_timing`/`adjust_sro`) once
  tracking is enabled, so the FFT window stays aligned and the channel no longer carries a
  residual timing ramp. The Doppler-adapted time-Wiener is ported and switchable but stays
  off by default; with the loop closed it should now be re-measured on the bench capture and
  enabled if it improves the MSC decode.

## 2026-10-05: the live deficit was the missing time-domain frequency tracking

`/tmp/live30.f32` was independently verified this session: converting it to a stereo WAV
(I left / Q right, 48 kHz) and running `decdrm rx --format iq --no-auto-flip` gives
**FAC ok 64 bad 9 · MSC 71 (ok 40) · 200 audio frames (115 concealed) · MER 17.8 dB**,
station `SAN90 DRM BENCH`, delay 0.8 ms, Doppler ~1.4 Hz, SRO −0.14 Hz. The sample is
therefore trustworthy — the overdriven RMS (64, vs the clean fixture's 0.25) is the
DDC digital-gain quirk `DRM_BENCH.md` already documents, not a defect: the reference
locks at exactly this level. Normalising the amplitude does not help, because the
channel/propagation is what defeats us, not the level.

The root cause, measured directly on the demodulated pilots:

- The residual carrier offset (after the one-shot coarse correction) **drifts slowly over
  seconds** between about −8 and +13 Hz (mean +2.4 Hz). A one-shot fine correction removes
  only the mean; the drifting residual (from the sample-rate offset) keeps rotating the
  channel, which the 3-symbol time interpolation cannot follow, and its inter-carrier
  interference smears the impulse response. The reference tracks the offset continuously
  from the frequency pilots and applies it to the mixer in the time domain
  (`framesync.rs`'s `freq_delta_hz` + the per-symbol mixer update), which this port lacked.
- The symptom was a smeared impulse response: the tracker's delay-spread estimate read
  **103 IR bins (10.6 ms)** against the reference's 0.8 ms, so the frequency Wiener got a
  ~5× too-wide length and destroyed the equalisation (the old limit-cycle diagnosis was a
  red herring; the spread is not real).

**The fix, landed this session** (commit `f7eab285` + follow-up): a `sync::freqtrack` stage
(port of DecDRM's `freq_delta_hz` + coarse `sro_estimate`) and a streaming NCO that is
re-tuned every symbol by it (`Nco::set_offset`), wired into the chanest harness's `run()` via
`rows_and_syms_tracked`. This removes the drifting offset **in the time domain**, before the
FFT. Post-FFT rotation of the cells cannot do this — it removes only the per-symbol phase, not
the inter-carrier interference the offset already baked into the demodulated cells (that path
reads 0.6–3 dB and still smears the IR). With the time-domain tracking the live FAC MER is
**16.3 dB** (reference 17.8, within the ±2 dB band) and the delay-spread estimate is **0.62 ms**
(reference 0.8 ms), deterministically, and the limit cycle is gone. The clean fixture still
reads 42.8 dB.

What remains for the live loop: the SRO/timing closed loop (the tracker's `sro_delta_hz` and
`timing_adjust` are computed but not yet applied in `run()`), which can only start once a FAC
decodes; and the same streaming frequency tracking needs wiring into `DrmReceiver` (which still
does one-shot acquisition and no mixer re-tuning). The live FAC count through the tracked chain
is **64 good / 10 bad** (the reference's 64 ok / 9 bad): the good count matches exactly and the
one extra bad block is the same marginal block the reference's 1.5 dB higher MER tips over.
Feeding the tracker's `timing_adjust`/`sro_delta_hz` back still destabilises the loop (−0.4 dB),
so the window is left on the guard-correlation acquisition for now.

**Task-5 measured 2026-10-05**: the live SDC decodes through the tracked chain to **21 ok blocks**
(matching the reference's 21 ok exactly), every label `SAN90 DRM BENCH`, the audio descriptor
HE-AAC (coding 0, SBR, mono, 12 kHz core, text) and the multiplex EEP 0/1 with one stream — all
as the reference reports. The fixture FAC/SDC tests (`fac_decodes_to_the_fixture_manifest`,
`sdc_decodes_to_the_fixture_manifest`) already pin the manifest values.

**Task-6/7 measured 2026-10-05 (second session)**: the receiver's MSC assembly bug is fixed —
two defects, both now regressed by tests: `run()` re-demodulated after mode detection and could
differ by a border-case window (it now reuses the demodulated rows, the chanest harness's path),
and `decode_msc` did not flush the final partial super frame. The clean-fixture receiver test
now pins the 6 MSC frames bit-exact against the xorshift stream, and the AAC-fixture test pins
30 deframed access units (3 complete super frames × 10). The MSC demux and AAC super-frame
parsing live in `drm2::audio`, and the receiver's `deframe_audio`/`decode_audio` (FDK TT_DRM for
AAC, libxaac for xHE) are wired; the wasm ABI that surfaces `websa_dsp_drm_audio_pcm` still
runs the previous receiver, which is task-9's swap. The live30 audio path through the tracked
chanest harness demuxes **48 valid super frames** (≥ the reference's 40 ok).

**The receiver decodes live30 end to end (2026-10-05)**: with the MSC assembly fixed and the
streaming frequency tracking ported into `DrmReceiver::run()` (coarse `freqacq`, streaming NCO
re-tuned every symbol by the tracker), the full capture through the receiver reads FAC
**64 ok / 10 bad**, station `SAN90 DRM BENCH`, HE-AAC mono 12 kHz and **240 deframed audio
access units** — no amplitude normalisation needed. The live acceptance test
(`receiver_on_live_capture`) pins all of it.

**The incremental processing model was attempted and reverted (2026-10-05)**: splitting `run()`
into a mode-detection-once pass plus an incremental demodulation (the NCO/FreqTrack state carried
over, only the new samples demodulated) broke the live30 decode — all 73 FAC blocks failed CRC
while the batch model reads 64 ok / 10 bad. The debug showed the frame phase computed correctly
(7) over 1125 tracked rows, so the defect is downstream of the phase (the incremental chanest
feed or the symbol indexing), not the frequency tracking. The batch model (one `run()` decodes
the whole buffered capture) is the committed state; the wasm block-fed bench needs this
incremental model because `run()` re-decodes the whole buffer on every 3248-sample push (89
full decodes for the 30 s capture), which is far too slow for wasm.

**The wasm ABI swap was attempted and reverted (2026-10-05)**: `drm2::DrPlugin` (a
`DigitalDemodulator` wrapper around the drm2 `DrmReceiver`, with `buffered()`/`snr_db()`/
`occupancy()` added for the readout) was swapped into `digital/mod.rs`, and the receiver's
`run()` was made incremental (the mode detection once, then only the new samples demodulated),
which fixed the block-fed bench's timeout. But in wasm the FAC decode fails entirely —
**FAC ok 0 / err ~74**, for BOTH channel-estimator paths — while the same code natively reads
64 ok / 10 bad on the same capture. The wasm-vs-native difference (both paths fail, so it is
not the time-Wiener) needs debugging; the dispatch was reverted to the previous receiver so
`make ci` stays green, and the drm2 receiver keeps its native end-to-end verification.

A constellation probe narrows it further: dumping `fac_constellation` through the existing
`websa_dsp_drm_constellation` export shows the drm2 FAC cells reading **~2.37x larger** than the
previous receiver's (mean |cell| 2.37 vs 1.007; the first cell 7.3). 2.37 = sqrt(PILOT_POWER) x
the channel gain, i.e. the *pilot* cell amplitude — suggesting `fac_constellation` (or the FAC
cell collection) reads pilot cells instead of FAC data cells in the wasm build. Both cellmaps
use the same PILOT_POWER conventions, so the difference is wasm-specific; needs a fresh session.

A native-vs-wasm FAC cell comparison (the same capture, the same receiver code) narrows it
further: the **native** FAC constellation (which decodes, FAC 64/10) has **4820 cells, mean
|cell| 1.54, first cells ~1.0 magnitude** — while the **wasm** constellation has mean |cell|
2.37 and a first cell of magnitude 7.3. The wasm FAC cells are systematically larger, i.e. the
wasm channel estimate is smaller than the native one for the same demodulated rows (the symbol
count matches at 1115). The largest gap is at the first emitted symbol (the time-Wiener warm-up
boundary), so the leading hypothesis is a wasm floating-point difference in the timing
acquisition or the Wiener filter build that shifts the channel estimate early in the stream.

Also learned: the FFT-window half-guard offset is **not** the deficit. Removing the `+ g/2` in
`TimeSync` (matching DecDRM's window) improves the clean-fixture MER to 47.9 dB but degrades the
frame-sync score (this port's `FrameSync::search` correlates time pilots directly, so it is
sensitive to the window phase ramp — DecDRM's correlates adjacent pairs and is not) and breaks
six clean-fixture tests, so it was reverted. Both window positions read ~1 dB on the live
capture. The frequency tracking is the real fix; the window position is a separate, later
question tied to porting DecDRM's adjacent-pair frame sync.

## Bench facts that took time to learn

- Ref level **-35 to -45 dBm**: in-band contrast is then 25 to 27 dB. At -40 dBm it was 6 dB and
  nothing locked. **Set it before every bench run**: presets and the backend's own reference
  adjustment overwrite it, and the operators on both sides of this goal have seen the signal
  disappear after a setting was restored.
- Transmit with `tools/pluto_drm_tx.py` and the 15 s file. A 60 s file makes a 250 MB cyclic
  buffer and the Pluto rejects the push with `EFAULT`.
- The bench stream carries the standard DRM HE-AAC configuration: a 12 kHz core with SBR, 208
  byte access units, five per 400 ms frame. The numbers add up against the 1048 byte stream
  frame, so the deframing matches the stream.
- The synthesised fixture claims SBR in its SDC but carries no SBR data (the "no-SBR smoke"
  case), so it exercises a different path from the bench stream.

## The audio defect, in order of discovery

1. The FDK wasm decoder trapped on the bench access units. Trap or not depended on the heap
   layout: the same artifact passed in some runs and failed in others.
2. `wasm/fdk/shim.cpp` ignored the alignment that `FDKaalloc(size, alignment)` asks for, so
   FDK's aligned SBR buffers were only 16-byte aligned. Repairing it removed the first fault and
   exposed a second one inside the decode, and the repair made the fixture trap. The repair is
   not landed; it needs its own investigation.
3. The access unit needs a buffer with room behind it. FDK looks for the SBR payload after the
   core frame, and a stream that claims SBR without carrying it made FDK read past the unit.
   Landed: `AesDecoder::decode` now copies the unit into a buffer with 512 zero bytes behind it,
   and the audio smoke test passed three times in a row.
4. The audio needs a decode pass after the lock, because the long interleaver fills over five
   frames (measured: a 3 s window yields 2 to 3 MSC frames and 5 access units).
5. Such a pass must carry the absolute row index, or the super-frame phase is wrong.

## Reference implementations to read

The two projects below are working DRM receivers. Their code is not a drop-in for this branch,
but their DRM and audio handling is worth reading before changing ours.

- The sibling worktree `/home/hui/git/harogic-websa-drm-dream`: the verified python path around
  the Dream binary. Read `web_sa/drm_dream/decoder.py`, `tools/drm_dream/probe_fixture.py`,
  `tools/drm_dream/loopback_probe.py` and `docs/en/DRM_DREAM.md`. It records what the DDC feed
  needed (the measured rate, the level) and what Dream reports.
- The DecDRM checkout `/home/hui/git/DecDRM`: the transmitter that generates the bench signal,
  with its receiver beside it. `crates/decdrm-codecs/src/aac/drm.rs` is the clearest reference
  for the DRM AAC access unit, including the SBR payload and the CRC byte.
- The FDK sources `/home/hui/git/fdk-aac`: `libMpegTPDec/src/tpdec_drm.cpp` shows what the DRM
  transport expects, and `libAACdec/include/aacdecoder_lib.h` lists the parameters and the
  stream info layout.

DecDRM's receiver modules map one to one onto the weak spots of our receiver, and they are in
the same language:

| DecDRM module | What it does | Ours |
| --- | --- | --- |
| `rx/freqacq.rs` | Coarse carrier acquisition from the three continuous frequency pilots (750, 2250, 3000 Hz) in a 6 x 1024 point FFT, searching the mirrored pattern too | The guard-correlation phase only sees the fraction, and the band edges only see whole carriers |
| `rx/timesync.rs` | Guard correlation on a signal low-passed to +/-4.5 kHz and decimated by 4, like Dream's path | Full-rate correlation, one static alignment |
| `rx/chanest/` (`time_wiener.rs`, `track.rs`) | Time-Wiener channel estimation with tracking | Per-symbol linear interpolation across scattered pilots |
| `rx/framesync.rs`, `rx/ofdm.rs`, `rx/mscdec.rs` | Frame sync, OFDM demod and the MSC decoder as streaming stages | One decode pass over a bounded buffer |

Read them for the algorithms; DecDRM is GPL-2.0-or-later, so no code is copied.

The goal keeps the Dream subprocess out of this branch. Read those projects for the algorithms,
not to add a second decoder.

**Next session plan**: port DecDRM/Dream's `freqacq.rs` (pilot-based coarse carrier
acquisition), `rx/chanest/` (time-Wiener channel estimation with tracking), and
`rx/timesync.rs` (low-pass + decimate guard correlation). These three address the
remaining failures: the carrier anchor, the channel estimation quality, and the
symbol timing on real signals. Then remove the SBR guard and test the audio decode
(the AU padding from the previous session should prevent the FDK trap).

**Bench ref level**: set to **-40 dBm** before every run. Presets and the backend's
own reference adjustment overwrite it. The user confirms the signal is visible at
this setting.

## Audio codec coverage

The receiver dispatches on the SDC audio coding field: 0 is AAC (with the SBR and audio-mode
flags in the descriptor), 3 is xHE-AAC. Both decoder paths are wired and committed.

| Codec | SDC | DecDRM station config | Receiver path | Status |
| --- | --- | --- | --- | --- |
| AAC-LC | coding 0, SBR off | `codec = "aac"` | FDK TT_DRM | decodes (the audio fixture) |
| HE-AAC | coding 0, SBR on, mono | `codec = "he-aac"` | FDK TT_DRM | the 24 kHz fixture decodes; the real bench stream is blocked by the FDK fault above |
| HE-AAC v2 | coding 0, SBR on, stereo | `codec = "he-aac-v2"` + `stereo` | FDK TT_DRM | verified on the live bench: 153 600 stereo 24 kHz samples, 1 kHz on both channels |
| xHE-AAC | coding 3 | `codec = "xhe-aac"` | libxaac with the AudioSpecificConfig from the SDC | wired; the USAC smoke test decodes 45 access units to non-silent PCM; the end-to-end DRM path is not yet tested |

Two notes for the next session:

- The audio fixture's SDC claims SBR while its payload carries none, so FDK is configured for a
  tail that does not exist. The synthesised fixture is therefore not a conformant SBR stream;
  the bench stream is.
- The xHE-AAC end-to-end path (coding 3 through the DRM receiver into libxaac) has never been
  driven with a real xHE stream. DecDRM can transmit one: set `codec = "xhe-aac"` in the bench
  station config.

## The post-lock pass, resolved

The earlier diagnosis blamed the per-symbol channel estimation, which was wrong. The pass
SNR collapsed frame by frame (20 dB to 1.7 dB) for two concrete reasons, both fixed:

- The MSC `CellDeinterleaver` was recreated per pass, so the long interleaver's five-frame
  fill ate the whole 3 s window. It now persists across passes and frames carry their
  absolute index, so overlapping windows feed each frame exactly once.
- `remove_carrier_offset` overwrote `mix_w` instead of accumulating it. After the
  whole-carrier anchor correction, every sample arriving after the lock was under-rotated
  by the first (fractional, ~120 Hz) correction. The lock pass never sees this — it rotates
  the whole buffer in place — which is why whole-buffer decoding masked it.

With both fixed, a streaming pass produces 25 access units from the 6 s capture and the
audio plays. The channel estimation itself was never the problem on this signal.

## Reference modules for the fix

DecDRM's receiver has modules that address exactly these weaknesses:

- `rx/freqacq.rs`: coarse carrier acquisition from the three continuous frequency pilots
  (750, 2250, 3000 Hz) in a 6 x 1024 FFT, searching the mirrored pattern too.
- `rx/timesync.rs`: guard correlation on a signal low-passed to +/-4.5 kHz and decimated
  by 4, like Dream's path.
- `rx/chanest/` (`time_wiener.rs`, `track.rs`): time-Wiener channel estimation with
  tracking, instead of our per-symbol linear interpolation.
- `rx/framesync.rs`, `rx/ofdm.rs`, `rx/mscdec.rs`: the frame sync, OFDM demod and MSC
  decoder as streaming stages.

These are in the same language (Rust) and the same project family. DecDRM is
GPL-2.0-or-later, so read them for the algorithms and implement independently.

## Next steps, in order

1. Bench-verify the audio live: ref level -35..-45 dBm before the run, transmit with
   `tools/pluto_drm_tx.py`, then a `tools/drm_capture.py` capture decoded through the wasm
   path (`frontend/scripts/drm_live_audio.mjs`) must show non-silent PCM, and the preset
   switch (DRM and back) must work without a page refresh.
2. Drive xHE-AAC end-to-end on the bench: set `codec = "xhe-aac"` in the DecDRM station
   config, capture, and check the libxaac path (`coding 3`) with a real xHE stream.
3. Exercise HE-AAC v2 (SBR + PS, stereo) the same way.
4. If the live bench shows channel-estimation or timing weaknesses, then port the DecDRM
   modules below; the offline suites pass today, so this is evidence-driven, not automatic.

## The remaining gap: channel estimation (next round's work)

The audio path is fixed and verified (see `docs/en/DRM_BENCH.md`). What is left is the
demodulator's channel estimation, and the reference receiver measures the size of the gap on the
same capture:

| Receiver | FAC | MSC frames | Audio |
| --- | --- | --- | --- |
| DecDRM's `decdrm rx` on `live30.f32` (30 s, MER 17.8 dB) | ok 64 / bad 9 | 71 (ok 40) | 200 frames (115 concealed) |
| Ours, same file | ok 40 / bad 52 | 64 | 55 access units (11 super frames) |

So with the same signal the reference decodes about four times as much audio. The cause is our
`wasm/src/digital/drm/chanest.rs`: per-symbol linear interpolation across the scattered pilots,
with no interpolation in time and no adaptation. The reference (and Dream, which it ports) does:

1. Gather the channel at the gain-reference (scattered) pilot grid, symbol by symbol.
2. **Wiener interpolation in time** across those symbols, using the Doppler/delay statistics —
   this is what carries a weak or fading signal (`rx/chanest/time_wiener.rs`, Dream's
   `CChannelEstimation::UpdateTimeWiener`).
3. **Wiener interpolation in frequency** from the pilot grid to every carrier
   (`rx/chanest/mod.rs::update_freq_wiener`, the Levinson-Durbin solve).
4. **Impulse-response tracking** (`rx/chanest/track.rs`, Dream's `CTrack`): delay spread, Doppler
   spread and sample-rate offset from the power delay profile; feeds (2) and (3) their statistics
   and the timing loop its correction.

Port source, all in Rust: `/home/hui/git/DecDRM/crates/decdrm-core/src/rx/chanest/{mod.rs,
time_wiener.rs, track.rs}` and `rx/scatter.rs` (the pilot/DSP helpers), with
`crates/decdrm-core/src/dsp/` for `levinson`, `iir1`, `sinc`. Dream's originals are
`src/chanest/` in the Dream sources.

Shape of the change in our tree: `chanest::equalize_symbol(map, sym, cells) -> EqSymbol` is
stateless and per-symbol; the Wiener estimator is stateful (the time filter spans several symbols,
so a symbol leaves the estimator a few symbols after it enters) and needs the SNR, the delay spread
and the Doppler spread. So it becomes a `ChannelEstimator` owned by `DrmReceiver`, fed one
demodulated symbol at a time, with a delay of `time_wiener::delay()` symbols; `decode()` must then
buffer the incoming symbols and consume the equalised symbols that come out. The SNR/MER numbers
it produces should replace our `snr_db` readout (which currently comes from the FAC decisions and
reads several dB low: -11 dB where the reference reports MER 17.8 dB).

Order of work, each step verifiable on its own:

1. The pilot grid + time-Wiener (replacing the frequency-only interpolation) — expect the FAC
   error count to drop first, then the MSC frame count.
2. The frequency Wiener and the SNR adaptation.
3. The impulse-response tracker (delay/Doppler/SRO), which also gives the timing loop its
   correction instead of our fixed grid.
4. Readout: report the estimator's SNR and MER, as the reference does.

Acceptance: on `live30.f32` our decoded-audio frame count approaches the reference's (`decdrm rx`
is the oracle), and the native suites (`drm_fixture`, `drm_live_fixture`, `drm_phy_robustness`)
stay green. The captures used for the measurements are in `/tmp` (`live30.f32`, `live60.f32`,
`lvl.f32`); a fresh one can be made with `tools/drm_capture.py`.

## Port progress (branch `feature/drm-dream-port`)

The previous receiver is abandoned; this branch re-implements the chain along Dream's stage
order in Rust, MIT-clean (Dream and DecDRM are read as references; no code is copied).
Work lands stage by stage, each with tests that cross-check the reference's own measurements:

| Stage | State | Evidence |
| --- | --- | --- |
| `drm2::params` — robustness-mode geometry (A–E) | done | A–D field-by-field against Dream's `tables/TableDRMGlobal.h`, the 400 ms frame invariant pinned; mode E (DRM+, 96 kHz, FFT 216, 100 ms frame, 40 symbols/frame, K −106..106, SO 0 only) transcribed from ES 201 980 V4.2.1 §8.2/§8.3 and cross-checked against gr-drm's `drm_config.cc` |
| `drm2::dsp` — complex type, DFT (radix-2 / Bluestein) | done | a pure tone lands in exactly one bin for 1024/1152/704/448/6144 |
| `drm2::sync::freqacq` — coarse carrier acquisition | done | the committed bench capture acquires at +121 Hz (within one carrier of the value `DRM_BENCH.md` records), the fixture acquires, an inverted spectrum is reported inverted, noise does not acquire |
| `drm2::sync::timesync` — guard correlation (low-passed, decimated), mode detection, timing and its tracking | next | |
| `drm2::sync::framesync` — frame phase from the time pilots | with task 3 | the time pilots live in the demodulated cells, so this needs the OFDM demodulation and the cell map (task 3) before it can be written and tested; it is folded into that task rather than guessed at here |
| `drm2::cellmap` + `drm2::tables` — the DRM cell layout (modes A–D × occupancies) | done | ported from this repository's own previous implementation (MIT, spec-derived, bench-verified); an equivalence test checks every legal mode/occupancy pair cell by cell, classification and pilot values included, plus spec anchors (65 FAC cells, mode B / 10 kHz at 2337 MSC cells per frame, three continuous pilots per symbol). Mode E added from the spec: 244 FAC cells (§8.5.2 table 66), 21 time pilots (table 57), no frequency pilots, 54 AFS cells in symbols 4 and 39 (table 61), five SDC symbols, no unused carriers — all pinned by tests |
| `drm2::ofdm` — FFT demodulation into the map's cells | done | agrees with the previous chain's demodulator sample for sample on the fixture's real windows; scattered-pilot ratios consistent (1387 pilots, spread 0.27); a synthesised mode E symbol (216-point FFT) demodulates back onto its carriers |
| `drm2::sync::framesync` — frame phase from the time pilots | done | the clean fixture syncs with a phase that survives shifting the search window by whole frames, and the bench capture syncs too |
| `drm2::sync::nco` — carrier-offset removal between acquisition and demodulation | done | removes a tone at the measured offset exactly and joins consecutive blocks without a phase step |
| `drm2::dsp::levinson` — the Haykin recursion Dream's Wiener filters use | done | diagonal and 2x2 systems (the first version was wrong; a diagonal-system test caught it) |
| `drm2::sync::finefreq` — residual carrier offset from the continuous pilots | done | injecting -2/-0.5/+0.5/+2 Hz into the fixture comes back within 0.1 Hz. OPEN: on the live capture it measures +2.62 Hz after the coarse correction while the search over residual offset and timing favours -1.0 Hz (3.7 dB) — the two disagree, so either the pilots' rotation carries more than the offset (a timing/sample-rate drift adds to it) or the search's optimum is not the offset's |
| `drm2::chanest` — pilot lattice, time interpolation, frequency Wiener, impulse-response tracking, FAC-decision MER | in progress | The frequency Wiener and the impulse-response tracker (`chanest/track.rs`, a port of DecDRM's `track.rs`) are in, and the time-interpolation step rotates each pilot to the emitted symbol's timing (DecDRM's `TimeWiener::rot`, sign verified against its timesync). Measured through the chain: clean fixture **41.6 dB** FAC MER (and its FAC now **decodes** — 13 blocks, 0 CRC failures), 1 ms echo **17.9 dB** (previous equaliser 10.9), low SNR **20.9 dB** (previous 18.8). The Wiener tap phase is now the reference's `π·pos·(len_ratio + 2·offs_ratio)` with `pos = i·x − diff` (the earlier `arg = 0` real taps and the per-symbol ramp de-rotation workaround are gone — the complex phase positions the delay spread and the timing ramp together). The live capture still reads **−8.6 dB** (reference 17.8). Diagnostics: the previous chain's per-symbol equaliser reads **−1.6 dB** on the same rows (so the front-end timing/carrier residual limits it), the old full-rate guard-correlation anchor reads **−9.2 dB** (so the decimated TimeSync is not the deficit), and clamping the delay-spread estimate to the echo's value makes it **worse** (−13.4 dB, confirming the ~10.6 ms spread is real and the 1-tap frequency-Wiener hold is correct). The remaining deficit is the **optimal window position** (Dream's timing tracking shifts the window to minimise the delay-spread ISI) plus the **time-Wiener (Doppler-adapted) time interpolation** — this port still uses a fixed linear interpolation where DecDRM's `TimeWiener` adapts to the Doppler spread. A closed-loop diagnostic (feed the tracker's `timing_adjust` back into the TimeSync) reads −10.4 dB and the tracker's energy method finds no clear first path on the spread profile, so the simple timing loop is not enough. The TimeWiener is **ported** in `chanest/time_wiener.rs` and now **wired in as an opt-in tracking path** (`start_time_wiener_tracking` switches from the exact linear interpolation, which stays the default so the clean-fixture MSC is bit-exact). On the live capture the time-Wiener path reads **−6.3 dB** FAC MER (the linear reads −8.6). The Doppler-spread estimate is now **enabled**: the instantaneous FAC MER fed back as the pilot SNR drove a positive-feedback collapse (σ pegged at 1.35 Hz, MER −19.6 dB); IIR-smoothing the SNR (5 s, the reference's `bound_snr` window) breaks the loop, and the Doppler-adapted time-Wiener now reads **+2.0 dB** FAC MER on the live capture (fixed σ reads −6.3, the linear −8.6). The `snr_pil_corr` correction and the `snr_after_ti` pass-through are in, completing the reference's SNR chain. The remaining gap to 17.8 dB is a **limit cycle in the estimation loop**: the delay-spread estimate alternates 103 ↔ 4 IR bins (period ~270 symbols ≈ 7 s, MER oscillating 1.7 → 0.1 → 2.2 → −8.2 dB) even *without* the timing loop — the grid→tracker→Wiener→grid feedback is unstable, which is what Dream's closed-loop timing stabilises. The tracker already computes `timing_adjust` and `sro_delta_hz` (live ~0.16 Hz ≈ 3 ppm); `ChanEst::start_timing_tracking`, `TimeSync::adjust_timing`, `TimeSync::adjust_sro` and `TimeSync::stop_timing_acquisition` now exist for the receiver loop, and `DrmReceiver` wires them with DecDRM's gating (time-Wiener after the first good FAC, timing tracking after the second plus a two-good-FAC countdown). On the live capture the loop **cannot start**: the FAC never decodes (0 blocks / 74 errors) because +2.1 dB MER is below the ~5 dB FAC threshold — the chicken-and-egg the reference breaks with a higher initial equalisation, so the next step is the initial equalisation quality, not the loop. **⚠ 2026-xx update: `/tmp/live30.f32` was NEVER verified before use.** A signal-level comparison against the clean fixture shows the live file's RMS is **45.3** (peak 213) vs the clean fixture's **0.18** (peak 0.80) — i.e. the live signal is ~254× (48 dB) hotter, meaning the capture was almost certainly taken at the wrong SDR reference level (the bench spec requires −35 to −45 dBm) and is severely overdriven. This invalidates the whole live30-vs-17.8 dB comparison: the deficit is a **capture-level problem, not a receiver defect**. Next step: re-capture the live DRM signal at the correct ref level (signal RMS comparable to the clean fixture) and re-run both receivers before drawing any further receiver conclusions. **Follow-up:** normalising the live signal's amplitude to the clean fixture's RMS (×1/254) does **not** help — the receiver still fails identically (0 FAC blocks / 74 errors), so the deficit is the live signal's **channel/propagation** (real multi-path), not the amplitude level or a receiver defect.
| `drm2::fec` + `drm2::fac` + `drm2::sdc` + `drm2::interleave` — MLC/Viterbi/CRC/interleavers and the FAC/SDC parsers | done | the MIT-clean FEC chain, the FAC/SDC parsers and the MSC cell deinterleaver are ported from the previous module (byte-identical apart from the Cplx path). FAC and SDC are both verified end to end on the clean fixture: 13 FAC blocks / 0 CRC failures with the manifest's occupancy/QAM/interleaving/service id, and 4 SDC super frames decoding to the station label `SAN90 DRM TEST` plus the audio descriptor (AAC/SBR/mono/24 kHz) and the multiplex description (EEP 0/1, one stream). |
| `drm2::mlc` (MSC) — cell/time deinterleave + MLC + multiplex demux | **done (bit-exact)** | the MSC decodes **bit-exact** against the xorshift stream (`msc_decodes_bit_exact`: frames match 8390/8390, shifted by one warm-up super frame). Three distinct defects were found and fixed this session. **(1) Super-frame assembly**: the MSC multiplex-frame boundary is every *N_MUX* (2337) **cells**, *not* every 15 symbols — mode B's frame 0 carries only 2123 MSC cells (the SDC symbols) while frames 1/2 carry 2445, so the sym-order cells must be concatenated and chunked by cell count exactly as the previous receiver does; a per-symbol flush produced 2445-cell frames. **(2) Flush point**: the buckets are keyed by super-frame symbol and flushed at frame 0's start (before collecting), skipping the first incomplete super frame that the chanest warm-up truncates. **(3) Timing ramp in the frequency Wiener**: the complex tap phase (`π·pos·(len_ratio + 2·offs_ratio)`, `pos = i·x − diff`) positions the delay spread and the timing ramp together, which real taps could not. The old `drm::DrmReceiver` stays the bit-exact oracle. |

Two facts worth carrying forward:

- **Dream has no mode E geometry.** `tables/TableDRMGlobal.h` defines `NUM_ROBUSTNESS_MODES 4`
  (A–D); its only mode E reference is the audio-super-frame frame count. Mode E/DRM+ (VHF)
  therefore cannot be ported from Dream and needs the DRM+ specification or another reference;
  it is its own task.
- **Reading fixtures from a test**: the Rust tests run with `wasm/` as the working directory, so
  committed fixtures are at `../tests/fixtures/...`. A `/tmp` capture is a bonus: gate its test
  on the file's existence, never assert its absolute carrier offset (that varies per session).

## Environment notes

- The backend stalls under load ("SDR stream stalled (watchdog)", 65 ms acquisition steps).
  Restart it with `./stop.sh && ./run.sh` before a bench run.
- Headless Chromium crashes on this machine (`chrome-headless-shell` traps in its compositor) -
  use Playwright's Firefox for the UI probes. `tools/e2e/demod_switch.py` still uses Chromium.
- Start the Pluto transmitter in a subshell (`( nohup ... & )`); chaining it with `&` and `&&`
  in one command line has silently failed more than once.

## The Wiener tap formula, read off the reference

`chanest/mod.rs::update_freq_wiener` (lines 220-235 of the reference) settles the question the
parameter search could not, and it names the two mistakes in the attempt recorded above:

- `rhp[i] = sinc((i*x - diff) * len_ratio)` and `arg = PI * (i*x - diff) * (len_ratio + 2*offs_ratio)`
  — `i*x` and `diff` are in **carriers**, not lattice steps (the attempt used lattice units, a
  factor `x` = 6 off).
- `len_ratio` is the impulse-response length over the useful symbol, which the reference takes
  from its tracking (`pds_len / n_car`; about 1 ms on this capture, i.e. ~0.047). The attempt used
  the guard ratio 0.25 — five times too large and not the physical quantity.

So one function changes: carrier units in `rhp` and in `arg`, and a delay spread near 0.05 (from
the tracking once it exists) instead of the guard ratio. Gate it on the clean fixture (>= 40 dB)
before the live capture.

### Measured: the reference formula alone does not close the gap

Implementing the formula above faithfully — carrier units and `len_ratio` = 0.05, right after the
lattice-index fix — still reads **17.1 dB** on the clean fixture, where the linear path reads 42.
The reason is structural and worth knowing before the next attempt: with a small delay spread the
correlation matrix is nearly singular, so the Levinson solve returns something close to an average
over the eleven lattice points. Averaging is what a *flat* channel wants, but our fixture's timing
offset makes the channel a linear phase ramp across carriers, and averaging across +/-30 carriers
destroys it. The reference can afford that filter because its **timing and sample-rate tracking
keeps the window aligned**, so its channel has no residual ramp; without the tracking, the local
two-point interpolation is the better estimator. So the next step is the tracking (Dream's
`TimeSyncTrack` and the reference's `track.rs`), and the frequency Wiener should be re-measured
after it, not before.

## The tracking stage, ready to implement

The measurements above say the tracking has to land before the Wiener can be re-measured, and both
references define it; the constants agree between them:

- **Power delay spectrum (PDS)** from the channel estimate's impulse response, smoothed with
  `TICONST_PDS` = 0.25 s, with the energy threshold `CONT_PROP_ENERGY` = 0.02, the minimum-statistics
  gate `NUM_SAM_IR_FOR_MIN_STAT` = 10 frames and `OVER_EST_FACT_MIN_STAT` = 4.0. The PDS gives
  `pds_len` and `pds_offset`, which are exactly the two numbers the frequency Wiener wants
  (`len_ratio = pds_len / n_car`, `offs_ratio = pds_offset / n_car`), and its **phase slope across
  time gives the sample-rate offset (SRO)**.
- **SRO tracking**: acquisition over `SAM_OFF_ACQ_LEN_S` = 4 s with a 1 s settle, then a steady
  state with `SRO_STEP_S` = 0.1 s steps, at most `SRO_MAX_STEP_BINS` = 3 bins per step,
  `SRO_TRACK_RATE` = 0.1, after `SRO_TRACK_MIN_S` = 5 s, over a history of `HIST_LEN_SAM_OFF_S` = 30 s.
- **Timing tracking** (Dream's `CTimeSyncTrack`): the guard-interval energy profile is searched for
  the target position `TARGET_TI_POS_FRAC_GUARD_INT` = 9 (twelfths of the guard), with the
  acceptance distances `TETA1_DIST_FROM_MAX_DB` = 20 dB and `TETA2_DIST_FROM_MIN_DB` = 23 dB and the
  contiguity proportions `CONT_PROP_IN_GUARD_INT` = 0.06 / `CONT_PROP_BEFORE_GUARD_INT` = 0.08; its
  correction is what our `TimeSync::timing_candidate` filter stands in for today (it has the outer
  shape — `LAMBDA_LOW_PASS_START` 0.99, `TIMING_BOUND_ABS` 150, `NUM_SYM_BEFORE_RESET` 5 — but no
  SRO input).
- **Where it goes**: the SRO correction belongs between the NCO and the timing stage (resample or
  re-phase), the PDS feeds `ChanEst`'s frequency filters, and the timing correction adjusts the
  symbol-window grid our `TimeSync` already emits. The reference's `track.rs` implements all three
  in one `PdsTracker`; Dream splits them into `CTimeSyncTrack` and `CTrack`.

### Low-SNR comparison (a task-4 acceptance item, measurable today)

`chanest::low_snr_tests::is_not_worse_than_the_previous_equaliser_at_low_snr` adds deterministic
AWGN (about 12 dB in-band SNR) to the fixture and reads the FAC MER through both estimators:
the new one reaches **20.2 dB**, the previous chain's per-symbol linear equaliser **18.8 dB**. The
gain is the three-symbol time interpolation averaging the pilot noise, and it is the first
quantified "low level" improvement over the previous implementation. The test asserts the new
estimator never trails the old one by more than a decibel, so a regression here fails the suite.

### One-millisecond echo (the "fading" acceptance item)

`chanest::fading_tests::is_not_worse_than_the_previous_equaliser_with_a_one_ms_echo` adds an echo
one millisecond behind the direct path — the delay spread the reference measures on the bench
capture — and reads the FAC MER through both estimators: the new one **14.9 dB**, the previous
chain's **10.9 dB**. Four decibels of margin under the frequency-selective condition the real
signal shows, again gated by a test. Both low-level and fading acceptance items of task 4 now have
measured baselines; the tracking and the Wiener work must preserve or improve them.

### Mode E: geometry now implemented from the standard

The reference checkouts really have no mode E (Dream's `NUM_ROBUSTNESS_MODES` is 4 and its
`Parameter.h` has no mode E geometry; DecDRM — transmitter included — defines no
`RobustnessMode::E` either). The geometry is therefore transcribed directly from the
standard — ETSI ES 201 980 V4.2.1 (fetched as a PDF during the port) §8.2 table 47, §8.3
tables 49/50, §8.4 tables 57/58/60/61 and §8.5 tables 62–66 — with gr-drm's `drm_config.cc`
(a GPL DRM+ transmitter) read only to cross-check the K range, symbol count, guard ratio and
FAC/SDC/pilot counts. The numbers agree, and two independent spec tables (the scattered-pilot
matrices of §8.4.4.3.6 and the AFS table of §8.4.5) cross-check each other in
`mode_e_afs_phases_match_table_61`. The 96 kHz sync front end and the mode-aware FAC decoder
are now implemented (see the mode E section below); the FAC/SDC/MSC signalling and audio
decode still need a mode E signal source.

## Mode E / DRM+ (VHF): scope and state

**Implemented and spec-pinned**: the mode E geometry is transcribed from ETSI ES 201 980
V4.2.1 (§8.2 table 47, §8.3 tables 49/50, §8.4 tables 57/58/60/61, §8.5 tables 62–66) and
cross-checked against gr-drm's `drm_config.cc`: 96 kHz baseband, FFT 216, guard 24, symbol 240,
40 symbols per 100 ms frame, 4 frames per super frame, K −106..106 (SO 0 only), 244 FAC cells,
21 time reference pilots, no frequency reference pilots, scattered pilots every 4 carriers
shifting 1 per symbol, 54 AFS cells in symbols 4 and 39. The tests pin all of it:
`mode_e_geometry_matches_the_spec` (params), `mode_e_layout_matches_the_spec` and
`mode_e_afs_phases_match_table_61` (cellmap), and the mode E arms of
`fac_cell_count`/`fac_positions`/`time_pilots`/`scattered_pilots` (tables).

**Implemented and independently verified on a synthetic mode E signal**: the mode-aware FAC uses
116 information bits, 4-QAM rate 1/4, two service descriptors, the RM flag and the four-frame
identity/toggle sequence. SDC uses mode E's 4-QAM rate 1/2 or 1/4. MSC uses the mode E 4/16-QAM
rate tables, six-frame cell interleaving, 29 842 useful MSC cells per super frame and 7 460 cells
per multiplex frame. The cell map's AFS-only cells are excluded from SDC/MSC data (the earlier map
counted 41 AFS cells as data). Native tests now generate a zero-noise 96 kHz Mode E signal with
FAC, SDC, four-frame MSC, and paired 200 ms AAC audio; the receiver decodes FAC 17/0, SDC 4,
seven MSC frames bit-exact and deframes 15 AAC access units. The same committed fixture
`drm_modeE_so0_96k_aac.f32` goes through the shipped `drmplus` wasm ABI and produces non-silent
PCM at both 48 kHz and 44.1 kHz output settings. Injected ±100 Hz carrier offsets also pass
FAC/SDC/MSC/AU recovery using cyclic-prefix phase tracking.

**Implemented entry path**: the plugin manifest exposes `drmplus` separately from DRM30. Selecting
it fixes the channelizer headroom to 100 kHz, builds the receiver at 96 kHz, highlights the ±50 kHz
DRM+ channel and uses the same decoded-audio worklet path. Selecting `drm` keeps the DRM30 SO3/10 kHz
path. The Filter control now labels both digital channel widths as automatic rather than offering
analog IF choices that do not change the broadcast occupancy.

**Still open**: no real DRM+ VHF transmitter or off-air Mode E recording is available in this
environment, so the synthetic result is not a substitute for a radiated VHF acceptance. Integer
carrier-offset acquisition beyond the cyclic-prefix unambiguous range, real VHF propagation and
mode E reconfiguration/service combinations still need a real signal source.

**Applicable scope when the 96 kHz chain lands**: DRM+ (mode E) is VHF only — band I/II
(47–108 MHz), 96 kHz baseband, one service, AAC/xHE-AAC audio. It shares the FAC/SDC/MSC
decoders with DRM30 (the MLC/Viterbi/CRC/interleaver are mode-agnostic once the cell layout and
the FAC cell count are mode-aware), so the remaining work is the rate-parameterised sync front
end and the mode-aware FAC decoder — roughly the `SAMPLE_RATE` uses above plus
`MlcParams::fac_for(mode)` — not a second receiver.

## Audio-path debugging and reliable offline data (2026-10-05)

The browser/live DRM audio cut out about once a second and then stopped. The following fixes
have offline coverage; real-time playback still needs verification:

- `websa_dsp_drm_audio_pcm` drained the whole receiver buffer but copied only the fixed output
  block (32768 samples), dropping the tail of every audio burst (~30% at 38.4 kHz). Fixed:
  `DigitalDemodulator::drain_audio_pcm(limit)` keeps the undrained tail
  (`drain_audio_pcm_keeps_the_tail`), and the worker's own cap is raised so a burst is never
  truncated.
- The receiver wedged after a run of FAC errors (a fade, or the bench cyclic buffer's wrap) and
  only revived when a setting was re-applied (a pipeline reset). It now re-acquires after 12
  consecutive FAC failures **or three seconds without a good FAC** (silence produces no timing
  windows and therefore no CRC failures). Recovery clears the old station, SDC/audio/MSC and
  codec state, and keeps only the last 1.5 seconds of baseband. Replaying the previous four-second
  window immediately relocked the *old* station on a dead channel. A fixture + silence + fixture
  regression confirms loss of lock and subsequent reacquisition; live verification is pending.
- Long-running wasm sessions now bound raw IQ, processed OFDM rows and retained FAC/MSC/AU
  history, without resetting cumulative readouts or the AAC decode cursor. FDK's interleaved
  HE-AAC v2 stereo output is downmixed before the mono worklet resamples it; otherwise the
  playback ran at the wrong duration/pitch. Native block-vs-batch checks and xHE/HE-AAC v2
  wasm fixture checks pass. No browser playback or real-station e2e was run for these changes.

- A remaining periodic cutout was found after the earlier fixes: the block-fed wasm fixture
  produces PCM bursts of 0.8 s, then 1.2 s every 1.2 s (`WEBSA_DRM_INCREMENTAL=1` in
  `frontend/scripts/drm_live_audio.mjs`). The mono worklet's 0.9 s ceiling discarded 0.3 s
  from every steady burst; its initial 0.25 s prime could also underrun before the next one.
  DRM now uses a 2.4 s ceiling and waits for two bursts (1.6 s prime), leaving analog modes
  at their 0.9/0.25 s settings. The burst-cadence worklet test pins zero slips and underruns;
  the change has not yet been checked by ear on the live bench.

- The DRM readout and FAC constellation now stay in their own structured floating pane instead
  of being interpreted as FT8 rows. The dedicated FT8 decoder and its extra IQ socket run only
  in FT8 mode; late cross-mode reports are ignored. The DRM readout no longer changes on every
  symbol just to repeat the symbol count. Targeted mode-switch/UI tests pass. The operator hears
  continuous audio after the worklet fix; occasional FAC errors at the Pluto file-loop boundary
  were reported, but the same operator sees no errors on a strong real station. The loop boundary
  is not yet independently measured, so do not tune the real-signal decoder against it.

- The DRM floating readout now fits service, mode, measured FAC MER and estimated FAC SNR,
  FAC good/bad, MSC/audio counts and a compact FAC constellation within its default
  460 × 240 window. SNR is decision-directed from weighted FAC errors, corrected for
  carrier power and nominal occupied bandwidth; it is not a calibrated RF noise-floor
  measurement and is shown as unavailable until a complete FAC frame. The committed
  HE-AAC fixture reports MER 19.9 dB / SNR 21 dB through the wasm ABI. A retune/stream
  reset clears the prior station and constellation immediately, including on an empty
  frequency. Dark/light and narrow-window component screenshots were checked.
- DRM30 currently fixes its decoded occupancy to SO3 (10 kHz); the old Filter buttons
  misleadingly offered 0.5–180 kHz although the digital decoder ignored them and the
  backend DDC remained ~48.8 kS/s for all narrow choices. Selecting DRM now applies
  12 kHz DDC headroom, labels the 10 kHz channel as automatic (manual choices hidden),
  and highlights ±5 kHz rather than FT8's +100..3000 Hz. This does **not** implement
  other DRM30 occupancies or mode E. The DRM core baseband rate is pinned to 48 kS/s
  independently of a 44.1/48 kHz sound card; PCM playback still follows the card rate.
- Rechecking `/tmp/cnr_rec.f32` through the current block-fed wasm ABI: the normal 32 kHz
  xHE interval outputs 1.203 s PCM per ~1.2 s burst on average, but after block 564 there
  is a ~14 s gap until block 775. The FAC count then restarts (93 -> 8), consistent with
  re-acquisition. This capture does not prove the smaller audible joins are codec bugs,
  and FAC good/bad counters reset at re-acquisition; a current `err 0` alone cannot prove
  continuous MSC or PCM. No live-device e2e was run for this diagnosis.

**Reliable offline data for the next session.** A known-audio bench loop: `cowtts` synthesises
speech to `/tmp/cowtts_test.wav`, `decdrm tx` builds `/home/hui/drm-bench/drm_iq_tts.wav`
(xHE-AAC mono 24 kHz), `tools/pluto_drm_tx.py` transmits it and `tools/drm_capture.py` captures
the baseband. On that loop our receiver and the reference both produce clean, continuous audio
(`/tmp/tts_rec.f32`, `/tmp/ours_tts_24k.wav`, `/tmp/decdrm_tts.wav`), so the **decoder is
exonerated**. On the real 13.825 MHz station (CNR-1, xHE-AAC 32 kHz, `/tmp/cnr_rec.f32`)
**both** decoders show the same splice artifacts, so that artifact is in the real signal or the
capture chain (HF fading and/or the capture path), not the decoder; the reference decodes more
frames there (725 vs our 536), which is the next robustness target. The committed fixtures
(`drm_live_modeB_so3_48828.f32`, the mode A/C/D, xHE and HE-AACv2 captures) remain the
deterministic regression set.
