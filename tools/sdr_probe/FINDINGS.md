# SAN-90 SDR feasibility — bench findings

All results below were measured on the live hardware (SAN-90 Model 67, API 0.55.89,
tinySA Ultra+ ZS407 as the known signal source). WebSA was stopped for every
exclusive run. Probe scripts: this directory.

## 1. Device capabilities (`probe_iqs_caps.py`, `Device_GetHardwareState`)

| Field | Value |
|---|---|
| Model | 67 (SAN-90), 9 kHz – 9 GHz |
| GNSS | present (`GNSSType=1`) |
| `SignalSourceEn` | **0** (no internal RF source) |
| `ADC_VariableRateEn` | **0** (fixed ADC rate) |
| Native IQ | **62.5 MSPS complex, 50 MHz bandwidth** (DecimateFactor = 1) |

## 2. IQS sample rates (`probe_iqs_caps.py`)

`DecimateFactor` is **power-of-two only**; > 2048 clamps to 2048.

| Decimate | IQSampleRate | Bandwidth |
|---|---|---|
| 1 | 62.5 MSPS | 50 MHz |
| 2 | 31.25 MSPS | 25 MHz |
| 4 | 15.625 MSPS | 12.5 MHz |
| 8 | 7.8125 MSPS | 6.25 MHz |
| 16 | 3.90625 MSPS | 3.125 MHz |
| 32 | 1.953125 MSPS | 1.5625 MHz |
| 64 | 976.5625 kSPS | 781.25 kHz |
| 256 | 244.14 kSPS | 195.31 kHz |
| 1024 | 61.04 kSPS | 48.83 kHz |
| 2048 | 30.52 kSPS | 24.41 kHz |

`DataFormat` (8/16/32-bit) changes the per-packet sample count; the packet byte
size stays ~64960.

## 3. Continuous streaming (`probe_stream.py`)

**Adaptive** trigger mode gives true gapless streaming at the full IQ rate:

| Decimate | Rate | USB | gaps |
|---|---|---|---|
| 2 | 31.240 MSPS | 125 MB/s | none |
| 4 | 15.620 MSPS | 62 MB/s | none |
| 8 | 7.810 MSPS | 31 MB/s | none |
| 16 | 3.905 MSPS | 16 MB/s | none |
| 64 | 0.976 MSPS | 3.9 MB/s | none |

`FixedPoints` (trigger per frame) is **not** continuous.

## 4. DSP_DDC (`probe_ddc.py`, `probe_ddc_sweep.py`, `probe_ddc_diag.py`)

Verified with a +200 kHz CW tone (SAN center 100 MHz, input 3.90625 MSPS):

- **Output format is interleaved complex float32** (`DataFormat == 6`),
  `PacketDataSize = N*8`. Vendor `DSP_FFT` does **not** work on it (all-zero);
  NumPy FFT is correct.
- **Offset sign:** `f_out = f_in_rel + DDCOffsetFrequency`. To bring absolute
  frequency `ft` to DC set `DDCOffsetFrequency = center - ft` (tone at +200 kHz
  → offset `-200e3`).
- Proper low-pass decimator: passband `±(SampleRate/DecimateFactor)/2`. Verified
  offsets `-220k … -180k` pass, others rejected at dec=100.
- `SampleRate` input field = **input** IQ rate; `ProfileOut.SampleRate` = output
  rate. `ProfileOut.SamplePoints = ceil(input/decimate)`.
- `DSP_DDC_GetDelay`: 0 (dec 1), 60 (dec 8), 73 (dec 100/1000) output samples.
- Cost ≈ 1.6–2.2 ms per 131072 input samples (~5–7 % realtime), independent of
  decimation.

## 5. Vendor DSP_FFT (`probe_vendorfft.py`)

- Works on int16 IQS data when the stream metadata is intact (official pattern).
- A −25 dBm CW at 100.200 MHz → peak at 100200019.8 Hz, −25.16 dB, axis
  `center ± IQSampleRate/2`, points = FFT size.

## 6. ADM analog demod (`probe_adm.py`)

tinySA AM 1 kHz/50 %, FM 1 kHz/25 kHz dev, SAN center 100 MHz, 1.953 MSPS:

| Mode | Result |
|---|---|
| AM `ADM_AMDemod_PM1` | ModRate 994.6 Hz, waveform tone 998.4 Hz, Carrier −19.7 dBm, SINAD 22 dB, SNR 23 dB |
| FM `ADM_FMDemod_PM1` | ModRate 998.3 Hz, Deviation 22331 Hz, CarrierErr 21 Hz, SINAD 7 dB |
| DDC float → FM ADM | works (`DataFormat=Complexfloat`), ModRate 998 Hz |

Note: `ModDepth` is returned as a **fraction** (0.49 for 50 %), not percent.
`Deviation` is ~11 % below the tinySA setting.

## 7. DCC / QDC receive-path comparison

Using the connected antenna and the 101.7 MHz broadcast signal at 1.953125 MSPS:

- `DCCOff`, `DCCHighPassFilterMode`, and `DCCAutoOffsetMode` differed by less than
  0.1 dB in integrated power over the centre +/-100 kHz.
- Moving the station to +200 kHz showed no measurable image-rejection improvement from
  `QDCAutoMode` on this weak/noisy signal (about 4.2 dB apparent desired/image ratio in
  every mode), so that capture cannot justify changing the production default.
- IQS configuration time remained about 5 ms for every DCC/QDC combination.

Keep the bench-proven `DCCHighPassFilterMode + QDCOff` default. Although high-pass DCC
can attenuate signals very close to DC, on this SAN-90 firmware `DCCOff` produces an
invalid/noisy IQ path and loses the verified FM station; a future centre-channel mode
needs a firmware-specific validation rather than simply selecting DCCOff.

## 8. Not available

- Digital `Demod_*` API (PSK/QAM/FSK/ASK/GMSK): `Demod_Check() == -1` — missing
  `libDigitalSigDemod.so` + license. Must be implemented in-house if required.
- Internal signal source: none.

## 9. Implication for the SDR mode

The whole chain is feasible and cheap:
`IQS Adaptive stream → DSP_DDC channelizer (float) → [ADM AM/FM | own CW/SSB/FT8/WSPR
in NumPy] → audio + panadapter/waterfall from a NumPy FFT of the wideband IQ`.
A decimate of 16–64 (3.9–1 MSPS) is plenty for a live panadapter plus narrowband
demod, and DDC+RTTY/FT8 DSP cost is a few percent of realtime.

## 12. Capability + SWP limits on API 0.55.100 (`probe_caps_rbw.py`, 2026-10-12)

Device GetDecimateFactorList / GetBusBandwidth (new in 0.55.100):

| Field | Value |
|---|---|
| `decimate_factors` | 1, 2, 4 ... 2048 (pow2 only, matches the old table) |
| `bus_bandwidth` | 625,000,000 (625 MB/s, USB3) |
| MFW raw | 14189 = 0.55.109 (three-part decode verified on the real unit) |

SWP profile probe (Start/Stop 999.9-1000.1 MHz):

| Probe | Result |
|---|---|
| Manual RBW 1/3/10 Hz | **accepted, echoed exactly** (device clamps VBW to 10xRBW); the old 100 Hz floor was ours |
| TracePoints 4001/10001/100001 request | device derives its own count: 12160 (RBW 100) .. **14488** (RBW 1-10) |
| Frame Detector Sample/Average/MaxPower/RawFrames | all status 0 (accepted) |
| TraceType Clear/MaxHold/MinHold | all accepted |
| FFTExecutionStrategy Auto/FPGAOnly/CPUOnly-HighResOcc | all accepted; FPGAOnly lowers derived points to 2280 |
| RBW OnePercentSpan / OneThousandthSpan | RBW = 2000 / 200 on a 200 kHz span (exactly 1% / 0.1%) |
| EstimateMinSweepTime | reported 0.0 in every probe configuration (do not rely on it) |

Consequences applied in code: manual RBW floor 100 -> 1 Hz, points request clamp now
follows caps.points_max (16000), the display-side "actual points" stays whatever
SWP_TraceInfo reports. I3 (NativeIQSampleRate) stays dead on this unit:
ADC_VariableRateEn = 0 (section 1).

## 13. Aux DDC live verification (`probe_aux_live.py`, 2026-10-12)

Real service + real SAN-90 (SDR mode, center 100 MHz, decimate 32):

| Check | Result |
|---|---|
| SET_SDR_DDC ch2 @ 100.5 MHz | accepted, echoed in STATUS `req.sdr.aux` |
| IQAX frames on ?iqaux=1 (5 s) | 600 frames, centerHz == 100.5 MHz exactly |
| IQBF leaking to the aux socket | 0 |
| Aux freq outside the capture half-window (decimate 32 -> 976.56 kHz) | rejected with a clear error |

Gotchas found live:
* `htra_api.DeviceInfo_TypeDef` stops after FFWVersion - PMU/AGU ride in the struct's own
  trailing padding (size stays 24, no overflow, values unreachable). sdk_bindings restores
  the fields (offsets 20/22) and rebinds Device_Open to the restored BootInfo.
* Aux enable before the first SDR packet has no capture geometry: the step loop configures
  aux lazily once `fs_in` exists.

## 14. IQS Level trigger on N90 R0 @ 0.55.100 — never fires (bench fact)

With a known CW on the input (tinySA 100.2 MHz, -25 dBm) and an IQS profile of
TriggerSource=Level / TriggerMode=FixedPoints / TriggerLevel -90..0 dBm /
TriggerLevel_SafeTime=0.01 / BusTimeout_ms=2000: **IQS_Configuration accepts the
profile** (PacketCount/StreamSamples/IQSampleRate all echoed sanibly) but **every
IQS_GetIQStream_PM1 returns -10 BusTimeout** — the level event never arrives.
Caveat: an SWP readback taken in the same window failed to show the tone, so signal
presence was not independently confirmed during that measurement; rerun SWP + IQS
together before blaming the API. OPEN: needs vendor clarification on TriggerLevel
semantics/units (dBm vs internal scale) or model support on the N90.

## 15. Version display vs the official About panel (`probe_version_check.py`)


Real service + real unit, STATUS vs sa_ver.png: **every decoded field matches** -
N90 R0 / UID 5230500C00380025 / MCU 0.55.109 / FPGA 0.55.104 / PMU 0.1.15(271) /
AGU 0.1.5(261) / API 0.55.100. Bonus: the screenshot's `HCD 8` row is the
DeviceInfo.HardwareVersion field (u16, value 8 on this unit) - the last open
version-display question from findings/version-display.md is now closed.

## 16. SWP trace-point strategy: BinSizeAssined cannot mean "fixed bin size" (removed)

Probe: `probe_tracebin.py`, 2026-10-13. The service was stopped, RBWMode was Auto, and
TracePoints was 1000. The probe measured one full `SWP_GetFullSweep` for each row.
`EstimateMinSweepTime` is 0.0 in every row.

| Span | speed (SweepSpeedPreferred) | accuracy (PointsAccuracyPreferred) | bin_size (BinSizeAssined) |
|---|---|---|---|
| 10 MHz | 5.5 ms (2 hops) | 2.0 ms (8 hops) | **1.14 s** (7 hops) |
| 100 MHz | 5.2 ms (3) | - | **3.16 s** (18) |
| 1 GHz | 1.3 ms (23) | 1.7 ms (23) | **3.41 s** (22) |
| 9.02 GHz (full span) | 12.1 ms (196) | 50.9 ms (787) | **28.28 s** (250) |

`ProfileDeInit` seed: `TraceBinSize_Hz = -1.0` (sentinel), `TracePoints = 4000`,
`TracePointsStrategy = 0` (SweepSpeedPreferred), `TraceAlign = 0` (NativeAlign), RBW 250 kHz.

**The firmware ignores `TraceBinSize_Hz`.** A request of 10 kHz, 100 kHz, or 1 MHz on a 10 MHz
span returns an identical `SWP_TraceInfo` (`TraceBinBW_Hz` = 10010, `DecimateFactor` 32,
`FrameTime` 0.66 ms). The grid is always span/TracePoints. The field that names the strategy
never reaches the sweep.

The strategy changes the sweep pattern instead. It sweeps the requested Start/Stop exactly
(`StartFreq_Hz` = the request, `UserStartIndex` 0). Each point gets its own FFT (`AnalysisBW_Hz`
1.4 MHz at a 10 MHz span, against 23 MHz for speed). The device uses many hops. It does not use
the wide-band hop pattern that `DSP_InterceptSpectrum` crops back to the window. One sweep costs
about 1 s for each 10 MHz of span.

Consequence on the live service (real device, POWR frames/s):

  - 13.5 at 10 MHz
  - 1.5 at 100 MHz
  - 0.5 at 400 MHz
  - 0 at 1 GHz

`speed` gives 212 frames/s. At the full span one sweep takes 28 s. The acquisition watchdog stops
at 10 s, and it kills the worker. The worker holds `COMMAND_LOCK` at that moment, so STATUS stops
too. The whole page freezes until the supervisor restarts the worker. The log shows the same
event as `Acquisition timed out`.

Action. The strategy is withdrawn:

  - `SET_TRACEGRID.points_strategy` accepts speed and accuracy only,
  - the profile map no longer contains BinSizeAssined,
  - a device that reports it as its own default reads back as `speed`,
  - the UI select offers two options.

`TraceAlign` is unaffected. It is a real grid-alignment choice, and it stays.

## 17. SDR panadapter: the last display bin legitimately has no data

Measured on the live service (centre 23 MHz, decimate 128 -> span 390.625 kHz, 2048 points):

| Field | Value |
|---|---|
| display window | 22804687.5 .. 23195312.5 Hz |
| frame capture grid | 22804717.57 .. 23195102.02 Hz (delta 240.5 Hz > one 190.7 Hz bin) |
| after the `dsp/grid.ts` rebin | exactly one NaN, bin 2047 (the frame's own last bin is a real -117.1 dBm) |

The published grid stops before the upper edge of the window. The rebin then marks the outermost
display bin as a gap. The trace renderer inked that sample anyway, and `getY()` puts a non-finite
level below the graticule. The trace ran down the right plot edge for most of the plot height. In
the screenshot of the operator this line is one contiguous green column of 231 px at x=887 (the
plot border), from the trace down to the foot of the graticule. The line reads as a signal.

Both trace renderers now lift the pen at a sample that is not plottable (`render/plot.ts:
plottableLevel`), so a gap breaks the line. The same build measures a longest contiguous green
column of 23 px in the middle of the plot (a real burst) at the same settings.
## 18. External reference: `ReferenceClockFrequency` declares the input frequency to the clock PLL

Symptom (2026-10-19, SAN-90, 10 MHz GPSDO on REF IN): `SET_REFCK external` left the
15.125 MHz band empty and the spectrum unstable; internal clock was normal.

Official sources:
- htra_api.h:380-384: `ReferenceClockSource_Internal/External` "(默认10MHz)"; External
  "当系统检测到外部时钟无法锁定时，将自动切换至内部参考"; `External_Forced` never falls
  back; `External_SysClock` (0x04) bypasses the PLL.
- htra_api.h:688 / htra_api.py (ASG_Profile_TypeDef): `ReferenceClockFrequency` —
  "指定参考频率：对内参考和外参考均生效".
- Guide V0.55.89 p.155: `ReferenceClockFrequency` — "设置参考时钟频率 Hz（仅可设置
  10 MHz），可手动对系统时钟准确度进行修正".
- Guide §8.3 `Device_CalibrateRefClock`: GNSS 1PPS calibration, TriggerCount > 30
  recommended; `MeasAuxInfo.RefClkFreqOffset` = reference offset vs GNSS, ppm.

Driver gap: none of the three profile builders (SWP `device._profile`, RTA
`rta._configure_locked`, IQS `sdr._configure_iqs_locked`) wrote
`ReferenceClockFrequency`. `SWP_ProfileDeInit` pre-fills it with the last measured
INTERNAL reference (measured 100,000,066.18 Hz on this unit), so switching to external
declared ~100 MHz against the 10 MHz input.

Measurements (100.0 MHz carrier from a PlutoSDR, TRUE = 99,999,998 Hz as programmed;
displayed peak on the analyzer):

| state | carrier displayed | timebase ratio | `RefClkFreqOffset` | profile readback `ReferenceClockFrequency` |
|---|---|---|---|---|
| internal | 99,999,182 Hz | 1.0000082 | −0.636 ppm | 100,000,066.18 (firmware-measured) |
| external, field unset (DeInit leak) | 90.93–91.07 MHz across runs | 1.0981 / 1.0998 / 1.1012 (unlocked, wandering) | **−90,909 ppm** | 11,000,000.0 |
| external, field = 20e6 (declared 2× of real input) | 50,247,916 Hz | 1.99013 ≈ 2.0 | −90,909 ppm | 11,000,000.0 |
| external, field = 10e6 (fix) | 99,999,182 Hz — same bin as internal | 1.0000082, identical across 3 runs | **−0.006 ppm** | 10,000,000.0 |

Facts established:
- The field is the DECLARED input frequency: declaring 20 MHz against a 10 MHz input
  scaled the timebase ×1.99 (EXP-D1); declaring 10 MHz locks it to the GPSDO
  (EXP-E, three identical runs).
- An undeclared external switch mis-locks: timebase ×1.098..1.101 wandering,
  `RefClkFreqOffset` −90,909 ppm, and the readback pins at 11,000,000.0. The readback is
  a firmware-side value of the unlocked state: it stayed 11,000,000.0 whether the leaked
  DeInit value (100.000066e6) or an explicit 20e6 was declared. Its origin is not
  documented in the SDK materials available (htra_api.h, htra_api.py, guide V0.55.89).
- Internal vs external after the fix: the 100 MHz carrier lands in the same bin under
  both; `RefClkFreqOffset` −0.636 ppm (internal VCXO) vs −0.006 ppm (GPSDO).
- GNSS readout: the cached `Device_GetGNSSInfo` stays empty (sats=0, time=00:00:00,
  altitude=0) in every tested state — both `GNSSDataSource` settings (Internal/External,
  full matrix measured), streaming and idle. The `_Realtime` variants return live data
  (19-25 sats visible, 19-22 used, UTC time, altitude): `query_gnss` uses them with a
  cached fallback. `GNSSAntennaState=0` decodes to `GNSS_AntennaExternal` (correct with
  the external antenna connected). `GNSSDataSource` supports exactly Internal/External
  (header); both measured.
- `Device_CalibrateRefClock(CalibrateByGNSS1PPS, 1.0 s)` returns 2× the reference in
  EVERY measured state — GNSS locked (22 sats) or not, internal or external reference,
  either data source: 200,000,138 / 200,000,135.56 / 200,000,163 Hz vs profile-measured
  100,000,066.18 Hz. The 50-150 MHz bound in `calibrate_ref_clock` refuses these. The
  vendor app's own earlier calibration result persists as 参考频率偏差 −0.615800 ppm,
  matching our independent `RefClkFreqOffset` −0.636 ppm and the profile readback
  +0.66 ppm.
- The documented External auto-fallback to internal did NOT trigger at a −90,909 ppm
  mis-lock: `ReferenceClockSource` readback stayed External (1) throughout.
- `external_forced` returns status −15 (clock-unlock family) and stops the stream.

Fix: `ReferenceClockFrequency = 10e6` when `ref_clock` is external/external_forced in all
three profile builders. Regression: tests/test_device_state.py
`test_external_profile_carries_the_reference_frequency`.

## 19. Tier-2 amplitude calibration: SDR panadapter vs SWP reference, ±0.42 dB

Setup: tinySA Ultra generator → DC block → coax → N90 RF In; GNSS-locked 10 MHz GPSDO
on REF IN; SWP mode (vendor-calibrated path) and SDR mode (panadapter) measured against
the same carrier at 100.3 MHz, decimate 32, 2.5 s settle per point, median of frames.

| tinySA cmd | SWP read (reference) | SDR vendor RTAF | vdr − SWP | `channel_dbm` (gate level) |
|---|---|---|---|---|
| −40 dBm | −39.24 | −39.58 | −0.34 | −39.66 |
| −30 dBm | −29.94 | −29.63 | +0.31 | −29.65 |
| −20 dBm | −20.18 | −19.76 | +0.42 | −19.78 |

Conclusions:
- The SDR panadapter agrees with the vendor-calibrated SWP path within ±0.42 dB across
  the measured range. No 6.02 dB complex-FFT dimension error and no gross scale error.
- The `2·|X|/Σwindow` term (spectrum.py:50) stays: the NumPy path measured +2.1/+3.0 dB
  vs SWP (window/detector semantics, linear, not the ×2 factor — removing it would move
  NumPy to −3.4 dB vs vendor, worse).
- `channel_dbm` (the squelch gate level) tracks the same reference within ±0.42 dB.

Procedure artifacts that produced earlier false readings (each was chased and refuted):
- The device auto-gain and the RTAF stream need seconds to settle after mode/level
  changes: a reading 0.5 s after a change showed −60.38 dBm where the settled value was
  −19.79 dBm.
- The tinySA `cw()` replans the sweep engine (driver timeout 40 s): the level takes
  ~28 s to apply. Level-only steps use `TinySA.set_level` (near-instant).
- The generator output is non-monotonic at some commands (cmd −50/−40 both produced
  ≈−39.2 dBm; cmd −60 produced ≈−57.5) — it is NOT a usable absolute reference below
  its tracking range. Absolute adjudication uses the SWP readback as the reference.
- GNSS: `Device_GetGNSSInfo` (cached) stays empty on fw 0.55.109; the `_Realtime`
  variants return live data (22-25 sats). `Device_CalibrateRefClock(CalibrateByGNSS1PPS)`
  returns 2× the reference in every measured state — refused by the 50-150 MHz bound.
  The usable ppm metric is `MeasAuxInfo.RefClkFreqOffset`: internal −0.636 ppm, GPSDO
  external −0.006 ppm, vendor app −0.615800 ppm (internal) — all consistent.
- `WEBSA_GNSS_POLL` (default 1 s) A/B: identical 12 startup-settle transient errors at
  1 Hz and 30 s polling — the realtime poll has no measurable steady-state stream cost.

An implementation of this (gate on `channel_dbm`, `websa_dsp_demod_set_squelch_level`
ABI, frontend RTAF feed) was built and live-verified against these measurements, then
ROLLED BACK on 2026-10-19 pending a decision on the dual-AGC architecture (the
measurements above remain valid). The shipped squelch still references the post-AGC
audio level: with the audio AGC on it has no strength discrimination (the on/off
behavior in the report).


IFBW vs audio level (tinySA AM 60% depth, 1 kHz tone, −40 dBm, AGC off):
IFBW 1200 → −74.55 dBFS, 2500 → −72.43, 6000 → −68.70, 12000 → −67.51,
25000 → −65.48. The rise with IFBW is AM sideband physics (a 1 kHz tone
needs ±1 kHz of passband; IFBW 1200 Hz cuts it), not a path error. The
IFBW-independent channel level is the panadapter quantity (plan B squelch).

Stream-stall correlation (the audio-dropout report, 2026-10-19): during the gate
verification the log shows repeated `SDR stream stalled (-12)` / `(watchdog);
restarting the stream (1/3)` cycles — each restart is an audible gap. Two distinct
triggers observed: (a) −12 IF overflow while a STRONG direct injection (−20 dBm,
AGC off, atten 0) sat in the passband; (b) watchdog stalls around mode/level
transitions and during 1 Hz realtime GNSS polls. Task A (IFBW ladder + network
throttle) quantifies both.
