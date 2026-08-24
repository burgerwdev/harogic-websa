# HAROGIC FAQ V2.2 Notes (project design related)

## API Call Constraints
1. **Only one function call at a time** → single-process serialized model (core architecture of this project)
2. A device does not support API + software running simultaneously (Device_Open returns -1 when SAStudio4 is running)
3. Configuration functions may be called multiple times; unset params use initialization defaults
4. Continuous acquisition requires looping the Get functions

## Spectrum / SWP
- SWP_GetFullSweep returns a wider range than the requested params → must trim with DSP_InterceptSpectrum
- Trace point count is the user-expected value; the device adjusts the actual value by internal policy
- RBW is computed from sample rate / window factor / decimation / sample points
- Noise floor: reduce reference level + RBW

## Reference Clock
- ReferenceClockSource: 0=Int, 1=Ext (auto-fallback to internal on unlock), 2=Int+ (DOCXO), 3=ExtForce (no fallback on unlock)
- External reference frequency: SAN-90 external input is 10 MHz (SCIPI example ROSC:EXT:FREQ 10MHz)
- EnableReferenceClockOut: 0 no output, 1 output reference clock; supported on 9 GHz+ models only
- SystemClockSource external switching is **dangerous** (vendor-supervised only), can hang the device
- Reference clock calibration: Device_CalibrateRefClock (GNSS 1PPS, TriggerCount ≥ 30, requires real 1PPS signal)

## Phase Noise (PNM)
- 100 Hz and 10 MHz boundary offsets are inaccurate; suggest SWP span = 2× max offset
- Typical minimum PNM input power: -50 dBm
- Incremental acquisition: FrameUpdateCounts increments per segment; each Get returns partial accumulation (real-time display mechanism)

## Misc
- IF gain 1/4 steps: ~1 dB amplitude difference at some frequencies
- Spur rejection algorithm is only effective in SWP mode
- SAN series: SAN-45 9kHz-4.5GHz / SAN-60 9kHz-6GHz / SAN-90 9kHz-9GHz, same functions, different specs
