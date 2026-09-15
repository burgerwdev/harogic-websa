# SAN-90 VSA probe findings — measured, with bench conditions

All numbers below were measured on the live bench. Device: SAN-90
(`UID 5230500C00380025`, Model 67, HW 8, MFW/FFW 14184) over USB.
Known source: tinySA Ultra ZS407 (`/dev/ttyACM0`), CW, directly connected by
SMA (no external attenuator, no antenna path) unless a row says otherwise.
WebSA was stopped for every run (`tools/vsa_probe/harness.py` does this and
restores it afterwards).

Source-level accuracy is *not* traceable to a lab standard: the tinySA's own
level accuracy is a few tenths of a dB at best and its absolute calibration is
unknown. Every "agreement" statement below is therefore bounded by that, and
by the vendor swept trace's own spread (quoted where used).

Probes: `probe_iq_capture.py`, `probe_iq_level.py`, `probe_siggen_selfcheck.py`.
Raw outputs: `iq_capture_*.json`, `iq_level_*.json`, `siggen_selfcheck.json`
(printed summaries are also kept in the run logs).

## 1. FixedPoints frame capture (the capture-then-analyse path)

`probe_iq_capture.py framed --decimate 8 --trigger-length 262144 --repeat 5`
(centre 100 MHz, RefLevel 0 dBm, tone 100.200 MHz at −25 dBm)

| Run | samples | packets ok/err | trigger | elapsed | peak Hz | peak dBm |
|---|---|---|---|---|---|---|
| 0 | 262144 | 17 / 0 | 0.1 ms | 34.7 ms | 200001 | −24.49 |
| 1–4 | 262144 | 17 / 0 | – | – | 200002 | −24.61 … −24.66 |

* shortfall **0** samples on every run; packet sizes 16240 (×16) + partial last.
* peak frequency **200002.0 Hz ± 0.47 Hz** (stdev over 5 runs), i.e. **+2 Hz**
  from the commanded 200 kHz offset — better than the tinySA's own setting
  accuracy, and repeatable.
* peak power **−24.61 dBm ± 0.063 dB** for a −25 dBm source (+0.39 dB).
* capture overhead: 262144 samples = 33.5 ms of signal at 7.8125 MSPS, whole
  frame in 34.7 ms → **≈1.2 ms fixed overhead**.
* multi-packet reassembly is clean: the CW phase increment across the frame has
  a packet-boundary residual of **0.0098 rad max** versus **0.0225 rad max** in
  the interior — no tear at the joins.

## 2. Trigger semantics

* `IQS_BusTriggerStart` starts the frame immediately: latency **0.1 ms**.
* Without `IQS_BusTriggerStart` the first `IQS_GetIQStream_PM1` returns
  **−10 (BusTimeOut)** after the configured `BusTimeout_ms`
  (measured 0.504 s for 500 ms) — a timeout is available as a "no signal"
  outcome, it is not a silent hang.
* Not measured: `IQS_TriggerSource` other than `Bus` (Level / External /
  GNSS1PPS / Timer / SpectrumMask) — untested for VSA use.

## 3. Reachable FixedPoints depth

`probe_iq_capture.py depth --decimates 4,16,64`

| Decimate | Rate | 2^14 | 2^16 | 2^18 | 2^20 | 2^22 | 2^24 |
|---|---|---|---|---|---|---|---|
| 4 | 15.625 MSPS | ok | ok | ok | ok | ok | ok |
| 16 | 3.906 MSPS | ok | ok | ok | ok | ok | ok |
| 64 | 0.9766 MSPS | ok | ok | ok | ok | ok | ok |

"ok" = status 0, all requested samples returned, 0 packet errors. At the largest
frame (16 777 216 samples = 128 MB of int16 IQ) the transfer took 1.96 s (dec 4),
5.05 s (dec 16), 17.93 s (dec 64) — i.e. the deep capture is **roughly
real-time**, not instant:

| Decimate | 2^24 frame = signal time | transfer time | ratio |
|---|---|---|---|
| 4 | 1.074 s | 1.962 s | 1.83× |
| 16 | 4.295 s | 5.048 s | 1.18× |
| 64 | 17.18 s | 17.93 s | 1.04× |

Depths above 2^24 were not tested (not a device limit that was hit — the probe
stopped there).

## 4. Adaptive continuous stream (the streaming path)

`probe_iq_capture.py stream --decimates 4,16,64 --seconds 5`

| Decimate | fs | effective | loss | USB | packets | errors | gap max |
|---|---|---|---|---|---|---|---|
| 4 | 15.625 MSPS | 15.6227 | 145 ppm | 62.5 MB/s | 4810 | 0 | 3.03 ms |
| 16 | 3.9062 MSPS | 3.9057 | 147 ppm | 15.6 MB/s | 1203 | 0 | 4.94 ms |
| 64 | 0.9766 MSPS | 0.9763 | 300 ppm | 3.9 MB/s | 301 | 0 | 17.3 ms |

The gap maxima equal one packet period (16240 samples at that rate), i.e. normal
pacing rather than stalls; the small rate deficit is the measurement window's
own overhead.

`probe_iq_capture.py soak --decimates 4,16,64 --soak-seconds 40` (sustained run,
per-10-s window rate ratio, CPU = this process over wall time)

| Decimate | packets ok | errors | worst 10 s window | rate deficit | CPU |
|---|---|---|---|---|---|
| 4 | 38485 | 0 | 0.99988 | 0.0014 % | 18.0 % |
| 16 | 9622 | 0 | 0.99968 | ~0 % | 5.1 % |
| 64 | 2406 | 0 | 0.99848 | ~0 % | 1.5 % |

* The raw IQS path (fetch + NumPy copy) costs **1.5–18 % of one core** depending
  on rate; no DSP is attached in this measurement.
* **Post-configuration drain is mandatory**: without discarding ~0.25 s of
  queued packets after `IQS_Configuration`, a decimate change produced 100 %
  fetch failures (measured: 10824/10824 errors when switching 4 → 16). With the
  drain, 0 errors. The production SDR session already does this; a VSA session
  must too.

## 5. Level / power calibration

`probe_iq_level.py level --levels -30,-25,-20,-15 --ref-levels 0,-30`
(decimate 16, 262144 samples, tone 200 kHz above the IQS centre)

IQS `RefLevel_dBm` = **0**:

| tinySA | IQ mean | IQ coherent tone | ScaleToV |
|---|---|---|---|
| −30 | −29.61 | −29.60 | 2.287e−05 |
| −25 | −24.73 | −24.70 | 2.302e−05 |
| −20 | −19.92 | −19.87 | 2.293e−05 |
| −15 | −18.85 | −18.83 | 2.307e−05 |

IQS `RefLevel_dBm` = **−30**:

| tinySA | IQ mean | IQ coherent tone | ScaleToV |
|---|---|---|---|
| −30 | −29.89 | −29.85 | 6.776e−07 |
| −25 | −25.00 | −24.88 | 6.761e−07 |
| −20 | −26.39 | −31.76 | 6.805e−07 |
| −15 | −27.56 | −36.53 | 6.794e−07 |

Findings:

* **Absolute accuracy**: with a linear front end the IQ path reports the source
  level within **0.4 dB** (RefLevel 0: −29.61/−24.73/−19.92 against
  −30/−25/−20). The coherent tone estimate and the block mean agree within
  **0.03–0.14 dB**, so `10·log10(mean|v|²/50) + 30` with the vendor
  `IQS_ScaleToV` is the correct conversion — no extra 3 dB bandpass factor.
* **`ScaleToV` tracks the front-end gain**: setting RefLevel 0 → −30 changes
  ScaleToV by 2.287e−05 / 6.776e−07 = **33.7× = 30.6 dB**, matching the 30 dB
  change. The volts are therefore absolute across RefLevel settings.
* **Headroom depends on RefLevel**: at RefLevel 0 dBm the response is linear to
  about **−19 dBm** input and compresses above it (−15 dBm reads −18.85,
  i.e. 3.9 dB low). At RefLevel −30 dBm the linear range ends at about
  **−25 dBm** (−20 reads −26.39, 6.4 dB low; −15 reads −27.56). Compression is
  visible twice over: the block mean falls short *and* the coherent tone
  collapses further (−36.5 at −15), which is the signature of gain compression
  rather than a mere level offset.
* Consequence for a VSA: `RefLevel_dBm` is not a display setting on the IQS
  path, it is the input attenuator/gain. A VSA must pick it per signal level or
  the constellation will be silently compressed.

### Vendor swept cross-check

Same tone through production code (`HarogicDevice` + `StdSession`, span 10 MHz,
RBW auto = 10 kHz, 1001 points, pos-peak, 12 traces per point):

| tinySA | swept peak mean | stdev | IQ mean − swept (RefLevel 0) |
|---|---|---|---|
| −30 | −27.69 | 4.59 dB | −1.92 |
| −25 | −26.61 | 2.15 dB | +1.87 |
| −20 | −21.72 | 2.11 dB | +1.79 |
| −15 | −19.74 | 0.47 dB | +0.89 |

**Agreement within ~2 dB**, with the caveat that the vendor swept trace's own
peak spread is ±2–4.6 dB (bin alignment of a CW tone at 10 kHz RBW) — that
spread, not the IQ path, bounds this cross-check. Two traps were found while
measuring this and are worth keeping in the documentation:

* a narrow span (200 kHz) with the default minimum sweep time gives an
  **uncalibrated** trace (peak stuck at ≈−19.5 dBm for every source level);
  the standard display configuration (10 MHz span, RBW auto) tracks correctly.
* `state.swp.*` fields are proxied flat onto `state`; using a wrong name (e.g.
  `state.rbw` instead of `state.rbw_hz`) silently keeps the default 100 kHz RBW,
  which is wider than a narrow span.

## 6. DC and image behaviour

`probe_iq_level.py dcimage` (decimate 16, 262144 samples, RefLevel 0, QDC off)

Tone level versus its offset from the IQS centre (the centre is moved, the tone
stays at 100.200 MHz):

| Offset | DCC high-pass, tone | DCC off, tone |
|---|---|---|
| 0 Hz | −24.55 | −24.66 |
| 500 Hz | −24.41 | −24.40 |
| 1 kHz | −24.69 | −24.69 |
| 3 kHz | −24.52 | −24.51 |
| 10 kHz | −24.64 | −24.63 |
| 30 kHz | −24.57 | −24.56 |
| 100 kHz | −24.59 | −24.57 |

* Total spread **0.29 dB**; at DC the tone is **0.1–0.25 dB** below its level at
  ±500 Hz. So on this firmware the DC canceller is essentially transparent to a
  signal at the centre — there is **no measured DC hole** for a VSA to work
  around (measured with a clean injected CW; the earlier SDR finding about a
  weak broadcast signal and `DCCOff` is a different case and is not contradicted).

Image rejection (tone at +offset, spur measured at −offset, same capture):

| Offset | DCC high-pass | DCC off |
|---|---|---|
| 100 kHz | 77.8 dB | 79.9 dB |
| 300 kHz | 86.4 dB | 88.3 dB |
| 700 kHz | 86.5 dB | 87.8 dB |
| 1.5 MHz | 82.0 dB | 85.1 dB |

**≥78 dB image rejection** at every offset tested — an image is not a limiting
factor for EVM. QDC was left off (the production default); the effect of
`QDCAutoMode` on image rejection was not re-measured here.

## 7. Device-link robustness (measured, fix deferred by the owner)

* Physical removal of the analyzer, then a worker recovery attempt, produced a
  **SIGSEGV inside `libhtraapi` `Device_Close`** on a stale handle
  (`/tmp/websa.log`, `web_sa/hardware/device.py:127`), and the WebSA worker then
  crash-looped (`exited with status -11; restarting`). The Python side cannot
  catch this; it needs either process isolation or not closing a dead handle.
* While the analyzer was absent, `Device_Open` returned **−1** on 5/5 attempts
  until the unit was power-cycled; the analyzer was also gone from `lsusb`.
* A *second probe process* holding the device makes `Device_Open` return −1 as
  well. The probe harness now refuses to start when another VSA probe is live
  (`harness.device_probe_peers`), and retries the open four times.

Owner decision: recorded, to be fixed as a separate change (not part of the VSA
analysis).

## 8. Not measured / unverified

* `IQS_TriggerSource` other than `Bus` (Level / External / GNSS1PPS / Timer /
  SpectrumMask) for triggered VSA captures.
* `TriggerLength` above 2^24 samples, and multi-frame back-to-back capture
  (trigger-to-trigger gap) — the VSA "repeat" case.
* Capture through `DSP_DDC` (these probes read the raw IQS stream).
* Long-term stability beyond 40 s, and behaviour across many hours or
  temperature drift.
* Effect of `QDCAutoMode` on image rejection; `DCCManualOffsetMode`.
* Absolute traceability: no calibrated signal generator was available, so all
  level statements are relative to the tinySA's own setting.

## 9. Synthetic IQ generator (offline reference)

`probe_siggen_selfcheck.py` — proves the synthetic source can be used as the
reference for every DSP claim (no hardware involved).

* constellation mean power 1.000 (max deviation 2.2e-16).
* noiseless matched-filter loopback: EVM 0.0127–0.0132 % — this is the RRC
  truncation floor of the generator, and it is measurable:
  span 4 → 1.62 %, span 10 → 0.290 %, span 20 → 0.0133 %, span 64 → 0.0072 %.
  All probes therefore use span 20.
* AWGN: realized SNR within 0.037 dB of the request; measured EVM against the
  matched-filter bound `sqrt(N0/Es) = 1/sqrt(sps*SNR)` gives a ratio of
  0.985–1.009 over 10–30 dB at sps = 8.
* injected timing offsets 0/0.25/0.5/0.75 samples are recovered to ≤0.5 sample
  at integer timing resolution (EVM 0.013 % at 0, 4.16 % at 0.25/0.75, 8.34 % at
  0.5 samples — the measured EVM cost of sub-sample timing error at sps 8).
* injected carrier offsets are recovered exactly: 0.001/0.005/0.020 cycles/sample
  estimated with **0 error at 6 decimal places**, residual EVM after correction
  0.013–0.19 %.

## 10. Tier 1 measurements and Tier 2 demodulation (offline loopback)

`probe_dsp_loopback.py`, synthetic QPSK/16QAM at 3.906 MSPS unless stated,
244.125 kSym/s, sps 16, roll-off 0.35, 25 dB SNR where a level is implied.

### Tier 1

* burst duty cycle **0.500 measured vs 0.500 expected**.
* noise-only CCDF follows the Rayleigh reference with **0.0021** maximum absolute
  deviation (single samples; a block-averaged CCDF is steeper and must not be
  compared against that curve).
* blind symbol-rate estimate **−0.03 Hz** error at 244.125 kSym/s.
* carrier offset from the spectrum centroid: bias −735…−778 Hz **independent of
  SNR (10–40 dB)**, ~640 Hz spread across noise realisations — finite-data PSD
  asymmetry, not noise. Coarse indicator only.
* cost over a 33.7 ms capture, as a fraction of real time: power-vs-time 22 %,
  CCDF 3 %, Welch spectrum (4096) 25 %, spectrogram (256/128) 120 %,
  Tier-1 constellation 316 %.

### Tier 2 — EVM against the matched-filter bound

| Kind | 8 dB | 12 dB | 16 dB | 20 dB | 25 dB | 30 dB |
|---|---|---|---|---|---|---|
| QPSK measured % | 9.986 | 6.314 | 3.991 | 2.521 | 1.419 | 0.799 |
| 16QAM measured % | 10.181 | 6.411 | 4.040 | 2.547 | 1.432 | 0.805 |
| theory % | 9.953 | 6.280 | 3.962 | 2.500 | 1.406 | 0.791 |

EVM/theory 1.003–1.023, SER 0 at every point.

### Tier 2 — impairments

| Impairment | Value | Estimate | EVM % | SER |
|---|---|---|---|---|
| carrier | 0 Hz | 0 Hz | 1.419 | 0 |
| carrier | 122.1 Hz | 122.1 Hz | 1.535 | 0 |
| carrier | 488.2 Hz | 488.2 Hz | 2.846 | 0 |
| carrier | 2441.2 Hz (1 % of R) | 2441.3 Hz | 3.388 | 0 |
| carrier | 12206.2 Hz (5 % of R) | 12206.3 Hz | 1.833 | 0 |
| timing | 0 / 0.25 / 0.5 / 0.75 samples | −0.02 / 0.22 / 0.47 / 0.72 | 1.419–1.437 | 0 |
| roll-off | 0.1 / 0.25 / 0.35 / 0.5 / 0.9 | rate error <0.01 % | 1.585 / 1.447 / 1.432 / 1.421 / 1.411 | 0 |

### Tier 2 — symbol rate and cost

| Symbol rate | sps | rate estimate | EVM % | theory % | CPU % of real time |
|---|---|---|---|---|---|
| 61.035 kSym/s | 16 | 61.035 k | 1.464 | 1.406 | 128 |
| 244.125 kSym/s | 16 | 244.141 k | 1.464 | 1.406 | 546 |
| 976.5625 kSym/s | 8 | 976.566 k | 2.050 | 1.988 | 1176 |
| 976.5625 kSym/s | 4 | 976.563 k | 3.111 | 2.812 | 826 |
| 244.125 kSym/s | 8 | 244.141 k | 2.050 | 1.988 | 343 |

* **2 samples/symbol fails**: with the symbol rate at Nyquist the |x|^2 line is
  outside the search band and the estimate collapses (350 kSym/s reported for a
  1.95 MSym/s signal) — the chain needs **sps >= 4**.
* **A 4-fold phase ambiguity is inherent** to the M-th power estimator (measured
  rotations of 0/90/180/270 degrees appear across the test set). EVM is unaffected
  (the LS gain absorbs a global rotation); the symbol *table* is wrong by a
  quadrant. Resolving it needs a preamble/pilot or a user-controlled rotation.
* **Four defects found and fixed while building the chain**, kept here because
  each one silently inflated EVM: the matched filter's transient and zero tail
  (9.9 %), unnormalised slicing scale, a wrong assumed symbol rate making the
  resampler stretch the time base (9.6 % — the demodulator resamples to a nominal
  rate, so the probe's `FS/SPS` must equal the generator's), and an inverted LS
  gain in the EVM helper (`vdot` conjugates its first argument; 200 % EVM).

## 11. Reconfiguration hazards and probe correctness (measured)

### 11.1 The IQS stream can wedge on a decimate change

Same probe, same bench, three soak runs (`--soak-seconds 40/60`, decimates
4/16/64 in one process):

| Run | dec 4 | dec 16 | dec 64 |
|---|---|---|---|
| A (40 s each) | 0 err | 0 err | 0 err |
| B (60 s each) | 0 err | 0 err | **3608/3608 err (-9)** |
| C (60 s, dec 64 alone) | – | – | 0 err |
| D (40 s each) | 0 err | **9622/9622 err (-9)** | 0 err |

So **2 of 6 reconfigurations in multi-rate runs left the stream returning
`-9 (BusDataError)` on every fetch for the whole window** — never on the first
rate, and never when the rate ran alone. The settle drain is not enough on its
own. The production SDR session already treats `-9` as transient and, after a
persistent streak, does a checked stop/config/start recovery; these probes have
no recovery, which is why the whole window fails. A VSA session must reuse that
recovery, and the capture path needs the same protection.

### 11.2 `IQStream_TypeDef` must be the production one

`web_sa/hardware/sdk_bindings.py` re-declares `IQStream_TypeDef` at the
header-correct **728** bytes (the vendor wrapper's copy is 720, and
`IQS_GetIQStream_PM1` writes 8 bytes past it on *every* packet — the heap
corruption documented there) and rebinds the entry point to that struct. The
probes now import `sdk_bindings` and use its struct
(`tools/vsa_probe/capture.py: new_stream()`): with the wrapper's struct the call
fails the ctypes type check as soon as `sdk_bindings` has been imported in the
same process, and without it the probes were exercising a *different* (shorter)
struct than production on thousands of packets.

### 11.3 Source repeatability

The tinySA's frequency setting repeats to roughly ±20 Hz between sessions: the
same commanded 100.200 MHz tone measured 199 985 Hz and 200 002 Hz (relative to
the IQS centre) in two runs, with the IQ path's own repeatability at ±0.5 Hz and
±0.07 dB across captures inside one run. Source level repeatability is ~0.1 dB.
Every "error" figure quoted for a hardware frequency or level is bounded by this.

## 12. Repeat-run summary (final verification)

Every probe was re-run end to end with the corrected `IQStream_TypeDef`; the
summaries are the logs and the JSON next to this file. Headline reproducibility:

| Probe | Reference run | Repeat run |
|---|---|---|
| `probe_siggen_selfcheck.py` | passes | passes (noiseless floor 0.0132 %, AWGN EVM/theory 0.985–1.009) |
| `probe_dsp_loopback.py` | EVM/theory 1.003–1.023 | EVM/theory 1.003–1.023, Rayleigh dev 0.0021, rate error −0.03 Hz |
| `probe_iq_capture.py framed` | 262 144 pts, 17 pkts, 0 err | 262 144 pts, 17 pkts, 0 err, peak −24.48 dBm |
| `probe_iq_capture.py depth` | 2^24 pts at dec 4/16/64 | identical (0 err, 1.79/5.02/17.90 s) |
| `probe_iq_capture.py stream` | 0 err, −18…300 ppm | 0 err, −46…81 ppm |
| `probe_iq_capture.py soak` | 0 err (run A) | **−9 wedge** on one rate (runs B and D, §11.1) |
| `probe_iq_level.py level` | 0.21–0.4 dB absolute | 0.21 dB mean offset, same compression points |
| `probe_iq_level.py dcimage` | 0.29 dB DC spread at −25 dBm | 1.2–1.6 dB at −15 dBm, image 81–92 dB |

The level, depth, framing and DSP results reproduce; the soak's `−9` wedge is the
one intermittent behaviour, and it is the reason a VSA session must reuse the
production transient-streak recovery.

## 13. The device-link crash, fixed and re-verified

The crash analysed in §7/§11.2 was fixed and shipped in v1.7.4
(`web_sa/hardware/device.py`, `web_sa/supervisor.py`):

* `HarogicDevice.close()` calls `DSP_Close`/`Device_Close` **only** while
  `_handle_ok` is true, i.e. only for a handle from a successful `Device_Open`;
  `mark_link_lost()` and a failed `open()` clear it. The handle is still dropped
  and `connected` still goes false, so no SDK call sees a stale pointer.
* `supervisor` stops with a critical message after `FAST_EXIT_LIMIT` (5)
  consecutive exits that each ran under `FAST_EXIT_S` (10 s), instead of
  restarting forever and writing one core dump per attempt.

Verification on the bench (deterministic, no cable pulling: a helper process holds
the device so `Device_Open` returns −1 — the exact condition that crashed it):

| Step | Before the fix | After the fix |
|---|---|---|
| worker starts while the device is held | SIGSEGV in `Device_Close`, exit −11, restart loop, 7 cores in <1 min | stays up, `WARNING device still unreachable: Device_Open status=-1`, **0 new cores** |
| device released | (never got there — worker kept crashing) | `INFO device link restored; std mode resumed`, `connected: true`, same worker PID |
| `./stop.sh` while connected | OK | OK, 0 new cores |
| `./stop.sh` while the link is lost | `Device_Close` on the dead handle from `cleanup_background` (same crash) | exit 0, 0 new cores |

Core count was 72 before and after every step; `coredumpctl` shows no new entries.
Unit tests pin the new behaviour (`tests/test_link_recovery.py`:
`test_close_skips_device_close_once_the_link_is_lost`,
`test_failed_open_leaves_no_handle_to_close`, `test_close_releases_a_live_handle`,
`test_reopen_does_not_close_a_dead_handle`; `tests/test_supervisor.py`: crash-loop
give-up and counter reset).

## 14. FixedPoints capture recipe and the service transfer budget (measured while implementing)

Found while making `SET_MODE 'vsa'` work (task-3 of the roadmap): the production session
first drained the stream through the settle window like SDR does, and every frame came back
as `-10` forever. Each recipe was run three times, interleaved, on the bench
(depth 131072, decimate 16, `BusTimeout_ms` 5000):

| Recipe | Result |
|---|---|
| read packet after packet right after `IQS_BusTriggerStart` | **3/3 full frames**, 131072/131072 samples, all statuses 0, 0.05–0.06 s |
| wait `TriggerLength / IQSampleRate` (33 ms) and then read | **3/3 full frames**, 0.11 s |
| one read immediately, then wait 33 ms, then read the rest | **3/3 lost exactly one packet** (114832/131072) and spent 20 s in `-10` |
| read inside the settle window (the SDR drain) | **3/3 destroyed the frame**: 0 samples, `-10` on every later read for 65 s |

So a fixed frame must be read straight through with nothing reading before the trigger; the
`IQS_BusTriggerStart` after `IQS_Configuration` is the flush that makes the ~0.25 s
post-configuration drain unnecessary here. The session implements exactly that
(`measurements/vsa.py`: the capture path skips the settle drain, the stream path keeps it).

Deep frames with the chosen recipe, one packet per step and a 2 ms pause between steps
(what the session's step loop does), all with 0 packet errors and the planned packet count:

| Depth | Decimate | IQ rate | Signal | Transfer | Ratio |
|---|---|---|---|---|---|
| 2^20 | 16 | 3.906 MS/s | 0.27 s | 0.27 s | 1.01 |
| 2^22 | 16 | 3.906 MS/s | 1.07 s | 1.08 s | 1.00 |
| 2^24 | 16 | 3.906 MS/s | 4.29 s | 4.30 s | 1.00 |
| 2^20 | 4 | 15.625 MS/s | 0.07 s | 0.17 s | 2.50 |

The last row is the USB-bandwidth bound, not a capture problem: 4 MB at ~24 MB/s. Through
the running service the same frames cost 1.92x (131072 samples) and 1.22x (2^24 samples) of
their signal duration, the extra being the settle/arm overhead that a short frame feels
most (`tools/vsa_probe/vsa_service_check.py` writes the record).

### 14.1 The VSA data frame and the client path (measured while implementing task-5)

* **Both frames reach a client per capture.** A constellation capture at depth 2^17 and
  decimate 16 delivers 6 RTAF + 6 VSAD frames in 3 s to the display WebSocket
  (`tools/vsa_probe/vsa_frame_check.py`), and the newest VSAD decodes with NumPy alone:
  cloud (4096, 2), ideal grid (4, 2), symbol rate/CFO/timing in the head and the
  measurement block by name.
* **Latest-wins is per frame type.** RTAF and VSAD are published together, so with one
  shared latest-wins slot one of the pair would always be dropped; each type now keeps its
  own newest frame and they drain in first-inserted order (pinned by
  `tests/test_client_stream.py`).
* **A CW carrier has no symbol rate.** The blind Oerder-Meyr estimate on the CW test
  source returned 554.95 kHz (and a 6.8-sample timing) — an arbitrary answer, because a CW
  tone has no symbol line. Nothing in Tier 1 may present that as a measurement: the
  decision-directed chain (Phase 2) has to reject it.
* **Level, re-confirmed through the frame path**: tinySA CW −25.0 dBm at 120 MHz, decimate
  16, `RefLevel 0` → capture mean **−25.0007 dBm**.
* **tinySA bench note**: `output off` does *not* stop the generator on this firmware
  (`mode low output` keeps radiating); switch back with `mode input`. Measured: 100.2 MHz
  stayed at −25.3 dBm across `pause`/`modulation off`/`output off` and dropped to
  −73.4 dBm only after `mode input`.

### 14.2 Tier 2 in production: the vectorised chain (measured while implementing task-6)

The probe chain's per-symbol PLL was the reported cost driver; the production chain replaces
it with a decision-directed phase/frequency **line fit** (three vectorised passes, outliers
rejected) and adds a decision-aided timing stage. Measured on synthetic QPSK/16-QAM
(3.906 MSPS, sps 16, 4096 symbols, 2.5 kHz carrier offset, 0.4 samples timing):

| Quantity | Measured |
|---|---|
| EVM / matched-filter bound | median 0.99 over 8–30 dB (no systematic bias); individual realisations 0.989–1.007 except one 8 dB case at 1.147, which is the estimator's own variance there |
| SER / BER, 8–30 dB | 0 (preamble resolves the 4-fold rotation: exactly one of the four candidates reaches SER 0) |
| Timing error | ≤ 0.026 sample over 8–30 dB, 0.001 at 30 dB. The blind `|x|^2` stage alone needs 30 dB to reach 0.05 (0.31 samples mean at 8 dB) |
| Carrier offset (total) | ≤ 0.03 Hz |
| Tracker cost | 2.34 ms for 4076 symbols, 50.5 ms for 65515 — **10× faster** than the probe's per-symbol PLL (25.4 ms / 494 ms) |
| Whole chain | 169 ms for a 2^17-sample capture (503 % of real time), 3.76 s for 2^20 (1402 %); the FFT stages and the two matched filters dominate, not the tracker |
| Noiseless EVM floor | 0.017 % (span-20 matcher; the truncation floor is 0.013 %) |

Two implementation lessons worth keeping:

* **The timing stage must run after the carrier is removed.** With a few kHz of offset the
  constellation rotates tens of times across a block, so any decision-directed timing
  statistic averages to nothing (measured: the eye-opening objective collapsed).
* **Minimise the EVM, not the eye opening.** The matched-filter eye is very flat near the
  optimum (measured differences of 0.01 % between 0.64 and 0.75 samples at 30 dB), so a
  parabola fitted to it is noise-dominated; the decision-directed EVM is sharply curved
  (4 % at 0.25 samples, 8 % at 0.5) and pins the phase to 0.001 samples at 30 dB.


## 15. Production reconciliation (final run, task-8)

Same bench, same source (tinySA CW, 100.2 MHz, −25.0 dBm into the analyzer), `RefLevel 0`,
decimate 16. The left column is what the probe suite measured while the feature was only a
feasibility study; the right column is what the shipped feature measures through the running
service (`tools/vsa_probe/vsa_service_check.py`, `vsa_frame_check.py`, `tools/command_sweep.py`).

| Quantity | Probe (analysis) | Production (feature) |
|---|---|---|
| Level of a −25 dBm CW tone | within 0.4 dB; −24.5 dBm at the centre | mean **−25.23 dBm**, tone level **−25.2 dBm** (0.2 dB from the source) |
| Peak bin vs tone level | — (the probe used the coherent estimator) | bin peak −28.25 dBm, i.e. the 2.00-bin window ENBW (3.0 dB) below the tone, reported as a separate field |
| Noise floor | — | −138.9 dBm/Hz (median density) |
| Duty of a carrier | — | 1.0 (a trace with no two levels is continuous) |
| Deep capture | 2^24 samples, 0 packet errors, 1.04–1.83× signal duration | **16777216/16777216 samples, 0 errors, 1034/1034 planned packets, 1.41×** |
| Short capture (2^18) | — | 262144/262144 samples, 0 errors, 17/17 packets, 2.28× (the fixed settle/arm cost dominates a 67 ms frame) |
| Streaming Tier 1 | 40 s soak, 0 errors, worst 10 s window 0.99848 | stream view refreshes through the shared panadapter, `busy` false, frames flowing; soak-length stability not re-run |
| Frame path | — | RTAF + VSAD reach the display WebSocket; the newest VSAD decodes with NumPy alone; a 2 s stall still leaves only the newest cloud |
| Tier 2 EVM vs the matched-filter bound | 1.003–1.023 over 8–30 dB (probe chain) | median 0.99 over 8–30 dB (no bias); one 8 dB realisation reads 1.15, which is the estimator's own variance there |
| Tier 2 timing | ≤0.03 samples at 25 dB | **≤0.026 samples over 8–30 dB**, 0.001 at 30 dB |
| Tier 2 carrier | 0.1 Hz at 1 % of the symbol rate | total (M-th power + tracker) ≤0.03 Hz |
| Tier 2 cost | 1.3–12× real time, "the per-symbol loop dominates" | 5.0× at 2^17, 14× at 2^20; the vectorised tracker is 10× faster than that loop and no longer dominates (the FFTs and the matched filters do) |
| Blind rate on a carrier | (not applicable: a CW tone has no symbol line) | an arbitrary 554.95 kHz, reported as a blind estimate, with `sps_too_low`/`no_symbol_line` the explicit refusals |
| Command registry | — | 33/33 commands swept, every guard rail held (`SET_FREQ` in VSA → `vsa_unsupported`) |

**Still unverified** (unchanged by this work, and listed in the roadmap section 6): Tier 2 on a
real modulated signal (the bench has no PSK/QAM source, so every symbol-level claim is
synthetic), `IQS_TriggerSource` other than `Bus`, `TriggerLength` above 2^24, capture through
`DSP_DDC`, hour-scale stability, `QDCAutoMode`'s effect, and the frontend against a real device
(the UI smoke runs against the fake backend, by design).
