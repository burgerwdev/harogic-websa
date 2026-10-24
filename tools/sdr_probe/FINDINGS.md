# SAN-90 SDK constraints

These constraints describe the SAN-90 SDK 0.55.100 adapter and reference tools.
Hardware-dependent observations apply to the tested SAN-90; they do not certify
throughput or RF performance of a different firmware or current service build.
Current operation and hardware gates are documented in [tools README](../README.md).

## IQ capture and SDK lifetime

- IQS uses a 62.5 MSPS base rate with 50 MHz usable bandwidth. Decimation factors
  are powers of two from 1 through 2048. Use returned capture geometry rather than
  inferring packet sample counts from the requested data format.
- Adaptive triggering supports continuous capture; FixedPoints triggers per frame.
  Configuration must be followed by explicit arm before reading.
- Keep SDK calls on the persistent capture thread and retain the stable
  `Device_Open` handle-slot storage for the session lifetime.
- A `-12` warning can accompany valid IQ. Preserve the samples and distinguish
  warning metadata from a failed acquisition. `san90_next_v3` returns IQ, FFT,
  warning and timing information together.
- Keep the validated high-pass DCC/QDC-off receive default. Changing to DCC-off
  requires firmware-specific RF validation; accepting a profile is insufficient.

## Vendor DDC and analog demodulation

- `DSP_DDC` output is interleaved complex float32 (`DataFormat == 6`), with
  `PacketDataSize = sample_count * 8`.
- Frequency translation follows `f_out = f_input_relative + DDCOffsetFrequency`.
  To tune an absolute frequency to DC, use `DDCOffsetFrequency = center - tune`.
- Input `SampleRate` is the input IQ rate; `ProfileOut.SampleRate` is the output
  rate. `ProfileOut.SamplePoints` is the rounded-up decimated sample count.
  `DSP_DDC_GetDelay` is expressed in output samples.
- Vendor `DSP_FFT` works with intact int16 IQS metadata but did not produce useful
  output for DDC complex-float data. Use the independent reference FFT for that path.
- ADM AM `ModDepth` is a fraction, not a percentage. Digital `Demod_*` APIs require
  the separate vendor library and license; they are not a portable decoder backend.

## Sweep and display geometry

- Use the actual point count and grid returned by `SWP_TraceInfo`. Requested
  TracePoints is not a promise of the resulting count. Manual RBW down to 1 Hz is
  accepted on the tested unit; `EstimateMinSweepTime` returned zero and is not a
  reliable scheduling estimate.
- Product trace-point strategy supports `speed` and `accuracy`. `BinSizeAssined`
  does not enforce the requested `TraceBinSize_Hz` on the tested firmware and can
  make wide sweeps extremely slow. `TraceAlign` remains an independent grid choice.
- A capture grid can end before the display window's upper edge. Rebinning may
  therefore produce an absent outer bin. Render non-finite levels as gaps; do not
  connect them to the plot border or invent a measured value.
- A Preset `device_default` request and the returned effective `pos_peak` detector
  are separate facts; report the request and effective state independently.

## Reference clock and GNSS

- `ReferenceClockFrequency` declares the reference input frequency to the PLL.
  For a 10 MHz external reference, explicitly set it to `10e6` in every acquisition
  profile; do not reuse the internal-clock value left by profile initialization.
- External reference mode can fall back to internal when lock fails; forced
  external mode does not. Readback from an unlocked PLL is not evidence of the
  actual input frequency.
- Cached `Device_GetGNSSInfo` can remain empty while the realtime query returns
  valid satellites, time and position. Prefer realtime data with a cached fallback.
  `GNSSDataSource` supports Internal and External; antenna state zero means external.
- `RefClkFreqOffset` is the reference offset relative to GNSS in ppm. Reference
  calibration and frequency accuracy require an independent RF/timebase check.

## Validation boundary

Use the current native hardware and browser gates for capture continuity, receiver
behavior and rendering. Short capture windows, synthetic throughput and display
frame rates alone do not establish sustained real-device sample conservation or
RF correctness. Keep raw external measurement evidence with its recording context.
