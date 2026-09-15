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
* **[done]** Phase 1 session layer: `SET_MODE 'vsa'` works end to end. `VsaSession` +
  `VsaParams` follow the `MeasurementSession` contract, `SET_VSA` is in the command table,
  `FakeVsaSession` backs the fake device, and a real SAN-90 run captured a full 2^24-sample
  frame with 0 packet errors (`tools/vsa_probe/vsa_service_check.py`).
* **[todo]** The rest of Phase 1: the Tier 1 measurement module, the `VSAD` frame, the
  frontend, and the closing reconciliation (sections 2.3, 2.5, 2.6, 2.7).

## 2. Phase 1 — Tier 1 end to end

### 2.1 IQS reuse layer

* **[done]** `measurements/iqs.py` owns the IQS plumbing (profile, mode reset,
  post-configuration drain, one-packet fetch, the transient/fatal split, the wedge verdict)
  and `SdrSession` calls it; the SDR recovery *action* stays with the session because it has
  to rebuild the vendor FFT/DDC/demod chain. Verified by `tests/test_iqs.py` (16 cases,
  no vendor library needed) and a live SDR stream: 361 packets, 0 errors, 46 spectrum
  frames, 2.46 s of AM audio at a 994.7 Hz tone.
* **[done]** The shared layer consumes `IQStream_TypeDef` from
  `web_sa/hardware/sdk_bindings` (728 bytes); the vendor wrapper's copy is 8 bytes short and
  the SDK writes past it on every packet.
* **[done]** The ~0.25 s post-configuration drain stays on the streaming path: measured
  10824/10824 failed fetches without it, 0 with it. A `FixedPoints` capture must **not**
  drain — see the recipe in section 7.
* **[done]** The `-9` recovery is reused rather than rebuilt: 2 of 6 decimate changes in
  multi-rate soaks wedged the stream permanently without it; both VSA paths re-arm in place
  off the shared verdict and escalate to a worker restart after `RECOVERY_LIMIT` attempts.
* **[todo]** Anything narrowband reuses what already exists rather than new DSP: the
  channelizer `demod/ddc.py` and the streaming FIR/resampler/AGC in `demod/filters.py`.
  (Tier 1 already reuses the shared panadapter `demod/spectrum.py` — the same windowed FFT
  the SDR display draws.)
* Constraint: SDR behaviour must not change (its tests plus one live SDR stream are the
  proof).

### 2.2 Session, parameters, mode

* **[done]** `VsaSession` following the `MeasurementSession` contract (`enter`/`exit` with a
  configuration snapshot, `step`, `health`, `request_stop`, `pacing`, `reconfigure`), in
  `web_sa/measurements/vsa.py`.
* **[done]** `VsaParams` as a mode-private block next to `SwpParams`/`RtaParams`, restored on
  re-entry (see [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md)). Verified on the bench: leaving
  `vsa` puts the swept centre/span back exactly.
* **[done]** `'vsa'` registered in `measurements/__init__.py` and in the `SET_MODE` choice
  list in `web_sa/web/commands.py`, with `SET_VSA` and a `NOT_IN_VSA` guard set.
* **[done]** `FakeVsaSession` so the mode is testable and demonstrable without hardware (the
  fake backend is what CI and the UI smoke use).

### 2.3 Tier 1 measurements

* **[done]** `web_sa/demod/vector.py`: Welch spectrum, power-versus-time, CCDF (plus the
  closed-form Rayleigh reference), spectrogram and the carrier/timing-corrected
  constellation cloud, selecting what runs through one entry point per `SET_VSA measure`.
  The symbol-level estimators it needs live next door in `web_sa/demod/digital.py`
  (`fft_resample`, Oerder-Meyr rate, timing + its half-symbol tie-break, M-th power CFO,
  the RRC matcher, the nominal grids); the decision-directed stages join them in Phase 2.
* **[done]** One absolute-dBm convention, documented once: `10*log10(mean|v|^2/50)+30` with
  the vendor `IQS_ScaleToV` — no extra 3 dB bandpass factor. Re-measured through the
  service against a known source (tinySA CW, 100.2 MHz, −25.0 dBm, `RefLevel 0`,
  decimate 16, 2^18 samples): mean −25.24 dBm, tone level −25.2 dBm, bin peak −28.27 dBm
  (the 2.00-bin window ENBW), noise density −138.9 dBm/Hz. The same **tone level** is what
  the reference tracker consumes; the bin peak is reported separately because a per-bin
  trace reads a CW tone ~3 dB low, and `duty` is 1.0 for a trace with no two levels.
* **[done]** Measured cost per measurement, recorded in the module docstring for the
  default 2^17 capture and for a deep 2^20 frame (one core, as % of real time):
  spectrum+levels 21/6, power-time 23/7, CCDF 57/21, spectrogram 136/44,
  constellation 389/181. The probe suite's 22/3/25/120/316 % over 100k-sample blocks is the
  same order; its CCDF figure used a block-averaged envelope, which is not comparable to
  Rayleigh. A capture-only pair (`spectrogram`, `constellation`) is refused in the stream
  view instead of being silently dropped.
* **[todo]** The constellation is shown with `sps_too_low` set when the blind rate implies
  fewer than 4 samples/symbol (measured: the estimate collapses at sps 2). Still open: the
  4-fold phase ambiguity in the UI (Phase 2 plus the frontend rotation control) and the
  drive-dependent DC bin (0.2–1.6 dB).

### 2.4 Acquisition paths

* **[done]** Capture-and-analyse: one `FixedPoints` frame at the requested depth, then
  analyse and publish, then arm the next frame (a capture view reports `busy` plus
  `progress`, it does not pretend to be live). Depth verified to 2^24 samples with 0 packet
  errors both off the probe and through the service; the transfer is roughly real time
  (1.00–1.01× signal duration measured per frame, 1.22× end-to-end through the service for
  2^24 samples, up to ~2.5× for frames short enough to be USB-bandwidth bound).
* **[done]** Streaming Tier 1 for spectrum/waterfall: `Adaptive` with the same drain, display
  at ≤20 Hz (`PAN_MIN_INTERVAL`). Fits for Welch (25 %); the spectrogram (120 %) does not
  stream and must be part of the analyse step (section 2.3).

### 2.5 Frame protocol

* **[done]** `VSAD` frame in `measurements/framer.py`: a float32 `(rows, cols)` matrix
  (interleaved I/Q for a cloud, `(x, y)` for a trace and a CCDF, a matrix for a
  spectrogram), the optional ideal grid, five head scalars (symbol rate, carrier offset,
  timing, EVM, SNR) and the **positional** measurement block named by `VSA_MEASURE_KEYS`
  (EVM/MER/SNR stay NaN until Phase 2). `decode_vsa` reads it with NumPy alone and refuses
  a frame whose declared shape does not match its length; `vector.frame_payload` produces
  display-sized slices (a 2^18-sample capture would otherwise be tens of thousands of
  points) so the frame stays bounded however deep the capture was.
* **[done]** Registered in `web_sa/web/client_stream.py`: latest-wins **per frame type**,
  because a capture publishes the spectrum (RTAF) and the measurement (VSAD) together and a
  single slot would let one evict the other. A slow client gets the newest cloud only, on
  both the unit-test level and a real 2 s stall on hardware.
* **[done]** Golden fixture `tests/fixtures/frames/vsa.bin` + manifest entry, produced by
  the production encoder, checked on both sides (`tests/test_frame_fixtures.py`, TS
  `frames.test.ts` → `decodeVsad`).
* **[done]** `RTAF` is reused unchanged for the spectrum/waterfall view; a capture publishes
  both frames, and the cloud never rides in `STATUS.vsa.last`.

### 2.6 Frontend

* **[done]** A VSA mode switch (`#btn-mode-vsa`, `graphMode` accepts `vsa`), the Tier 1
  spectrum/waterfall on the existing RTA canvas (RTAF, reused unchanged), and a panel that
  draws the measurement a VSAD frame carries: a symbol cloud against its ideal grid, a
  power-versus-time trace, a CCDF curve or a spectrogram (one canvas, because all four are a
  float32 matrix — see `core/vsaState.ts`).
* **[done]** The constellation panel owns the phase-rotation control. It writes `phase_rot`
  through `SET_VSA`, shows `resolved_by · rotation` and states which of the three answers it
  is (a preamble, the user, or nothing yet) — the ambiguity is never silent, and the panel
  warns while it is unresolved.
* **[done]** A VSA settings group: centre, decimation, depth, view (capture/stream),
  measurement, modulation, roll-off, symbol rate and the rotation. A capture-only measurement
  is *disabled* in the stream view rather than refused by the backend later, and entering VSA
  sends the whole geometry in one `SET_VSA` (a second configure in quick succession is the
  measured wedge hazard) after tuning to the marker/centre the user was looking at.
* **[done]** The reference level keeps using the shared Ref group, i.e. the measured input
  gain, and the depth/decimation lists are the ones the device accepts; no new client-side
  limits were invented.
* **[done]** i18n for both languages (`core/i18n/dict.vsa.ts`) and a hint row in the panel.

### 2.7 Tests for Phase 1

* **[todo]** Unit tests on synthetic IQ where the answer is known (burst duty 0.500,
  noise CCDF against Rayleigh, symbol-rate error, constellation scale).
* **[done]** Session lifecycle: mode-private snapshot/restore, `health`, `SET_MODE`
  validation table (`tests/test_vsa_session.py`, 15 cases, no vendor library).
* **[done]** Fake-backend end-to-end: entering and leaving `vsa` without disturbing the other
  modes (`test_fake_backend_round_trips_through_vsa`).
* **[todo]** A UI smoke that fails on a blank canvas or a JS error (with section 2.6).

## 3. Phase 2 — Tier 2 demodulation

* **[done]** The chain lives in `web_sa/demod/digital.py` (`demodulate`): symbol rate,
  timing, carrier recovery, slicing, EVM/MER/SNR, SER/BER and the Gray-coded symbol table.
  Measured on synthetic QPSK/16-QAM over 8–30 dB: EVM/theory median 0.99 (no bias; one 8 dB
  realisation in five reads 15 % high, which is the estimator's own variance there),
  SER/BER 0 with the preamble resolving the rotation, total carrier error ≤ 0.03 Hz.
* **[done]** The per-symbol PLL is gone: `track_carrier` fits one phase/frequency line per
  pass with the decisions as the reference (vectorised, three passes, outlier rejection).
  Measured 2.3 ms for 4000 symbols and 50 ms for 65000, i.e. **10× faster** than the probe's
  loop, and no longer the cost driver — the FFT stages and the matched filters are.
* **[done]** The 4-fold ambiguity is explicit: `resolve_ambiguity` either matches a known
  preamble (reporting every candidate's SER, and only one of the four reaches 0) or applies
  the user's rotation and reports it (`resolved_by` = `reference`/`user`/`none` plus the
  angle); a capture with no reference and no rotation comes back as `none`, never silently
  rotated.
* **[done]** Impossible inputs are refused with a code instead of a wrong number:
  `sps_too_low` (blind rate collapses below 4 samples/symbol), `no_symbol_line`, `silent`,
  `too_short`; `tier1_cloud` flags the same case with `sps_too_low`.
* **[done]** The timing stage is decision-aided: the blind |x|^2 estimate alone reached
  0.05 samples only at 30 dB (0.31 samples mean at 8 dB), so `refine_timing` minimises the
  decision-directed EVM over the sampling phase. Measured worst case 0.026 samples over
  8–30 dB, and 0.001 at 30 dB.
* **[done]** Degradation is recorded where the tests can see it: the 8 dB EVM spread (above),
  the sps < 4 collapse, and the clock case in section 7 (a CW carrier has no symbol line, so
  Tier 1 must never present the blind rate as a measurement).
* **[todo]** Roll-off extremes (0.1–0.9) and carrier offsets beyond ~1 % of the symbol rate
  are measured by the probes but are not pinned by a unit test yet.
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
| **FixedPoints read recipe** | Read **packet after packet with no read before the trigger**: back-to-back reads returned 2^24 samples in 3/3 runs (ratio 1.00) while one mid-frame read lost exactly one packet (114832/131072, 20 s of `-10`) and a read inside the settle window destroyed the frame (0 samples, `-10` forever). The Bus trigger start is the flush, so a capture does not drain |
| Capture depth granularity | `PacketCount` = ceil(depth / `PacketSamples`): 9 packets for 131072 samples, 1034 for 2^24 — planned count matched delivered count in every run |
| Capture transfer ratio (through the service) | 1.22× signal duration for 2^24 samples; 1.92× for 131072 samples (the fixed settle/arm overhead dominates a short frame) |
| Tier 1 absolute level (service, all five measurements) | tinySA CW −25.0 dBm → mean −25.00 dBm (re-measured through the frame path at 120 MHz), tone level −25.2 dBm, bin peak −28.3 dBm (window ENBW 2.00 bins), noise density −138.9 dBm/Hz |
| VSA frame path | 6 RTAF + 6 VSAD frames in 3 s at depth 2^17/decimate 16; the newest VSAD decodes with NumPy alone; a 2 s stall still leaves the newest cloud pending |
| Blind rate on a carrier | a CW tone has no symbol line: the estimate returned an arbitrary 554.95 kHz, so Tier 1 never presents a rate as measured (Phase 2 must reject it) |
| Streaming cost | raw fetch 1.5–18 % of a core; Welch 25 %; spectrogram 120 % |
| Blind symbol rate | needs sps ≥ 4; at sps 2 the estimate collapses |
| Carrier phase | 4-fold ambiguity inherent to the M-th power estimator |
| Image rejection | 78–92 dB — not a limiting factor |
| DC bin | 0.2–1.6 dB, drive-dependent (coherent LO leakage, not a notch) |
