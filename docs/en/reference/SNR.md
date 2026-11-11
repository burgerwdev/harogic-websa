# Native IQ and protocol measurements

[Documentation](../INDEX.md) · [简体中文](../../zh-CN/reference/SNR.md)

RF SNR measurement is disabled by default. Enable **Measure RF SNR** in the channel’s mode settings and apply settings to start analysis and show its live card. Disabling it releases the analyzer and stops RF measurement events; demodulation, protocol SNR and audio continue. The processor API field is `rf_snr` (boolean, default `false`); `observed_receiver.rf_measurement` is absent while disabled.

## Definitions and contract

The native receiver emits `rf-measurement` events on SDE2 and includes the latest `rf_measurement` in `processing_runtime.observed_receiver`. Each event inherits the receiver UID, service ID, fine-channel ID, capture epoch, processing generation and sample clock of its envelope. Historical decoded FT8 reports describe their original burst; they are not current channel readings.

| Metric | Definition / units | Bandwidth and position | Method and validity |
|---|---|---|---|
| RF SNR | `10 log10((Pchannel − N0 B)/(N0 B))`, dB | Explicit `band_low_hz` through `band_high_hz` relative to the dial; before demodulation, AGC, squelch, NR and EQ | Fixed Hann/Welch analysis; **conditional** on sparse occupancy and additive, spectrally flat background. All occupied in-band power, including other stations, is in the numerator. Unresolved interference can be in the background. |
| FT8 SNR | Decoded-tone power minus matched-filter background, divided by `N0 × 2500 Hz`, dB | **2500 Hz**; original IQ before cancellation, central 75% of each complete observed symbol | Median-initialized exponential-tail-corrected noise fit from six fixed ±5/7/9-bin Hann references. Numeric estimate and quality are independent; no display-range clamp or gate. |
| FAC weighted MER | `−10 log10(Σ |H|² |Y/H − Dnearest|² / Σ |H|²)`, dB | Equalized FAC QPSK cells over one frame; nominal DRM occupancy is reported as context, not an integration bandwidth for MER | Includes noise, interference, distortion and equalization error. Nearest-decision bias persists at low MER. Not RF SNR. |
| FAC decision SNR | FAC MER plus `10 log10(avg_cell_power × Ncar × Δf/Bnominal)`, dB | DRM30 occupancy bandwidth; **100 kHz for DRM+**, not occupancy-code-zero's DRM30 4.5 kHz | Conditional **flat AWGN** interpretation only. Unavailable below 10 dB MER, before FAC lock, after a failed current FAC or at the numerical error floor; accumulators reset every frame, including zero-error frames. |
| WFM pilot contrast | Smoothed `20 log10(A19k/A21k)`, dB | Discriminator composite after anti-alias filtering and decimation; one-pole two-sided ENBW `Fs α/(2−α)` (about 157 Hz for its 50 Hz pole) | 2 s smoothing; diagnostic approximation to pilot SNR, not a power-debiased noise measurement, RF SNR or programme-audio SNR. This statistic does not control listener gain or NR. |
| External reports | Original number, unit and provider | Provider-defined | The provider defines the algorithm and raw output. GPS per-satellite C/N₀ remains **dB-Hz**. |

`metric_type`, `unit`, `reference_bandwidth_hz`, `location`, `method`, `averaging_seconds`, `state` and `reason` travel with native measurements. RF additionally reports observation duration, FFT spacing, window ENBW, flat passband, linear powers (ADC full-scale squared), accepted-reference fraction, statistical uncertainty and a maximum age. Unavailable values are JSON `null`; negative SNR is preserved. Legacy numeric DSP/WASM reports use NaN for an unavailable FT8 value, which the native JSON layer serializes as null.

There is no claim of exact audio SNR for arbitrary speech, music or images without a clean reference. Known-tone SNR/SINAD/THD remain bench metrics. Wiener/DeepFilterNet internal estimates and the DRM equalizer's Wiener regularization are not RF readings or measured quality improvements.

## FT8 value and quality

`snrDb` and `measurement.value` contain the same finite estimate or null. Finite
values carry `value_kind: estimate`; this implementation reports neither a
clipped number nor a bound. Finite estimates use `state: conditional`; unavailable values use
`state: unavailable`. `quality: good/degraded/unavailable` and `quality_reasons`
are optional metadata. Test coverage and the −25…+10 dB interval do not gate
numeric display. Reference differences above 6 dB indicate limited quality.

A corrected exponential order-statistic median initializes a bounded noise fit.
Each iteration rejects powers above four times the current noise scale and
compensates for the removed AWGN tail using
`E[X | X < 4μ]/μ = 1 − 4/(exp(4) − 1)`. The estimate uses both reference sides;
it never selects the quietest side or minimum bins. Side imbalance, heavy tails,
receive-edge support, partial bursts, repeated IQ peaks and statistical
uncertainty are reported separately. Common IQ scaling does not imply clipping.
Only complete observed symbols contribute; a partial-burst value describes that
observed portion. The approximate statistical uncertainty excludes fading,
model error and interference bias.

Decoded messages survive measurement failure. Truly unresolved signal/noise,
insufficient observations, invalid metadata/codewords and nonfinite IQ produce
null with a reason. The UI keeps usable degraded numbers, marks them with a
subtle theme-aware colour and dotted underline, and explains quality on hover.
The dash also has a reason tooltip. CSV appends quality/definition columns and
full measurement JSON; JSONL retains the same object. Older numeric-only reports
remain readable. The floating window displays buffer duration, attempts and decoded-message
counts. Linear IQ RMS percentage is omitted. Decode rows do not retune the receiver.
Near-zero negative estimates round to `0`, never `−0`; hover shows the estimate
to two decimals, and exports retain the original unrounded value.

This is a local background estimate, not a precision measurement of an isolated
transmitter. Co-channel energy is inseparable; dense majority contamination,
sharp PSD curvature and receiver filtering can bias it. A quality flag is not an
error bound. See the [pinned official source review](FT8_SNR_REVIEW.md),
[fixed acceptance conditions](FT8_SNR_ACCEPTANCE.md) and
[validation data](../../../tools/bench/ft8_snr_validation_v2.json).

## RF analysis and limitations

Complex two-sided PSD is `|FFT(xw)|²/(Fs Σw²)`. Fractional edge-bin integration uses the actual IQ rate, not a rounded audio clock. Periodic Hann ENBW is `1.5 Fs/N`. This normalization conserves complex power; it does not divide by coherent gain or ENBW twice.

The upstream FIR's certified flat passband comes from the actual baseband plan. Noise references exclude four edge bins and Hann leakage neighborhoods. The analyzer masks strong spectral components, requires at least 64 remaining bins and 60% clean reference coverage, and checks coverage and agreement (within 1.5 dB) across eight frequency regions. It never samples the suppressed stopband as a noise floor or tries to undo deep attenuation. The same-passband FIR calibration checks power after the real channel filter; this is not a calibrated correction for arbitrary RF hardware response.

Pure noise, a signal below the three-sigma resolution threshold, insufficient references, broad occupancy, nonflat background/response, excessive leakage or overload produce unavailable states. **A broadband interferer indistinguishable from noise cannot be identified from IQ alone.** Even an accepted reading is conditional. WFM/DRM or any signal filling the flat passband may have no identifiable background and therefore no RF SNR. The analyzer shares channelized IQ with decoding and recording; it does not require a separate wideband capture.

Calibration covers injected RF SNR **−10…35 dB** for isolated/sparse signals in white Gaussian noise. Reported boundary estimates may extend by the 1 dB error allowance; values beyond −11…36 dB are unavailable. The uncertainty field is an approximate one-sigma sampling uncertainty, not a hardware calibration or guaranteed confidence interval. The fixed tests are reproducible checks, not an exhaustive statistical certification.

Analysis uses 512–4096 points and 32 non-overlapping windows, at most 64 FFTs/s/channel, with reusable FFT storage and no additional queue. Narrow channels average longer: at 2 ksample/s and 250 Hz bandwidth the observation is 65.536 s; at 390625 sample/s and 100 kHz it is about 0.5 s. Both effective averaging time and elapsed observation are exposed. RF expires after twice the observation budget plus 0.5 s, including in a stopped browser stream. Retuning, bandwidth/rate/flat-passband changes, capture restart and discontinuities clear incompatible estimates. Audio gain does not reset RF history. Hardware gain reconfiguration starts a new capture identity. Clipping visible at channel IQ is rejected; clipping upstream that is hidden by filtering cannot be certified absent.

## Validation

```sh
cargo test --release -j 1 --manifest-path backend/Cargo.toml --lib rf_ -- --nocapture
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --lib snr_measurement_tests -- --nocapture
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --test ft8_snr_robust -- --nocapture
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --test ft8_gfsk --test ft8_snr_sweep --test ft8_osd --test ft8_reference --test ft8_real_slot --test ft8_subtract
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --lib decision_metric_validation -- --nocapture
python3 tools/bench/compare_ft8_snr.py --reference-bin /tmp/wsjtx/usr/bin --output /tmp/ft8-snr
```

The reference tool requires NumPy and independent WSJT-X `jt9`/`ft8sim` binaries.
Debian WSJT-X **2.7.0+repack-1** provides a reproducible executable reference.
`ft8sim` synthesizes GFSK; known active-burst power determines the fixed-seed AWGN
variance. Both decoders receive identical quantized samples. The tool records
executable/input hashes, reference output and native CSV. A sensitivity comparison
requires actual payload recovery; known-codeword measurement alone is calibration.
The `ft8_gfsk` test uses independent BT=2 synthesis, multiple onset times and carrier
offsets, pure-noise rejection and the decoder timing budget.

Numerical cases, error statistics, quality flags, decode rates and performance
results are available in the [FT8 validation record](../../../tools/bench/ft8_snr_validation_v2.json).
The [acceptance specification](FT8_SNR_ACCEPTANCE.md) defines numerical and
resource limits. `tools/bench/ft8_snr_cost.py` compares precompiled executables in
seven alternating serial trials; compilation and other tests are excluded from
the timed interval. Process CPU includes identical input/test setup.

[RF/DRM validation data](../../../tools/bench/snr_validation.json) includes sample
rates, bandwidths, injected powers, FIR/fading cases and decision-bias results.
RF checks cover chunk independence, pure noise, gain/AGC/squelch/NR independence,
channel isolation, retuning invalidation, real FIR response, frequency offset,
fading and stale/overload states. Display FFT, spectrum/waterfall scaling and local
audio controls are outside the meter configuration. Ambient reception supplies no
known absolute SNR reference.

References: [2500 Hz convention](https://wsjt.sourceforge.io/wsjtx-main_en.html),
[pinned WSJT-X/JTDX source comparison](FT8_SNR_REVIEW.md),
[ETSI FAC MER/SNR definitions](https://www.etsi.org/deliver/etsi_TS/102300_102399/102349/04.02.01_60/ts_102349v040201p.pdf).

## WFM audio and continuity

IQ and IQ/2 resamplers retain bounded unmatched tails; only corresponding samples
enter the stereo matrix and linked AGC. Retuning clears those tails. A 33-tap
half-band FIR filters the discriminator composite before decimation. Integer mono
delay and a centred 33-tap fractional side delay align stereo phase and upper-band
gain. At 325520.8333 → 48000 sample/s the added presentation delay is 19 frames
(0.396 ms), reported in AUD2 timing. This path uses fixed FIR/delay state and a
reusable f64 block buffer without an additional IQ queue.

Continuity checks cover fractional input rates, 44.1/48/192 kHz output,
1/17/9765/11165/12180-sample partitions and retuning. Out-of-band tone checks
require at least 50 dB rejection at 300.480769/325.520833/488.28125 ksample/s and
44.1/48 kHz output. These checks quantify synthetic alias rejection and waveform
consistency; they do not measure programme-audio SNR or subjective reception quality.
Structured waveform evidence is available in the
[audio diagnostic record](../../../tools/bench/fm_audio_diagnostic.json).

```sh
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --lib analog::
cargo test --release -j 1 --manifest-path backend/Cargo.toml --lib reused_worker_history_matches_serial_bits_at_partial_boundaries
```
