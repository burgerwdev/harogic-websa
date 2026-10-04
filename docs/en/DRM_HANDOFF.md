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
| `drm2::chanest` — pilot lattice, time interpolation, frequency Wiener, impulse-response tracking, FAC-decision MER | in progress | The frequency Wiener and the impulse-response tracker (`chanest/track.rs`, a port of DecDRM's `track.rs`) are in, and the time-interpolation step rotates each pilot to the emitted symbol's timing (DecDRM's `TimeWiener::rot`, sign verified against its timesync). Measured through the chain: clean fixture **41.6 dB** FAC MER (and its FAC now **decodes** — 13 blocks, 0 CRC failures), 1 ms echo **17.4 dB** (previous equaliser 10.9), low SNR **20.9 dB** (previous 18.8). The Wiener taps are **real** (`arg = 0`): with the tracker's delay spread still inflated by the unresolved timing offset, the reference's tap phase rotated the channel on the flat fixture and broke the FAC phase; real taps keep the frequency-direction smoothing without the phase error, and the phase term returns once the timing tracking lands. The live capture still reads **−8.6 dB** (reference 17.8) with a delay-spread estimate of ~10.6 ms (~twice the guard) — the diagnostic that localises the remaining deficit to the **timing/sample-rate tracking loop**. The tracker already computes `timing_adjust` and `sro_delta_hz` (live ~0.16 Hz ≈ 3 ppm), but nothing applies them yet; that closed loop is the next step. |
| `drm2::fec` + `drm2::fac` + `drm2::sdc` + `drm2::interleave` — MLC/Viterbi/CRC/interleavers and the FAC/SDC parsers | done | the MIT-clean FEC chain, the FAC/SDC parsers and the MSC cell deinterleaver are ported from the previous module (byte-identical apart from the Cplx path). FAC and SDC are both verified end to end on the clean fixture: 13 FAC blocks / 0 CRC failures with the manifest's occupancy/QAM/interleaving/service id, and 4 SDC super frames decoding to the station label `SAN90 DRM TEST` plus the audio descriptor (AAC/SBR/mono/24 kHz) and the multiplex description (EEP 0/1, one stream). |
| `drm2::mlc` (MSC) — cell/time deinterleave + MLC + multiplex demux | in progress | the MSC chain runs (9 frames), but the **bit-exact check fails (~51% equal)**. Direct cell-by-cell diff vs the previous receiver showed the drm2 cells land on the **same 64-QAM points**, just **noisier** (sym-15 errors ~0.12 vs ~0.03). **Timing is ruled out**: per-symbol pilot-ramp correction drives the window offset to 0.00, and additionally disabling the `rot()` (`shift=0`) leaves the MSC at ~51%; `iterations=0` (single-pass multistage) is only ~68% on the best frame, so the second pass is not the defect either. The LSB (level 2) is ~50% correct — the signature of cell noise. Remaining suspect: a noise source in the chanest's equalisation (time/frequency interpolation) that the FAC 4-QAM and SDC 16-QAM tolerate but 64-QAM does not. The old `drm::DrmReceiver` stays the bit-exact oracle. |

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
`mode_e_afs_phases_match_table_61`. What is NOT yet done for mode E is its 96 kHz sync front
end and FAC/SDC/MSC signalling decode — that is task 8, after the DRM30 chain decodes.
