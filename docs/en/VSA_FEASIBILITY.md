> **Status.** This document is the frozen feasibility analysis: its numbers are what the
> probe suite measured. What has since been built, what is verified and what is still open
> lives in [VSA_ROADMAP.md](VSA_ROADMAP.md), and `tools/vsa_probe/FINDINGS.md` section 15
> reconciles these numbers with the shipped feature.

# VSA feasibility on the SAN-90 (WebSA)

Vector signal analysis (VSA) means measuring *and demodulating* a digitally
modulated signal: power versus time, CCDF, spectrum and spectrogram,
constellation and EVM, and — for the second tier — symbol decisions and a symbol
table. This document answers whether WebSA can offer that on the SAN-90, what it
would cost, and what it must not assume.

It is an analysis: **no VSA production code exists**. Everything numeric here was
measured on the bench or in an offline loopback; the raw evidence lives in
[tools/vsa_probe/](../../tools/vsa_probe/README.md) with the full result tables in
[tools/vsa_probe/FINDINGS.md](../../tools/vsa_probe/FINDINGS.md). Claims are
marked **[measured]**, **[inferred]** (reasoned from code or SDK structure without
a measurement) or **[unverified]**.

## 1. Verdict

**Feasible in two tiers, with one hard constraint and one architectural one.**

* **[measured] Tier 1 is cheap and low-risk.** The SAN-90 captures a 16 777 216
  sample (128 MB) triggered frame with zero packet errors and no shortfall, and
  streams Adaptive IQ for 40 s at 0.98–15.6 MSPS with zero fetch errors. Power
  versus time, CCDF against the Rayleigh reference, a Welch spectrum and a
  carrier-corrected constellation all run between 3 % and 316 % of real time
  (only the spectrogram and the Tier-1 constellation exceed real time, and both
  belong to a capture-and-analyse model anyway).
* **[measured] Tier 2 is feasible but is capture-and-analyse, not streaming.**
  The in-house NumPy demodulator meets the matched-filter bound: measured EVM
  1.419 % against a theoretical 1.406 % at 25 dB SNR (QPSK), and the EVM/theory
  ratio stays within 1.003–1.023 from 8 to 30 dB for QPSK and 16QAM. But the
  chain costs 0.017–0.047 s for 16.9 ms of signal (128–1176 % of real time), so
  it must run on a captured frame, not inside the streaming loop.
* **[measured] The vendor's digital demodulation library is not an option.**
  `Demod_Check() == -1` (no `libDigitalSigDemod.so`, no licence), so every
  symbol-level step is ours. The plan below assumes no such library.
* **[measured] One real hardware hazard.** After a physical unplug,
  `Device_Close` on the stale handle **segfaults inside `libhtraapi`** and the
  worker crash-loops; while the analyzer is off the bus `Device_Open` returns
  −1. This is the deferred "Web/SDK process separation" item, and §6 gives the
  measured case for doing at least part of it in the VSA phase.
* **[measured] Absolute level is trustworthy.** With `RefLevel_dBm = 0` the IQ
  path reports the source level within 0.4 dB in its linear range (about
  −20 dBm and below) and agrees with the vendor's swept trace within ~2 dB.
* **[measured] Image rejection is a non-issue, and the DC bin has a small,
  drive-dependent effect.** Rejection is 78–92 dB at every offset tested, so an
  image cannot limit EVM. A tone sitting exactly on the IQS centre reads
  0.1–0.25 dB low at a −25 dBm drive and 1.0–1.6 dB low at a −15 dBm drive
  (compressed), while the block *power* at DC is unchanged — an LO-leakage
  vector, not a notch. A VSA must not treat the centre bin as trustworthy at
  high drive.

## 2. Capability boundary

### 2.1 Tier 1 — vector measurements without decisions

Spectrum (Welch, absolute dBm), power versus time and burst duty cycle, CCDF with
the Rayleigh reference, spectrogram/waterfall, and a constellation cloud built
from the matched-filter output with software carrier and timing correction. No
symbol decisions, so it works on any signal, including ones with no known
modulation — which is what makes it the safe first tier.

### 2.2 Tier 2 — in-house PSK/QAM demodulation

Blind or guided symbol-rate estimation, RRC matched filtering, timing recovery,
carrier recovery, PSK/QAM slicing, EVM/SER, and a symbol table. Validated on
synthetic QPSK/16QAM with injected timing offset, carrier offset and AWGN, because
the tinySA can only produce CW/AM/FM **[measured]**. The chain is offline-verified
against a theoretical floor and is *not* yet verified on a real modulated signal —
no calibrated PSK/QAM source is available on this bench **[unverified]**.

### 2.3 Explicitly out of scope

No `SET_MODE 'vsa'`, no frontend panel, no production code, no change to existing
modes, no vendor demodulation library acquisition, no FT8/WSPR or other concrete
digital protocol, no hardware modification. Those are the *plan*, not this work.

## 3. Measured evidence — hardware

### 3.1 IQ capture: framed and continuous

`probe_iq_capture.py` (SAN-90, tinySA CW at 100.200 MHz, −25 dBm).

* **Triggered frames** (`FixedPoints`): 262 144 samples in 17 packets, 0 errors,
  0 shortfall, on 5 consecutive runs; trigger latency 0.1 ms; capture of 33.5 ms
  of signal completed in 34.7 ms (≈1.2 ms fixed overhead). The peak lands within
  2 Hz of the commanded +200 kHz offset inside a run (200 002.0 Hz ± 0.47 Hz; a
  later session measured 199 985.0 Hz ± 0.015 Hz, so the source's own setting
  repeats to about ±20 Hz), and at −24.48…−24.66 dBm for a −25 dBm source
  (±0.07 dB within a run). Multi-packet reassembly is clean: the phase residual
  at packet boundaries (0.0098 rad max) is *smaller* than in the frame interior
  (0.0225 rad max).
* **Depth**: 2^14 … 2^24 samples all completed at decimates 4, 16 and 64, with
  0 errors — a 2^24 frame is 1.07 s (dec 4), 4.30 s (dec 16) or 17.18 s
  (dec 64) of signal and takes 1.96 s / 5.05 s / 17.93 s to transfer. Deep
  capture therefore works, but it is **not faster than real time**.
* **Trigger semantics**: `IQS_BusTriggerStart` starts the frame immediately
  (0.1 ms). Without it the first fetch returns **−10 (BusTimeOut)** after the
  configured `BusTimeout_ms` (0.504 s for 500 ms) — a usable "no signal" outcome.
* **Continuous stream** (`Adaptive`): a 40 s soak gave 0 errors at every rate,
  worst 10 s window rate ratio 0.99848, and a settle drain of ~0.25 s worth of
  queued packets is **mandatory** after any reconfiguration — without it, a
  decimate change produced 10 824/10 824 failed fetches, with it 0.

### 3.2 Level, DC and image

`probe_iq_level.py`.

* **Absolute level**: with `RefLevel_dBm = 0` the IQ block mean reports −29.61 /
  −24.73 / −19.92 dBm (one run) and −29.73 / −24.74 / −19.90 dBm (a repeat) for
  −30 / −25 / −20 dBm from the source, i.e. within **0.4 dB** (0.21 dB mean
  offset on the repeat); the coherent tone estimate agrees with the block mean to
  0.03–0.14 dB.
* **`ScaleToV` is absolute across RefLevel**: setting RefLevel from 0 to −30 dBm
  changes `IQS_ScaleToV` by 33.7× = 30.6 dB, matching the 30 dB change, so
  `10·log10(mean|v|²/50) + 30` is correct with the vendor scale and needs no
  extra 3 dB bandpass factor.
* **Headroom follows RefLevel**: linear to about −20 dBm at `RefLevel = 0` (a
  −15 dBm input reads 3.9 dB low) and only to about −25 dBm at
  `RefLevel = −30 dBm` (a −20 dBm input reads 6.2 dB low). Compression shows
  twice over — the block mean falls short *and* the coherent tone collapses
  further — so a VSA must choose `RefLevel_dBm` per signal level, not treat it as
  a display setting.
* **Vendor cross-check**: the same tone through the production swept path
  (`HarogicDevice` + `StdSession`) agrees within ~2 dB, but that comparison is
  bounded by the swept trace's own peak spread of ±2–4.6 dB. Two traps were
  found: a 200 kHz span with the default minimum sweep time gives an
  **uncalibrated** trace (peak pinned at ≈−19.5 dBm for every source level), and
  `state.swp.*` fields are proxied flat onto `state`, so a wrong name (for
  example `state.rbw` instead of `state.rbw_hz`) silently keeps the 100 kHz
  default RBW.
* **DC**: a tone sitting exactly at the IQS centre reads 0.1–0.25 dB below its
  level at ±500 Hz when driven at −25 dBm, and 1.0–1.6 dB below when driven at
  −15 dBm (a compressed operating point). The block power at DC is unchanged in
  both cases, so this is a coherent leakage vector at the centre, not a notch.
  Total spread over ±100 kHz: 0.29 dB at −25 dBm, 1.2–1.6 dB at −15 dBm.
* **Image**: rejection 77.8–92.3 dB across two runs at offsets 100 kHz … 1.5 MHz
  (QDC off, the production default). An image cannot limit EVM.

### 3.3 Cost, loss and recovery

* **[measured] Fetch cost**: 1.5 % (dec 64) to 18 % (dec 4) of one core for
  reading and copying the IQ stream with no DSP attached — the DSP budget starts
  from there.
* **[measured] Post-configuration drain**: 0.25 s of discarded packets is the
  difference between zero errors and total failure (§3.1).
* **[measured] A decimate change can wedge the stream.** Across three soak runs,
  two of six rate changes left *every* fetch returning **−9 (BusDataError)** for
  the whole window (9622/9622 and 3608/3608), while the same rates were clean
  when they were the only rate in the process. The production `SdrSession`
  already treats `−9` as transient and reconfigures in place after a persistent
  streak; a VSA session must reuse that recovery, and the probe (which has none)
  is how the failure was found.
* **[measured] Use production's `IQStream_TypeDef`.** `sdk_bindings` re-declares
  it at the header-correct 728 bytes — the vendor wrapper's copy is 8 bytes short
  and the SDK writes past it on *every* packet — so any new capture code must go
  through `sdk_bindings`, not `htra_api`'s struct.
* **[measured] Crash on a lost link**: `Device_Close` on a stale handle
  segfaults inside `libhtraapi` (`web_sa/hardware/device.py:127`), and the worker
  then exits with status −11 and restarts in a loop while the analyzer is absent.
  The Python layer cannot catch it (see §6).
* **[unverified] Long-run stability**: the longest continuous run measured here is
  40 s; hours-scale behaviour, thermal drift and repeated deep captures are not
  characterised.
* **[unverified] Trigger sources other than `Bus`**, `TriggerLength` above 2^24,
  back-to-back frames, and the `DSP_DDC` path for VSA captures.

## 4. Measured evidence — synthetic DSP loopback

`probe_siggen_selfcheck.py` and `probe_dsp_loopback.py`, offline, no hardware.

### 4.1 Tier 1 accuracy and cost

* **[measured] Burst duty cycle** 0.500 measured against 0.500 expected.
* **[measured] CCDF**: a noise-only capture follows the Rayleigh reference to
  0.0021 maximum absolute deviation (single samples; block-averaged CCDF is
  steeper and must not be compared with that curve).
* **[measured] Symbol rate**: −0.03 Hz error on a 244.125 kSym/s signal
  (blind, from the |x|² line).
* **[measured] Carrier from the spectrum**: the power-weighted centroid is biased
  by −735…−778 Hz *independently of SNR* (10–40 dB) with ~640 Hz spread across
  noise realisations — finite-data PSD asymmetry, not noise. It is a coarse
  indicator only; the Tier-2 estimator is the precise one.
* **[measured] Cost as a fraction of real time** (33.7 ms capture): power versus
  time 22 %, CCDF 3 %, Welch spectrum 25 %, spectrogram 120 %, Tier-1
  constellation 316 %.

### 4.2 Tier 2 EVM against theory

Reference: the matched-filter bound `EVM = 1/sqrt(sps · SNR)`, itself validated in
the generator self-check.

| Kind | SNR 8 dB | 12 dB | 16 dB | 20 dB | 25 dB | 30 dB |
|---|---|---|---|---|---|---|
| QPSK measured % | 9.986 | 6.314 | 3.991 | 2.521 | 1.419 | 0.799 |
| 16QAM measured % | 10.181 | 6.411 | 4.040 | 2.547 | 1.432 | 0.805 |
| theory % | 9.953 | 6.280 | 3.962 | 2.500 | 1.406 | 0.791 |

EVM/theory ratio 1.003–1.023, SER 0 throughout. Timing offsets of 0, 0.25, 0.5
and 0.75 sample are recovered to ≤0.03 sample (EVM 1.419 → 1.437 %); roll-off
0.1 → 0.9 keeps EVM in 1.585 → 1.411 %; the carrier estimate is accurate to
0.1 Hz at 1 % of the symbol rate.

### 4.3 Degradation and failure conditions

* **[measured] Carrier offset**: EVM 1.419 % (0 Hz) → 1.535 % (122 Hz) →
  2.846 % (488 Hz) → 3.388 % (2441 Hz) → 1.833 % (12.2 kHz). The non-monotonic
  shape comes from the coarse estimate's residual versus what the
  decision-directed loop has to absorb; a real VSA should keep the residual
  inside ~0.2 % of the symbol rate for best EVM.
* **[measured] A 4-fold phase ambiguity is inherent** to the M-th power estimator:
  the recovered constellation can be rotated by 0/90/180/270°. EVM is unaffected
  (the LS gain absorbs a global rotation) but the *symbol table* is wrong by a
  whole quadrant. A product VSA needs a known preamble/pilot, or must present the
  constellation modulo rotation and let the user rotate it.
* **[measured] Blind rate estimation needs oversampling**: at 2 samples/symbol
  (symbol rate at Nyquist) the |x|² line falls outside the search band and the
  estimate collapses (350 kHz reported for a 1.95 MHz symbol rate). The chain
  needs **sps ≥ 4**, i.e. an IQ rate of at least ~3× the symbol rate.
* **[measured] Cost**: the full chain (rate estimate, resample, timing, matched
  filter, M-th power carrier, decision-directed PLL, EVM) costs 0.0177–0.0467 s
  for 16.9 ms of signal — 128 % of real time at 61 kSym/s, 546 % at 244 kSym/s,
  1176 % at 977 kSym/s with 8 samples/symbol. The decision-directed PLL is a
  per-symbol Python loop and dominates; vectorising it, or running the loop at a
  decimated update rate, is the obvious optimisation **[inferred]**.
* **[measured] Degradation found while building it** (worth keeping): the
  matched filter's transient and zero tail must be dropped (9.9 % phantom EVM),
  the symbols must be RMS-normalised before slicing, and a wrong assumed symbol
  rate makes the resampler stretch the time base and drift the timing across the
  frame (9.6 % EVM).

## 5. Integration plan

### 5.1 Mode and session

Add `'vsa'` to `SET_MODE` and a `VsaSession` in `web_sa/measurements/`, following
the existing `MeasurementSession` contract (enter/exit with a configuration
snapshot, `step()`, `health()`), registered in
`web_sa/measurements/__init__.py` and in the `SET_MODE` choice list in
`web_sa/web/commands.py`. Mode-private settings follow the convention in
[docs/en/MODE_STATE_FLOW.md](MODE_STATE_FLOW.md): a `VsaParams` block alongside
`SwpParams`/`RtaParams`, restored on re-entry, with the existing
`SessionManager.switch()` sequencing `stop → exit → construct → enter → ready`.

### 5.2 Acquisition model

Two paths, chosen by the operation:

* **Capture and analyse** (the default for anything vector): one `FixedPoints`
  frame at the requested depth → analyse → publish one result frame. Measured
  depth 2^24 samples with zero errors; the transfer is roughly real time, so the
  UI must show progress rather than pretend it is live.
* **Streaming Tier 1** for spectrum/waterfall/CCDF: an `Adaptive` stream with the
  same post-configuration drain as the SDR session, updating the display at
  ≤30 Hz. Measured sustained cost with no DSP is 1.5–18 % of a core, leaving room
  for a Welch spectrum (25 % measured) but not for a spectrogram (120 %).

A `DSP_DDC` channel can narrow the analysis bandwidth for narrowband signals
(reusing `web_sa/demod/ddc.py`), but that path has not been measured with the VSA
configuration **[unverified]**.

### 5.3 Frame protocol and frontend

* Reuse **RTAF** unchanged for the spectrum/waterfall view: it already renders in
  the RTA canvas and the SDR mode proves the pattern.
* Add one frame for vector data — a `VSAD` frame carrying the symbol cloud
  (float32 interleaved I/Q), the reference/ideal points, and a compact
  measurement dict (EVM, MER, carrier offset, symbol rate, timing, SNR estimate).
  Keep the existing 16-byte-header framing in `web_sa/measurements/framer.py` and
  register it in `web_sa/web/client_stream.py`'s latest-wins policy (a
  constellation is a snapshot, so latest-wins is right).
* Frontend: reuse the RTA canvas for spectrum/power-time/CCDF/spectrogram and add
  a constellation panel plus a VSA settings group. `SET_MODE` handling, the
  mode-private settings restore and the canvas interaction already exist for
  RTA/SDR; no new rendering engine is needed **[inferred]**.

### 5.4 Configuration and capability limits

Expose: centre/span (or centre + capture bandwidth), capture depth or duration,
`RefLevel_dBm` (documented as *the* input gain, with the measured headroom),
decimate factor, Tier-1 view selection, and — for Tier 2 — modulation, symbol
rate (or blind), roll-off, and whether a preamble resolves the phase ambiguity.
Limits must come from `DeviceCapabilities`/`HardWareState` and from
`IQS_StreamInfo` (sample rate, bandwidth, packet sizes), never from constants:
the repo already has a single source for device limits, and the depth limit
measured here is a probe choice, not a device ceiling.

### 5.5 Reuse versus new code

| Piece | Verdict |
|---|---|
| IQS configure/fetch/recovery, settle drain | **Reuse** `web_sa/measurements/sdr.py` (extract the IQS plumbing it already owns) |
| `IQStream_TypeDef` / `IQS_GetIQStream_PM1` binding | **Reuse** `web_sa/hardware/sdk_bindings.py`: its struct is the header-correct 728 bytes, the vendor wrapper's is 8 short (§3.3) |
| `DSP_DDC` channelizer | **Reuse** `web_sa/demod/ddc.py` |
| Streaming FIR/resampler/AGC | **Reuse** `web_sa/demod/filters.py` |
| Frame encode/decode, latest-wins policy | **Reuse** `web_sa/measurements/framer.py`, `web_sa/web/client_stream.py` |
| Mode/session lifecycle, mode-private state | **Reuse** `measurements/__init__.py`, `web_sa/hardware/state.py`, `home`-convention in `MODE_STATE_FLOW.md` |
| RTA canvas rendering | **Reuse** `frontend/modern/src/render/spectrum.ts` (+ the mode switch in `src/ui/controls.ts`) |
| `SdrSession` as the VSA base class | **Do not** subclass: the demod/audio chain is SDR-specific. Share the IQS plumbing, not the session |
| Tier-1 measurements (spectrum, power-time, CCDF, spectrogram, constellation) | **New**: `web_sa/demod/vector.py`, promoted from `tools/vsa_probe/measure.py` |
| Tier-2 chain (rate/timing/carrier/slicing/EVM) | **New**: `web_sa/demod/digital.py`, promoted from `tools/vsa_probe/demod.py` |
| `VsaSession` | **New**: `web_sa/measurements/vsa.py` |
| `VSAD` frame | **New** in `framer.py` |
| Constellation panel, VSA settings group | **New** frontend code |

The probes were written as standalone modules exactly so this promotion is a move,
not a rewrite: `siggen.py` (synthetic source and theory), `measure.py` and
`demod.py` have no dependency on the probe harness.

## 6. Process model — Web/SDK separation

The architecture review defers "persistent Web/SDK process separation" to the VSA
phase, and `KNOWN_ISSUES.md` repeats it. The measurements now give that deferral a
concrete cost:

* **[measured]** A stale SDK handle makes `Device_Close` segfault *inside*
  `libhtraapi`. Python cannot catch it, so the whole worker dies (exit −11) and
  the supervisor restarts it in a loop for as long as the analyzer is missing.
  For a VSA this is worse than for spectrum display: a deep capture holds
  hundreds of megabytes and a long DSP result, all of which are lost.
* **[measured]** Recovering also requires not calling `Device_Close` at all once
  the link is known lost; today `reopen()` calls `close()` first.

Recommendation, smallest useful step first:

1. **Now (cheap, no new process)**: make `reopen()` skip `Device_Close` when the
   link is already marked lost, and treat `Device_Open` failure as a retryable
   state instead of a crash path. This removes the measured crash loop.
2. **VSA phase**: keep the supervisor/worker shape but move the *SDK session*
   (open/configure/fetch) into a dedicated child process that the worker talks to
   over a pipe, so a native crash costs one IQ capture, not the web worker. The
   analysis stays in the worker where the NumPy buffers already live.
3. **Later**: full separation (an SDK daemon owning the device, the web worker
   purely a client) — only worth it if more than one consumer needs the device.

Step 1 is implemented and bench-verified (shipped in **v1.7.4**: no new core dump with the
 analyzer unavailable, quiet retry, automatic resume when it is released, clean stop in both
 link states; the supervisor now stops after 5 consecutive crashes that each lasted under
 10 s). Steps 2 and 3 remain.

Impact on the existing recovery chain: steps 1–2 do not change RTA/SDR session
reconfiguration, the link watchdog, or the mode switch; they change *where* the
SDK calls happen and what happens when they crash. Step 3 would touch the
supervisor contract and should be its own change.

## 7. Staged roadmap and effort

| Phase | Content | Rough effort |
|---|---|---|
| 0 (this work) | Probes, bench numbers, this document | done |
| 1 — Tier 1 | `VsaSession`, capture-analyse + streaming spectrum, Tier-1 measurements, `VSAD`, constellation/measurement panel, mode-private settings, tests | 2–3 dev-days |
| 2 — Tier 2 | `digital.py` promoted and vectorised, preamble/pilot or manual rotation for the phase ambiguity, EVM/SER/symbol table, degradation tests; capture-analyse only | 5–8 dev-days |
| 3 — Process | §6 step 1 (crash path), then step 2 (SDK child process) | 1–2 dev-days, then 3–5 |

Effort is an estimate from the measured work, not a commitment **[inferred]**.

## 8. Risks and blockers

* **[measured]** Vendor library instability on the recovery path (§3.3, §6) —
  the only measured blocker for a *persistent* VSA.
* **[measured]** An IQS reconfiguration can wedge the stream into a permanent
  `−9` streak (2 of 6 rate changes in multi-rate soaks); recovery must be reused
  from the SDR session, not reinvented.
* **[measured]** Streaming Tier 2 is 5–12× real time in NumPy; the design must be
  capture-and-analyse, or the PLL must be vectorised.
* **[measured]** Blind rate estimation fails near Nyquist (need sps ≥ 4).
* **[measured]** 4-fold carrier phase ambiguity without a preamble.
* **[measured]** Accuracy is bounded by the source on this bench: the tinySA has
  no traceable absolute calibration, so "within 0.4 dB" is path consistency, not
  lab accuracy.
* **[measured]** The centre bin is drive-dependent (§3.2): a VSA that averages
  the DC region into a measurement will see 0.2–1.6 dB of coherent leakage.
* **[unverified]** Long-run stability, non-`Bus` trigger sources, `DSP_DDC`-based
  VSA capture, and Tier 2 on a *real* modulated signal (no PSK/QAM source exists
  here).
* **[risk]** `RefLevel_dBm` doubles as the input gain: a VSA that exposes it as a
  display reference will silently compress strong signals (measured, §3.2).

## 9. Verification and evidence index

Reproduce everything:

```bash
python3 tools/vsa_probe/probe_siggen_selfcheck.py     # generator + theory
python3 tools/vsa_probe/probe_dsp_loopback.py         # Tier1/Tier2 offline
python3 tools/vsa_probe/probe_iq_capture.py all       # SAN-90 required
python3 tools/vsa_probe/probe_iq_level.py all         # SAN-90 + tinySA required
```

Raw outputs: `tools/vsa_probe/*.json`, tables in
[tools/vsa_probe/FINDINGS.md](../../tools/vsa_probe/FINDINGS.md).
Bilingual structure is enforced by `python3 tools/check_docs_parity.py`.
