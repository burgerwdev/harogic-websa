# DRM demodulator — handoff: state, evidence, next steps

This page is for whoever continues the work, including a later session with a different model.
`docs/en/DRM_BENCH.md` holds the measurements; this page holds the state and the plan.

## Where the work stands

Working, verified by tests and by the bench:

- The live bench capture locks. The receiver resolves the whole-carrier part of the carrier
  offset from the detected band edges and the fraction from the guard correlation, and it tries
  each super-frame phase and keeps the one whose SDC passes its CRC.
- The readout shows the station, its robustness mode, its bandwidth, its bit rate, its codec and
  its FAC SNR, and the MSC passes its CRC. It also reports again about every five seconds, so a
  cleared window recovers.
- Selecting another demodulator after DRM works without a page refresh: a trapped worker is
  dropped and the next mode change starts a fresh one.
- **The audio decodes.** The real HE-AAC stream (12 kHz core with SBR) decodes to non-silent
  24 kHz PCM in wasm, from the worker's block feed and from a whole-buffer push. The FDK trap
  is fixed at its root (the aligned allocator must zero, like FDK's `genericStds`); the SBR
  flag is no longer stripped; the post-lock passes produce audio (persistent MSC
  deinterleaver, accumulating carrier-offset corrections). Details and the evidence trail:
  `docs/en/DRM_BENCH.md`, defect 3.
- Suites: `cargo test --test drm_fixture` 4/4, the live-capture tests 5/5 (including the
  streaming-audio regression), the physical-layer robustness checks 5/5, the DRM wasm tests
  7/7 (`drmLiveAudio`, `drmAudioEndToEnd`, `drmAudio`, `drmXaac`, `workerRecovery`).

Open:

- Bench-verified 2026-10-04: the live capture decodes natively (35 access units) and through
  the wasm block-fed path (57 600 non-silent 24 kHz samples), and `tools/e2e/drm_switch.py`
  passes — DRM locks in the browser, survives a round trip to AM, no page refresh. One bench
  quirk to remember: the DDC's digital gain differs per service instance, and only the hot
  level locks — see the verified-live section of `docs/en/DRM_BENCH.md`.
- xHE-AAC end-to-end (coding 3 through libxaac) has never been driven with a real xHE stream;
  DecDRM can transmit one (`codec = "xhe-aac"`). HE-AAC v2 is wired but untested.
- The channel estimation is still the simplified per-symbol linear interpolation. The
  physical-layer suites pass on the bench capture, so the Wiener port below is only worth
  doing if the live bench shows a need.

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
| HE-AAC v2 | coding 0, SBR on, stereo | `codec = "he-aac-v2"` + `stereo` | FDK TT_DRM | wired, not yet tested on the bench |
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
| `drm2::params` — robustness-mode geometry (A–D) | done | field-by-field against Dream's `tables/TableDRMGlobal.h`, the 400 ms frame invariant pinned |
| `drm2::dsp` — complex type, DFT (radix-2 / Bluestein) | done | a pure tone lands in exactly one bin for 1024/1152/704/448/6144 |
| `drm2::sync::freqacq` — coarse carrier acquisition | done | the committed bench capture acquires at +121 Hz (within one carrier of the value `DRM_BENCH.md` records), the fixture acquires, an inverted spectrum is reported inverted, noise does not acquire |
| `drm2::sync::timesync` — guard correlation (low-passed, decimated), mode detection, timing and its tracking | next | |
| `drm2::sync::framesync` — frame phase from the time pilots | with task 3 | the time pilots live in the demodulated cells, so this needs the OFDM demodulation and the cell map (task 3) before it can be written and tested; it is folded into that task rather than guessed at here |
| `drm2::cellmap` + `drm2::tables` — the DRM cell layout (modes A–D × occupancies) | done | ported from this repository's own previous implementation (MIT, spec-derived, bench-verified); an equivalence test checks every legal mode/occupancy pair cell by cell, classification and pilot values included, plus spec anchors (65 FAC cells, mode B / 10 kHz at 2337 MSC cells per frame, three continuous pilots per symbol) |
| `drm2::ofdm` — FFT demodulation into the map's cells | done | agrees with the previous chain's demodulator sample for sample on the fixture's real windows; scattered-pilot ratios consistent (1387 pilots, spread 0.27) |
| `drm2::sync::framesync` — frame phase from the time pilots | done | the clean fixture syncs with a phase that survives shifting the search window by whole frames, and the bench capture syncs too |
| `drm2::sync::nco` — carrier-offset removal between acquisition and demodulation | done | removes a tone at the measured offset exactly and joins consecutive blocks without a phase step |
| `drm2::dsp::levinson` — the Haykin recursion Dream's Wiener filters use | done | diagonal and 2x2 systems (the first version was wrong; a diagonal-system test caught it) |
| `drm2::chanest` — pilot lattice, time interpolation, frequency Wiener, FAC-decision MER | in progress | the structure is in and the lattice-indexed frequency window fixed channels that came out exactly zero (an offset-plus-spacing formula read non-pilot carriers). OPEN: the equalised FAC constellation is a 4-QAM turned by about 45 degrees, so the MER reads negative; the two acceptance tests are ignored with that evidence. Lead: the phase convention shared by the pilot reference values, the FAC mapping and the estimator — the previous chain's equaliser on the same windows is the fastest way to see it. |
| `drm2::fac`, `sdc`, `mlc`, `msc`, `audio` | pending | see the task list |

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
