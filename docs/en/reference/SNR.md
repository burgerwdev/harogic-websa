# Native IQ and protocol measurements

[Documentation](../INDEX.md) · [简体中文](../../zh-CN/reference/SNR.md)

## Definitions and contract

The native receiver emits `rf-measurement` events on SDE2 and includes the latest `rf_measurement` in `processing_runtime.observed_receiver`. Each event inherits the receiver UID, service ID, fine-channel ID, capture epoch, processing generation and sample clock of its envelope. Historical decoded FT8 reports describe their original burst; they are not current channel readings.

| Metric | Definition / units | Bandwidth and position | Method and validity |
|---|---|---|---|
| RF SNR | `10 log10((Pchannel − N0 B)/(N0 B))`, dB | Explicit `band_low_hz` through `band_high_hz` relative to the dial; before demodulation, AGC, squelch, NR and EQ | Fixed Hann/Welch analysis; **conditional** on sparse occupancy and additive, spectrally flat background. All occupied in-band power, including other stations, is in the numerator. Unresolved interference can be in the background. |
| FT8 SNR | Decoded-tone power minus matched-filter background, divided by `N0 × 2500 Hz`, dB | **2500 Hz**, on original IQ before cancellation; central 75% of each complete symbol | Known Costas and CRC-decoded data tones; six off-tone Hann filters at ±5/7/9 window bins. Conditional on local flat background; never sync-score × 0.5. |
| FAC weighted MER | `−10 log10(Σ |H|² |Y/H − Dnearest|² / Σ |H|²)`, dB | Equalized FAC QPSK cells over one frame; nominal DRM occupancy is reported as context, not an integration bandwidth for MER | Includes noise, interference, distortion and equalization error. Nearest-decision bias persists at low MER. Not RF SNR. |
| FAC decision SNR | FAC MER plus `10 log10(avg_cell_power × Ncar × Δf/Bnominal)`, dB | DRM30 occupancy bandwidth; **100 kHz for DRM+**, not occupancy-code-zero's DRM30 4.5 kHz | Conditional **flat AWGN** interpretation only. Unavailable below 10 dB MER, before FAC lock, after a failed current FAC or at the numerical error floor; accumulators reset every frame, including zero-error frames. |
| WFM pilot contrast | Legacy smoothed `20 log10(A19k/A21k)`, dB | Discriminator composite after anti-alias filtering and decimation; one-pole two-sided ENBW `Fs α/(2−α)` (about 157 Hz for its 50 Hz pole) | 2 s smoothing; diagnostic approximation to pilot SNR, not a power-debiased noise measurement, RF SNR or programme-audio SNR. This statistic does not control listener gain or NR. |
| External reports | Original number, unit and provider | Provider-defined | Algorithms and raw provider output are unchanged. GPS per-satellite C/N₀ remains **dB-Hz**. |

`metric_type`, `unit`, `reference_bandwidth_hz`, `location`, `method`, `averaging_seconds`, `state` and `reason` travel with native measurements. RF additionally reports observation duration, FFT spacing, window ENBW, flat passband, linear powers (ADC full-scale squared), accepted-reference fraction, statistical uncertainty and a maximum age. Unavailable values are JSON `null`; negative SNR is preserved. Legacy numeric DSP/WASM reports use NaN for an unavailable FT8 value, which the native JSON layer serializes as null.

There is no claim of exact audio SNR for arbitrary speech, music or images without a clean reference. Known-tone SNR/SINAD/THD remain bench metrics. Wiener/DeepFilterNet internal estimates and the DRM equalizer's Wiener regularization are not RF readings or measured quality improvements.

## RF analysis and limitations

Complex two-sided PSD is `|FFT(xw)|²/(Fs Σw²)`. Fractional edge-bin integration uses the actual IQ rate, not a rounded audio clock. Periodic Hann ENBW is `1.5 Fs/N`. This normalization conserves complex power; it does not divide by coherent gain or ENBW twice.

The upstream FIR's certified flat passband comes from the actual baseband plan. Noise references exclude four edge bins and Hann leakage neighborhoods. The analyzer masks strong spectral components, requires at least 64 remaining bins and 60% clean reference coverage, and checks coverage and agreement (within 1.5 dB) across eight frequency regions. It never samples the suppressed stopband as a noise floor or tries to undo deep attenuation. The same-passband FIR calibration checks power after the real channel filter; this is not a calibrated correction for arbitrary RF hardware response.

Pure noise, a signal below the three-sigma resolution threshold, insufficient references, broad occupancy, nonflat background/response, excessive leakage or overload produce unavailable states. **A broadband interferer indistinguishable from noise cannot be identified from IQ alone.** Even an accepted reading is conditional. WFM/DRM or any signal filling the flat passband may have no identifiable background and therefore no RF SNR. This phase does not add a wider parallel capture or change the decoder/recording IQ to obtain guard bands.

Calibration covers injected RF SNR **−10…35 dB** for isolated/sparse signals in white Gaussian noise. Reported boundary estimates may extend by the 1 dB error allowance; values beyond −11…36 dB are unavailable. The uncertainty field is an approximate one-sigma sampling uncertainty, not a hardware calibration or guaranteed confidence interval. The fixed tests are reproducible checks, not an exhaustive statistical certification.

Analysis uses 512–4096 points and 32 non-overlapping windows, at most 64 FFTs/s/channel, with reusable FFT storage and no additional queue. Narrow channels average longer: at 2 ksample/s and 250 Hz bandwidth the observation is 65.536 s; at 390625 sample/s and 100 kHz it is about 0.5 s. Both effective averaging time and elapsed observation are exposed. RF expires after twice the observation budget plus 0.5 s, including in a stopped browser stream. Retuning, bandwidth/rate/flat-passband changes, capture restart and discontinuities clear incompatible estimates. Audio gain does not reset RF history. Hardware gain reconfiguration starts a new capture identity. Clipping visible at channel IQ is rejected; clipping upstream that is hidden by filtering cannot be certified absent.

## Reproduce validation

```sh
cargo test --release -j 1 --manifest-path backend/Cargo.toml --lib rf_ -- --nocapture
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --lib snr_measurement_tests -- --nocapture
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --lib decision_metric_validation -- --nocapture
python3 tools/bench/compare_ft8_snr.py --reference-bin /tmp/wsjtx/usr/bin --output /tmp/ft8-snr
```

The optional reference tool needs NumPy and independent `jt9`/`ft8sim` binaries. This run used Debian WSJT-X **2.7.0+repack-1**, unpacked without installing it. It generates GFSK with `ft8sim`, adds fixed-seed AWGN at known active-burst power, and gives identical quantized samples to both implementations. It saves hashes, reference output and native CSV. At −20 dB the native search did not decode this input, so the measurement test uses the codeword confirmed by WSJT-X and independently known symbol placement. At −24 dB WSJT-X also did not decode this realization; that case is recorded, not claimed as a decoder comparison. Search sensitivity and SNR accuracy are separate checks.

Measured on 2026-11-10:

| Validation | Result |
|---|---|
| RF 24 combinations: 2/12/48.828125/390.625 ksample/s, 250/3000/12000/100000 Hz, −10/−5/0/10/25/35 dB | Maximum observed error **0.162 dB**; acceptance target <1 dB |
| Strong adjacent off-bin tone outside measurement band | **9.960 dB** for injected 10 dB; rejected from background references |
| FT8 independent GFSK comparison, injected −20/−16/−10/0/+8 dB | Native **−20.473/−16.396/−10.354/−0.336/+7.667 dB**; WSJT-X **−20/−16/−10/0/+8 dB**; largest difference **0.473 dB** |
| FT8 known-symbol AWGN with per-symbol fading, 8/12/48 ksample/s | Maximum observed error **2.126 dB** at −24 dB; targets <3 dB below −20 dB and <1.5 dB from −20 dB upward; approximate sampling uncertainty is reported |
| Real fine-channel FIR at 15258.7890625 sample/s; frequency offset + fading | Injected −5 dB → **−4.964 dB** after FIR; fading average 1.761 dB → **1.669 dB** |
| FAC nearest-decision bias, flat AWGN | At −5/0/5/10/20 dB: **2.251/1.754/0.489/0.014/0.000 dB**; motivates the ≥10 dB conditional SNR gate |
| RF computational microbenchmark, 2 s at 390625 complex sample/s | **0.0134 s** wall time in the final observed run, **98304 bytes** persistent FFT buffers; FFT plan and temporary statistics are additional bounded storage |

Other regression checks cover chunk independence, pure noise, gain/AGC/squelch/NR independence, channel isolation, tuning invalidation, real FIR response, frequency offset, fading and stale/overload states. Display FFT length/window, spectrum scaling, waterfall and local audio controls are absent from the meter configuration and data path. Hardware ambient reception cannot establish an absolute SNR truth; use injected IQ for accuracy checks.

Reference definitions: [WSJT-X guide: 2500 Hz reference](https://wsjt.sourceforge.io/wsjtx-main_en.html), [WSJT-X decoded-tone estimation](https://github.com/WSJTX/wsjtx/blob/master/lib/ft8/ft8b.f90), [ETSI TS 102 349: FAC weighted MER and receiver SNR](https://www.etsi.org/deliver/etsi_TS/102300_102399/102349/04.02.01_60/ts_102349v040201p.pdf). The native FT8 method uses its actual filter ENBW rather than copying WSJT-X's fixed −27 dB offset or its reported SNR floor.

Machine-readable [validation results and reference hashes](../../../tools/bench/snr_validation.json) are included with the test tools. The frontend suite passed 2122 tests; backend library and service suites passed 154 and 400 tests, respectively (hardware/model-dependent tests remain explicitly ignored); DRM passed 92 tests, FT8 unit checks passed 30 tests, and FT8 reference/sensitivity/subtraction regressions passed 8 tests. On a steady host, the existing FT8 slot timing gate passed at **3.044 s**; simultaneous compilation/UI tests previously produced **5.690 s**, so this is not a real-time worst-case guarantee. The real SAN-90 service was restarted with the new binary and its exact reception configuration restored. Switching display FFT 32768 → 65536 → 0 kept the fine stream identity, 12 kHz RF bandwidth and 1024-point RF analysis unchanged; current ambient reception correctly returned unavailable for a contaminated noise reference.

## Broadcast reception and audio continuity checks

The 101.7 MHz popping investigation has a [diagnostic record](../../../tools/bench/fm_audio_diagnostic.json) describing simultaneous backend AUD2 PCM, continuous browser audio taps and historical comparisons. With actual playback drift correction and linear interpolation included, controlled Chromium source output matched backend PCM with peak residuals of **3.73×10⁻⁹ / 1.49×10⁻⁸** at 48/44.1 kHz. At 48 kHz the final EQ/compressor output, aligned by constant gain and 288 frames of delay, had residual RMS **1.80×10⁻⁹**. These are waveform consistency errors, **not audio SNR or measured quality improvements**. The user's existing browser session and physical audio sink were not captured.

Before the auxiliary anti-alias fix, identical saved IQ produced identical native and v1.10.3 WFM output with AGC disabled. The discriminator also matched SDK `ADM_FMDemod` with about 0.0014 Hz RMS difference. Raw SDK IQ bypassing the PFB contained the same envelope dips and discriminator outliers. These observations do not establish their physical cause.

A separate reproducible stereo partition defect was fixed: IQ and IQ/2 resamplers can disagree by a frame at a block boundary. Padding or discarding difference frames creates transients. Bounded unmatched tails now wait before linked AGC; only matched samples enter the matrix, and retuning clears the tails. Regression covers actual fractional rates, 44.1/48/192 kHz output, 1/17/9765/11165/12180-sample blocks and retuning. The original 12180-sample broadcast packets did not trigger this defect, so the fix is not evidence that the reported popping has been resolved.

A further defect was fixed: directly decimating the discriminator composite by two folded out-of-band components near the pilot/stereo carrier into audible tones. A 33-tap half-band FIR now precedes decimation. Integer mono delay and a centred 33-tap fractional side delay preserve stereo phase and upper-band gain. At 325520.8333 → 48000 sample/s the extra delay is 19 frames (0.396 ms), included in AUD2 timing. Reset, partitioning and startup account for the new state. Input IQ is unchanged. Known out-of-band tone regression requires at least 50 dB rejection at 300.480769/325.520833/488.28125 ksample/s and 44.1/48 kHz output. This measures synthetic alias rejection, not broadcast audio SNR improvement.

The user confirmed the same noise in both backend/browser clips, good reception with another radio on the same antenna, and noise with the longwire but not YouLoop. Antenna and live gain trials contain different programmes; waveform subtraction or audio RMS/peak comparisons cannot establish exact improvement. Trials covered 0/−20/−40/−50 dBm reference levels, preamp policy, IF grade, high linearity and attenuation, then restored settings. No lower-gain setting reliably eliminated the original issue; analog overload is not established. Manufacturer guidance says Auto preamp usually enables below about −30 dBm; at fixed total gain, higher IF grade lowers RF gain, improves linearity and raises the noise floor. See the [SAStudio preamp guide](https://www.harogic.com/wp-content/uploads/2026/08/HAROGIC-SAStudio4-User-Guide-V4.55.64.pdf) and [HTRA gain mapping guide](https://www.harogic.com/wp-content/uploads/2026/08/HAROGIC-HTRA-API-Programming-Guide-V0.55.100.pdf). After the fix, the operator reported a large improvement with the original noise barely audible. This is qualitative reception validation, not a measured audio SNR improvement. The physical origin of IQ envelope dips remains unresolved. The audio fix applies to the WFM-specific branch; shared RF measurements apply to every receive channel.

```sh
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --lib analog::
cargo test --release -j 1 --manifest-path backend/Cargo.toml --lib reused_worker_history_matches_serial_bits_at_partial_boundaries
```

Anti-alias validation passed 54 analog, 154 backend library, 400 service, 86 related frontend, 3 independent demodulation reference and 1 full-band stereo response checks. Across 7 offline replays of the same 12.048 s IQ, median full-demodulation wall time for 12180-sample blocks changed from 0.693 s to 0.738 s on this host; this is not a real-time worst-case bound. Added storage consists of fixed FIR/delay state and a reusable f64 block buffer, with no additional IQ queue. Final same-IQ replay preserves mono sample bits after compensating 19 frames. Hardware service was restarted with the new binary and exact reception settings restored; IQ/audio continuity and browser playback showed no underruns or extra resets.
