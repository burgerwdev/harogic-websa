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
- Suites: `cargo test --test drm_fixture` 4/4, the live-capture tests 4/4, the physical-layer
  robustness checks 5/5, and the frontend suite 322 tests (plus one FT8 speed test that fails
  only when the machine is busy).

Open:

- The AAC audio of a real HE-AAC stream (a 12 kHz core with SBR) does not decode yet, and the
  receiver skips that configuration so a fault cannot take the session down.
- The readout does not follow the signal: the values come from the lock pass, because the
  receiver stops taking baseband once it locks. The fix is written and verified offline; see
  step 3 below.

## Bench facts that took time to learn

- Ref level **-45 dBm**: in-band contrast is then 25 to 27 dB. At -40 dBm it was 6 dB and
  nothing locked.
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

## Next steps, in order

1. Remove the SBR guard in `decode_audio` and feed the live capture through the node harness
   (`instantiateDsp`, push 3248 sample blocks, read `websa_dsp_drm_audio_pcm`). The padding from
   finding 3 may have removed the live trap as well. If the PCM is non-zero and non-silent, the
   audio path works.
2. If it still faults, finish finding 2 (land the aligned `FDKaalloc`, then build FDK with
   symbols and without `strip` to name the faulting function) or bound the fault so only the
   audio decode can fail.
3. Land the readout pass. It is written and verified offline, then reverted because the FDK
   fault made it unstable: the constants `PASS_SECONDS` and `WINDOW_SECONDS`, the fields
   `locked_r`, `locked_super_phase`, `locked_map`, `locked_row_start`, `pass_at`,
   `audio_units_decoded`, the removal of the early return in `push`, `decode_next_window`, a
   `row_base` argument for `decode`, and `clear_readout_state`.
4. Then finish task 4 (the preset path, on the bench) and task 5 (the fixture, the docs, and one
   full `make ci`).

## Environment notes

- The backend stalls under load ("SDR stream stalled (watchdog)", 65 ms acquisition steps).
  Restart it with `./stop.sh && ./run.sh` before a bench run.
- Headless Chromium crashes on this machine (`chrome-headless-shell` traps in its compositor) -
  use Playwright's Firefox for the UI probes. `tools/e2e/demod_switch.py` still uses Chromium.
- Start the Pluto transmitter in a subshell (`( nohup ... & )`); chaining it with `&` and `&&`
  in one command line has silently failed more than once.
