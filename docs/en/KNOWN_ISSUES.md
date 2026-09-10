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
12. **Peak right-shift stuck (fixed)**: pos match takes nearest list item (±3 bins), avoiding matching an adjacent bin that would loop right-shift to itself
13. **Auto Ref with manual attenuation**: Auto Ref remains selected but is suspended while Atten is manual; it resumes with Atten Auto
14. **Remote access**: query tokens can enter browser history/proxy logs; use an HTTPS reverse proxy for remote control
15. **Fast worker exit**: the worker uses `os._exit` to avoid unstable vendor-SDK destruction, so it does not send a WS close frame; browsers reconnect automatically
16. **Remaining hardware qualification**: continue covering USB hotplug, GNSS calibration, slow clients, frequent SWP/RTA switching, and 6/7/9 GHz soak tests
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
20. **Auto Ref releases an armed trigger (to be fixed)**: in RTA, pressing Auto Ref after arming a level
    trigger returns the button to `Capture` and the threshold line disappears (the cause is not yet
    identified; Auto Ref does not re-enter the session). Changing Ref **manually** is unaffected - even
    when the threshold sits outside the visible range the line is pinned to the edge (with an arrow).
    Change the reference manually while armed, or press `Capture` again afterwards.
