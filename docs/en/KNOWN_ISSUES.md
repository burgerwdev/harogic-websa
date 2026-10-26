# Known Limitations

## Device and SDK

- **Exclusive ownership:** `libhtraapi` requires exclusive access to the device. `Device_Open` returns `-1` while SAStudio4 holds it. The proprietary SDK can block or crash; unplug/replug recovery and shutdown must be qualified on the deployed hardware build.
- **Capture recovery:** a sustained SDK stall ends capture with its original cause. Recovery requires a fresh capture epoch; old timestamps cannot continue. Queue backlog waits under bounded backpressure until explicit failure without reopening the device. A stuck vendor owner remains parked. SDK finish and reopen are refused during its vendor call. Capture failure uses global error handling.
- **External reference:** supply a 10 MHz reference and set `ExternalSystemClockFrequency` accordingly. `Ext` falls back to the internal reference on unlock; `ExtForce` forces external reference use. Do not set `SystemClockSource=External`: it can hang the device.
- **GNSS calibration:** some firmware leaves `RefClkFreqOffset` at zero; use the computed offset after calibration. GNSS/1PPS calibration and 6/7/9 GHz soak qualification remain outstanding.
- **IF AGC:** `EnableIFAGC=0` is the default, consistent with the vendor profile. Its effect under IF saturation remains unqualified.
- **Factory calibration:** calibration is stored in firmware. Do not load an external frequency-response file with `Devcie_SetFreqResponseCompensation`.
- **Internal spurs:** integer-GHz spurs, including 1.000 and 2.000 GHz, affect SWP/RTA/SDR. Their apparent level can rise with input attenuation; narrow RBW can expose them in SWP. Treat them as device artefacts when interpreting a trace.
- **Trace grid:** the device-native grid determines the available points; a requested point count is not a guarantee of display resolution. The firmware ignores `SWP_Profile_TypeDef.TraceBinSize_Hz`, so fixed bin spacing is unsupported. `SET_TRACEGRID.points_strategy` accepts `speed` and `accuracy`.

## Measurements and triggers

- **Normalization:** the external sweep source must cover the measurement bandwidth, with dwell time at least as long as the analyzer hop time. A CW source only normalizes its own frequency effectively; the baseline is clamped to zero.
- **Phase noise:** offsets at the 100 Hz and 10 MHz boundaries are inaccurate. Minimum input level is -50 dBm.
- **Channel measurements:** channel power, OBW and ACPR use the displayed trace and are limited by its bin spacing. A narrow CW signal can occupy only one or two bins even with a much narrower RBW. For finer OBW resolution, reduce span or increase the available point count.
- **RTA level trigger:** triggering requires a threshold crossing. A steady signal already above the threshold when armed can wait indefinitely. `bus` is the supported RTA trigger source. POI describes the shortest burst the configuration is guaranteed to capture. Vendor timeout is also a communication timeout, so waiting can remain unknown; finite capture completion requires device/frame evidence. Fixture input does not simulate hardware hits.
- **Swept-mode level trigger:** SWP has no device level-trigger field. The frontend detects crossings between consecutive sweeps, making one sweep the minimum time unit. The display remains live while armed and freezes on a hit. RTA timing controls and POI are unavailable in swept mode.

## SDR

- **Zero-IF artefact:** IQ DC and LO leakage sit at the requested capture centre. A signal exactly at that frequency overlaps the artefact.
- **Upper-edge gap:** the capture grid can end before the upper edge of the display window. Uncovered bins remain gaps; RTA/SDR do not interpolate them with swept-mode GapFill.
- **Vendor ADM:** SINAD/SNR/THD require a dominant modulation tone. They are not meaningful quality indicators for ordinary broadcast audio; raw values remain available in STATUS `sdr.adm`.
- **Squelch level:** the pre-AGC tuned-bin peak is not integrated channel power. Modulation spreads power across bins, so set the squelch threshold for the received signal and mode.

## Receiver and browser boundaries

- **Receiver capacity:** the backend supports three independently configured slots. The product UI exposes only the main slot and disables auxiliary slots on entering SDR. Mixing, recording and dynamic channel counts are not delivered features; internal PFB bin counts do not represent product receiver capacity.
- **Raw DSP input:** direct `Pipeline` and `AnalogDemod` trait callers must supply finite input. Production `Receiver` and offline entry points check for nonfinite input; the raw DSP API does not.
- **RDS:** decoded text quality remains constrained; stereo indication reports pilot presence, not different left/right programme content.
- **AM:** modulation depth is single-tone-equivalent RMS envelope depth, not arbitrary programme peak depth. Overmodulation folds the absolute envelope and can under-report signed depth; values are not capped at 100%. Carrier fading invalidates offset/depth and clears lock. The recovery estimator is limited to `min(IF bandwidth * 0.2, receiver IQ rate * 0.1, 3000 Hz)` and excludes lock near that boundary; this is a pull-range limit, not a guarantee of acquisition. Sync enabled does not itself mean carrier locked; these metrics do not establish real-RF reception quality.
- **FT8:** decoding supports standard message types 1/2 within the supported search window, not unrestricted message types or searches.
- **SSTV:** post-image FSK callsign decoding is unavailable.
- **Radiofax:** automatic IOC 288/576 and 60/90/120/240 LPM are supported; 180 LPM is manual. Each segment holds at most 2400 lines, then reception continues in a new segment. A 64.058 s loop of a real midstream IQ sample produced 127 rows after the receive fix; a fresh 8.045 s capture at 7.7931 MHz produced 16 rows. Both used the unlocked IOC576/120 LPM fallback. Recorded events also rendered a 1810×127 browser image and exported PNG. Current live checks showed search states without image rows. Complete APT/phasing acquisition, other IOC/LPM combinations, long-page clock drift, frequency offset and fading interoperability remain unverified.
- **DRM:** short or looped codec playback can underrun; such clips do not establish sustained playback continuity. Codec audio bypasses the receiver AGC, noise reduction, squelch and de-emphasis controls, which are disabled in the UI; volume and audio enable remain usable. NR preferences remain stored with effective NR Off. Full signalling reconfiguration and next-multiplex timing are not fully applied.
- **Browser rendering:** a 20 fps acquisition target does not guarantee 20 fps for every display layer. Existing observations with spectrum, density, waterfall and navigation enabled are around 15 fps; this is not a fresh hardware or browser measurement.

For device constraints and probe context, see [SDR probe findings](../../tools/sdr_probe/FINDINGS.md). Hardware observations there are configuration-specific; this document does not establish qualification of a deployed build.
