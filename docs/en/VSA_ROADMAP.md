# VSA roadmap — remaining work

A living checklist for turning [VSA_FEASIBILITY.md](VSA_FEASIBILITY.md) (the frozen
analysis) into a working mode. Every item names the measured number that drives it, so
"why does this matter" is never a guess. Statuses: **done**, **todo**, **blocked**
(needs something outside this branch), **unverified** (measured nowhere yet).

## 1. Current state

* **[done]** Feasibility analysis, probe suite and measured numbers:
  [VSA_FEASIBILITY.md](VSA_FEASIBILITY.md), `tools/vsa_probe/` (see its `FINDINGS.md`).
* **[done]** The device-link crash the analysis asked to fix (dead handle handed back to
  the vendor library) plus the supervisor startup-crash guard — shipped in **v1.7.4**, and
  master is merged into this branch.
* **[done]** Log rotation policy actually applies (v1.7.4).
* **[todo]** Everything below: `SET_MODE 'vsa'` does not exist yet, so today the analysis
  is still analysis.

## 2. Phase 1 — Tier 1 end to end

### 2.1 IQS reuse layer

* **[done]** `measurements/iqs.py` owns the IQS plumbing (profile, mode reset,
  post-configuration drain, one-packet fetch, the transient/fatal split, the wedge verdict)
  and `SdrSession` calls it; the SDR recovery *action* stays with the session because it has
  to rebuild the vendor FFT/DDC/demod chain. Verified by `tests/test_iqs.py` (16 cases,
  no vendor library needed) and a live SDR stream: 361 packets, 0 errors, 46 spectrum
  frames, 2.46 s of AM audio at a 994.7 Hz tone.
* **[todo]** The shared layer must consume `IQStream_TypeDef` from
  `web_sa/hardware/sdk_bindings` (728 bytes); the vendor wrapper's copy is 8 bytes short and
  the SDK writes past it on every packet.
* **[todo]** Keep the ~0.25 s post-configuration drain: measured 10824/10824 failed fetches
  without it, 0 with it.
* **[todo]** Keep the `-9` recovery: 2 of 6 decimate changes in multi-rate soaks wedged the
  stream permanently without it.
* **[todo]** Anything narrowband reuses what already exists rather than new DSP: the
  channelizer `demod/ddc.py` and the streaming FIR/resampler/AGC in `demod/filters.py`.
* Constraint: SDR behaviour must not change (its tests plus one live SDR stream are the
  proof).

### 2.2 Session, parameters, mode

* **[todo]** `VsaSession` following the `MeasurementSession` contract (`enter`/`exit` with a
  configuration snapshot, `step`, `health`, `request_stop`, `pacing`, `reconfigure`).
* **[todo]** `VsaParams` as a mode-private block next to `SwpParams`/`RtaParams`, restored on
  re-entry (see [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md)).
* **[todo]** `'vsa'` registered in `measurements/__init__.py` and in the `SET_MODE` choice
  list in `web_sa/web/commands.py`.
* **[todo]** `FakeVsaSession` so the mode is testable and demonstrable without hardware (the
  fake backend is what CI and the UI smoke use).

### 2.3 Tier 1 measurements

* **[todo]** Promote the probe modules into `web_sa/demod/vector.py`: Welch spectrum,
  power-versus-time, CCDF, spectrogram, carrier/timing-corrected constellation cloud.
* **[todo]** One absolute-dBm convention, documented once: `10*log10(mean|v|^2/50)+30` with
  the vendor `IQS_ScaleToV` — no extra 3 dB bandpass factor (measured: within 0.4 dB of the
  source with a linear front end).
* **[todo]** Record the cost of each measurement next to it (measured: power-time 22 %,
  CCDF 3 %, Welch 25 %, spectrogram 120 %, Tier-1 constellation 316 % of real time).
* **[todo]** The constellation must be shown with its measured uncertainty: blind symbol
  rate needs sps ≥ 4; a 4-fold phase ambiguity is inherent; the DC bin is drive-dependent
  (0.2–1.6 dB).

### 2.4 Acquisition paths

* **[todo]** Capture-and-analyse: one `FixedPoints` frame at the requested depth, then
  analyse and publish. Depth is verified to 2^24 samples with 0 packet errors; the transfer
  is roughly real time (1.04–1.83× signal duration), so the UI needs a busy/progress state
  instead of pretending to be live.
* **[todo]** Streaming Tier 1 for spectrum/waterfall: `Adaptive` with the same drain, display
  at ≤30 Hz. Fits for Welch (25 %); the spectrogram (120 %) does not stream and must be part
  of the analyse step.

### 2.5 Frame protocol

* **[todo]** `VSAD` frame in `measurements/framer.py`: symbol cloud (float32 interleaved
  I/Q), ideal points, and a compact measurement dict (EVM, MER, carrier offset, symbol rate,
  timing, SNR estimate).
* **[todo]** Register it in `web_sa/web/client_stream.py`'s latest-wins policy (a
  constellation is a snapshot) and keep `gen_frame_fixtures.py --check` green.
* **[todo]** Reuse `RTAF` unchanged for the spectrum/waterfall view.

### 2.6 Frontend

* **[todo]** A VSA mode switch, Tier 1 views on the existing RTA canvas, a constellation
  panel with a phase-rotation control (see the ambiguity), and a VSA settings group.
* **[todo]** Settings must be honest about the hardware: `RefLevel_dBm` is the input gain
  (linear to about −20 dBm at `RefLevel = 0`), and device limits come from
  `DeviceCapabilities`/`HardWareState`/`IQS_StreamInfo`, never from constants.
* **[todo]** i18n for both languages and the usual shortcut hints.

### 2.7 Tests for Phase 1

* **[todo]** Unit tests on synthetic IQ where the answer is known (burst duty 0.500,
  noise CCDF against Rayleigh, symbol-rate error, constellation scale).
* **[todo]** Session lifecycle: mode-private snapshot/restore, `health`, `SET_MODE`
  validation table.
* **[todo]** Fake-backend end-to-end: entering and leaving `vsa` without disturbing the other
  modes, plus a UI smoke that fails on a blank canvas or a JS error.

## 3. Phase 2 — Tier 2 demodulation

* **[todo]** Promote the probe chain into `web_sa/demod/digital.py`: symbol rate, timing,
  carrier recovery, slicing, EVM/SER, symbol table. Measured reference: EVM/theory
  1.003–1.023 for QPSK/16QAM over 8–30 dB.
* **[todo]** Vectorise (or decimate) the decision-directed PLL: the per-symbol Python loop is
  the whole cost (chain is 1.3–12× real time), and Tier 2 is a capture-and-analyse
  operation.
* **[todo]** Handle the 4-fold phase ambiguity explicitly — a known preamble/pilot when one
  exists, otherwise a user-controlled rotation with the ambiguity reported as a number, never
  a silently rotated symbol table.
* **[todo]** Refuse impossible inputs with a clear error instead of a wrong number
  (sps < 4 collapses the blind rate estimate).
* **[todo]** Degradation tests that record where it breaks (carrier offset beyond ~1 % of the
  symbol rate, timing error, roll-off extremes).
* Honest scope: symbol-level claims are validated on synthetic IQ — the bench has no
  calibrated PSK/QAM source (the tinySA produces CW/AM/FM only).

## 4. Phase 3 — process separation (**out of this round**)

* **[done]** Step 1: never hand a dead handle back to the library; supervisor stops after
  repeated startup crashes (v1.7.4).
* **[todo]** Step 2: move the SDK session (open/configure/fetch) into a dedicated child
  process so a native crash costs one capture instead of the web worker.
* **[todo]** Step 3: full separation (an SDK daemon owning the device) — only worth it when
  more than one consumer needs the device.
* Motivation stays measured: a stale handle made `Device_Close` segfault inside
  `libhtraapi`, which lost the whole worker (8 core dumps, one per restart attempt).

## 5. Loose ends deliberately not in this round

* **[todo] [blocked]** Cap the system core dumps (`/etc/systemd/coredump.conf` `MaxUse` +
  `coredumpctl vacuum`); needs root. Currently ~1.6 GB. Note `ulimit -c 0` does **not**
  suppress them here — measured.
* **[todo]** Remove the frontend's redundant 1.1 Hz `/api/state` poll: the WebSocket already
  pushes the same payload. It is the sole source of log volume (16.5 MB/day with a tab
  open). Rotation now bounds it, so this is tidiness, not urgency.
* **[todo]** Absolute level traceability: the tinySA has no calibrated absolute output, so
  "within 0.4 dB" is path consistency. A calibrated source would close this.

## 6. Unverified items (keep them marked)

* `IQS_TriggerSource` other than `Bus` (Level / External / GNSS1PPS / Timer / SpectrumMask).
* `TriggerLength` above 2^24 and back-to-back frames (trigger-to-trigger gap).
* Capture through `DSP_DDC` (all probes read the raw IQS stream).
* Long-run stability beyond the 40 s soak, and thermal drift.
* `QDCAutoMode`'s effect on image rejection (QDC stays off, the production default).
* Tier 2 on a **real** modulated signal (no PSK/QAM source on this bench).

## 7. Measured constraints the implementation must honour

| Constraint | Measured value |
|---|---|
| Post-configuration drain | 0.25 s; without it 10824/10824 fetches failed |
| `-9` wedge on reconfiguration | 2 of 6 rate changes; recovery must be reused, not rebuilt |
| `RefLevel_dBm` is the input gain | linear to ≈ −20 dBm at RefLevel 0; −15 dBm reads 3.9 dB low |
| `ScaleToV` across RefLevel | 33.7× for a 30 dB change — absolute volts, no extra factor |
| Deep capture depth | 2^24 samples, 0 packet errors, transfer ≈ real time |
| Streaming cost | raw fetch 1.5–18 % of a core; Welch 25 %; spectrogram 120 % |
| Blind symbol rate | needs sps ≥ 4; at sps 2 the estimate collapses |
| Carrier phase | 4-fold ambiguity inherent to the M-th power estimator |
| Image rejection | 78–92 dB — not a limiting factor |
| DC bin | 0.2–1.6 dB, drive-dependent (coherent LO leakage, not a notch) |
