# Known Limitations & Notes

1. **Occasional libhtraapi segfault**: fast reconfiguration / extreme configs may crash; restart the service to recover, the device may need replug
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
