# Known Limitations & Notes

1. **Occasional libhtraapi segfault/block**: RTA first attempts two in-place reconfigurations after repeated Trigger/Get failures. The supervisor restarts the worker after persistent failure, native crash, or call timeout. A fully wedged device can still require USB replugging. Persistent Web/SDK process separation is deferred to the VSA architecture phase.
2. **SAStudio4 exclusivity**: device is single-handle; Device_Open returns -1 while the official software is running
3. **External reference locking**: requires ExternalSystemClockFrequency=10 MHz correctly set with external signal present;
   Ext mode falls back to internal on unlock (normal device behavior); use ExtForce to force
4. **SystemClockSource=External is dangerous**: hangs the device (do not use)
5. **Normalization**: external sweep source must cover the measurement bandwidth; source dwell time ≥ analyzer hop time;
   under CW stimulus only the source point normalizes effectively (baseline clamped to 0)
6. **Phase noise**: 100 Hz / 10 MHz boundary offsets are inaccurate; min input -50 dBm
7. **Points**: frontend peak-preserving resampling; displayed points = device-native (requested value only affects device config)
8. **GNSS ppm**: RefClkFreqOffset is not filled by some firmware (always 0); use computed value after calibration
9. **Valley traversal (frequency direction)**: left/right valley navigation follows frequency direction (like peaks) — Valley locates the global minimum of display data,
   left/right jumps to adjacent depressions (independent valleys); multiple local minima within a depression are merged (25 bins), valley list items match Valley positioning
   (rebuilt as depression-wide display minimum), jumping back to the deepest valley has no offset
10. **smooth & peak/valley detection**: when smooth is on, extrema detection is fully based on the smoothed curve (matches display), amplitude uses smoothed value;
    when off, raw data is used (Raw Anchor real extremum in raw neighborhood); valley sort/dedup: peaks 3 bins, valleys 25 bins (depression merge)
11. **Pk threshold**: user edit locks (input realtime lock + activeElement guard), Auto button restores peak-50;
    threshold not updated when all markers are off (kept)
12. **Peak/valley navigation matches the nearest list item (±3 bins)**: a right jump cannot loop back to the same bin (matching an adjacent bin used to make right-shift appear stuck)
13. **Auto Ref with manual attenuation**: Auto Ref remains selected but is suspended while Atten is manual; it resumes with Atten Auto
14. **Remote access**: query tokens can enter browser history/proxy logs; use an HTTPS reverse proxy for remote control
15. **Fast worker exit**: the worker uses `os._exit` to avoid unstable vendor-SDK destruction, so it does not send a WS close frame; browsers reconnect automatically
16. **Remaining hardware qualification**: USB hotplug, GNSS/1PPS calibration and 6/7/9 GHz soak are
    still to be covered. Frequent SWP/RTA switching, slow clients and 60 s SDR audio continuity are now
    exercised by the Playwright state regression (0 worklet underruns with the audio worker).
17. **Resolution floor of the channel measurements**: channel power / OBW / ACPR are computed
    from the *displayed* trace, so they are limited by the point count. When the RBW is far below
    the displayed bin spacing a CW carrier only lights 1-2 bins and the OBW bottoms out there
    (measured at span 100 MHz / 998 points: RBW 100 kHz -> OBW 401.6 kHz, RBW 10 kHz -> 200.8 kHz).
    For a finer OBW reduce the span or raise the point count instead of only narrowing the RBW.
18. **RTA level trigger behaviour (measured)**: a level trigger fires on a threshold **crossing**
    (rising edge by default), so a steady signal that is already above the threshold when armed never
    triggers and simply waits (no new data). While waiting the device sends **no packets at all**, so the
    canvas stays on the last frame - which is why arming clears the canvas and shows a waiting chip.
    `Intercept >= ...` (POI) is the shortest burst the current configuration is *guaranteed* to catch.
    Of the RTA trigger sources only `bus` works here (host bus-triggered, ~100 fps, the default);
    `freerun` delivered no data in testing and is not offered. Triggering is an RTA-domain feature:
    the SWP profile only has internal free run and external triggers, with no level trigger field; the swept-mode level trigger shown in the UI is implemented in the frontend (see the next item).
19. **The swept-mode trigger is a software implementation**: SWP has no level trigger field, so the
    level trigger lives in the frontend and is judged on the crossing **between two consecutive sweeps**
    (one sweep is the smallest time unit). While armed the display stays live and it freezes on a hit.
    The RTA device timing features (debounce, delay, pre-trigger, acquisition, re-trigger, trigger
    output) and POI are therefore greyed out in the swept mode.
20. **SDR capture centre and the zero-IF artefact**: the IQS stream is tuned exactly on the
    requested centre now (the old "+200 kHz to avoid the DC centre" shift was removed - it left the
    lowest 200 kHz of the displayed window uncovered, a blank strip at the left edge). The IQ DC /
    LO-leakage artefact therefore sits at the centre of the panadapter; a signal exactly on the
    requested centre overlaps it.
21. **SDR audio path**: playback is received *and* delivered by a dedicated worker over a second
    `?audio=1` WebSocket and drives the AudioWorklet directly (the display connection uses
    `?noaudio=1` and carries no audio). A remaining audio gap is therefore a browser-side worklet
    ring underrun, not a dropped frame; `document.getElementById('spectrum').dataset.sdrAudio`
    exposes `enabled/muted/frames/buffered_ms/underruns/rms/worklet/worker` for diagnosis.
22. **Vendor ADM (SINAD/SNR/THD) is a single-tone metric**: with real broadcast audio there is no
    dominant modulation tone, so the vendor `ADM_*` values collapse to ~0 and THD is meaningless
    (same code, 1 kHz test tone: SINAD ~7, SNR ~12). The SDR panel no longer displays it
    (`cur-sdr-adm` removed); the raw values stay in STATUS `sdr.adm` for API use.
23. **SDR squelch threshold is channel-power dBFS**: it depends on the demod mode / IF bandwidth
    (a wider IF raises the noise floor), so the same number squelches differently per mode. The
    gate has 3 dB hysteresis, a 0.3 s hold and a 5/80 ms ramp, so it does not chatter at the
    threshold.
24. **IF AGC stays off (`EnableIFAGC=0`)**: it matches the official `Profile.xml` and the
    hardware available here cannot saturate the IF, so AGC has no observable effect. The wiring
    and the `ifagc_gain` readback remain for diagnostics (`WEBSA_IFAGC=1` flips the default for
    A/B tests). Measurement caveat worth remembering: the **tinySA Ultra+ output tops out around
    -18.5 dBm**; an earlier "receiver compression" reading at 0 dBm was the source limiting
    itself, not the analyzer. With the source at -18.5 dBm the SAN-90 error is about -0.68 dB,
    and with `preamp=AutoOn` the device keeps `preamp_actual=1` (it refuses to bypass).
25. **No frequency-response compensation is needed**: the factory calibration is burned into the
    firmware, so `Devcie_SetFreqResponseCompensation` (external file) must not be called.
    Measured baseline with a known tinySA source (-18.5 dBm, atten 0, preamp off): 100 MHz error
    -0.68 dB; the -30..-18.5 dBm range stays within 0.4-0.7 dB and the attenuator tracking is
    correct. When checking wideband flatness remember the tinySA's own flatness (typically
    +-2 dB) is part of the result.
26. **Integer-family spurs at 1.000/2.000 GHz are device-internal**: the same spur appears in all
    three modes (SWP/RTA/SDR) at exactly integer GHz, and it is **after** the input attenuator -
    adding 10 dB of attenuation raises its apparent level by ~10-13 dB (an external CW source
    stays flat), so it is an internal IF/ADC/clock spur, not a received signal. SWP hides it only
    because of the RBW (it emerges at 1.9 kHz and below). Software cannot remove it; report the
    1/2 GHz and rotation-harmonic family to the vendor if needed.
27. **Unexplained (low priority): "inverted attenuation" in the FM broadcast band.** At 101.7 MHz,
    span 1 MHz, `preamp=AutoOn`, raising the actual attenuation from 3 to 18 dB made the reading
    *rise* by ~15 dB (-71.4 -> -56.4 dBm). A CW source retest does not reproduce it, so the
    working hypothesis is front-end overload from multi-carrier composite power; confirming it
    needs a known source in a strong-signal environment.
