# FT8 SNR acceptance

Baseline and candidate use identical deterministic inputs. Baseline identity is
recorded in `tools/bench/ft8_snr_validation_v2.json`. Timed executables run
serially after compilation, without concurrent builds or test suites.

- Measure S/(N0 × 2500 Hz) on original receive IQ before cancellation. Numerical
  results are estimates, never silently clipped or substituted. A finite estimate
  outside the −25…+10 dB interval remains visible.
- Independent encoded GFSK with real and complex AWGN: −30, −27, −24, −20,
  −10, 0, +9, +15 and +20 dB, at 8/12/48 ksample/s. Absolute error ≤1.5 dB
  at ≥−20 dB and ≤3 dB below −20 dB on fixed realizations. Compare mean bias
  across independent seeds (|mean signed error| ≤0.5 dB at −10 dB).
- Common IQ scaling by 0.01/1/100 changes a finite estimate by <0.01 dB;
  scaling alone must not imply clipping. Fading and off-grid frequency use the
  same numerical tolerances. Small unresolved frequency error is diagnosed by
  regression, not searched against a known message in the decoder.
- Strong adjacent CW, multiple adjacent encoded signals and a sloped background:
  retain finite values; ≤3 dB error for the explicitly synthesized separable
  cases. Mark detected contamination/nonflatness independently of value.
  Exact co-channel interference cannot be separated by this meter and is a
  documented limitation, not an assertion of isolated transmitter power.
- Receive edges and partial bursts: use available reference support, retain a
  calculable value and label limited support. Truly insufficient symbols, zero
  noise/signal, invalid metadata and nonfinite IQ return null with a reason.
  Clipping is a quality limitation when a finite ratio survives.
- JSON snrDb equals measurement.value (including null). Quality, value kind and
  reasons survive ingress, history and JSONL/CSV export. Numeric legacy ABI and
  existing SDE2 envelopes remain compatible. Both languages explain degraded
  values and unavailable values; theme contrast uses existing theme tokens.
- Keep all current decoder tests and their floors: WSJT-X identical-input −20 dB,
  independent weak GFSK actual message recovery, OSD, sensitivity, real slot and
  cancellation. No unrelated decodes. Null SNR must not discard decoded text.
- Run sixteen-owner isolation, blocked search and full-mailbox checks. No new
  queues, workers, decoder candidate budgets or acquisition configuration.
- Report same-input meter and full-decoder wall/CPU medians separately. Meter
  median wall and CPU ≤1.25× baseline, full-decoder median ≤1.10× baseline;
  retain existing timing gates. These are host measurements, not hard deadlines.

Statistical calibration is distinct from decoder sensitivity. Validation retains
all scenarios, the complete input interval and the numerical tolerances above.
