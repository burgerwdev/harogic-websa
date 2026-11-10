# Operations and limitations

[Documentation](INDEX.md) · [Getting started](GETTING_STARTED.md)

## Service and device recovery

Run `./status.sh` first. It reports the owned process, actual endpoint, log path, device state and
whether the running executable matches the file on disk. After a rebuild, restart the intended
instance if it reports a changed binary. Use `--instance NAME` consistently for named instances.
The root launchers use port 8080; invoking `websad` directly defaults to 8099 and fixture input
when no SDK adapter is specified. Launcher configuration is in [Getting started](GETTING_STARTED.md).

| Symptom | Check and action |
|---|---|
| Device open fails or returns `-1` | Close SAStudio4 and other SDK clients; the device requires exclusive ownership. |
| Service is not running | Read the log path reported by the launcher; check its binary, frontend and SDK prerequisites before running `./run.sh` again. |
| USB/SDK capture is stalled | Inspect the original error and log; stop the instance before reconnecting USB and starting again. A vendor call that never returns can prevent safe reopen and may require a host restart. |
| SDR stops after a brief clock/ADC warning (`-14` through `-19`) | Continuous receivers retry shared capture up to three times with fresh stream identities; strict receivers remain stopped. Inspect the retained interruption if recovery does not complete. |
| A receiver failed but spectra continue | Inspect that receiver's diagnostic and explicitly retry its mode. `receiver_retiring` means old workers/subscriptions have not finished; retry later. |
| Browser opens but audio is silent | Enable browser audio, then check receiver state, listen/mute/solo, local volume, squelch and decoder lock. DRM also needs a supported audio service. |
| Configuration is rejected in harmonic/phase-noise mode | Stop that measurement before changing SWP-owned frequency, bandwidth, gain or clock settings. |

The launcher monitors abnormal exits and retries after 1, 2, 4, 8, 16, then 30 seconds
(capped at 30 seconds until resources recover). `./stop.sh` cancels retries and stops the
owned child. `./status.sh` reports recovery while the child is unavailable. Restarts use
the original launch environment and fresh service state; unsaved live receiver settings
are not restored. Directly invoking `websad` bypasses this monitor. A failed shared capture
affects its dependent receivers; individual processing failures can leave raw IQ and spectra
available. Successful recovery establishes fresh stream identities rather than joining samples
across a gap. SDK calls are serialized by the retained device owner; a blocked owner must finish
before that handle can be closed or reopened. See [state ownership](reference/STATE.md).

The service defaults vendor MKL to one internal thread (`MKL_NUM_THREADS=1`,
`MKL_DYNAMIC=FALSE`) before loading the SDK, reducing nested parallelism under resource
pressure. These settings do not restrict Rust multichannel workers; explicit environment
overrides are respected.

The plot's diagnostic area retains errors until a matching successful operation or explicit
recovery clears them. A socket reconnect or unrelated successful command does not prove recovery.
For reports, include the error, active mode, device/SDK version and instance log.

For full-rate IQS, `USB handoff full` means the delivery queue filled; `USB transfer failed`
reports the original USB status and received byte count. Bounded timing records near the error
help distinguish these paths. Callback times describe userspace handling, not device completion.
Delivery queue age is not USB thread scheduling delay; neither value alone establishes the cause.

## RF levels, clocks and firmware

- IF overflow means measurements may be saturated. Raise the hardware reference or run explicit
  Auto Scale. Ordinary manual operation does not automatically alter gain or reference; Auto
  uses fresh data and has bounded recovery. The display viewport and hardware reference have
  different limits, so compare the actual readback when the display still looks clipped.
- Audio AGC, RF frontend gain and squelch are independent. Squelch uses the pre-AGC tuned-bin
  peak, not integrated channel power; the far-left −160 dBm setting disables it. Squelch is off
  by default for every mode, and Preset restores it to off. Decoder RMS,
  RF IQ power and the playback meter are different readings; see [User guide](USER_GUIDE.md).
- Zero-IF DC/LO leakage appears at the capture centre. Integer-GHz internal spurs can also
  appear in SWP/RTA/SDR, and their apparent level may rise with attenuation. Retune the capture
  before interpreting a centre-frequency feature as a received signal.
- SAN-90 external reference input is 10 MHz. `Ext` falls back to internal on unlock; `ExtForce`
  requires a valid reference. The separate vendor `SystemClockSource=External` setting is not
  exposed and can hang the device. Clock/output capabilities vary by model.
- GNSS 1PPS calibration requires a locked physical input; vendor calls can block without it.
  A zero firmware `RefClkFreqOffset` does not prove accuracy. GNSS/1PPS calibration and high-band
  soak qualification remain outstanding; IF AGC under saturation is also unqualified.
- Factory frequency-response calibration resides in firmware. Do not apply an external file
  through the vendor `Devcie_SetFreqResponseCompensation` call.

## Measurement and trigger limits

| Area | Current boundary |
|---|---|
| Trace resolution | SDK-native points determine spacing. Fixed `TraceBinSize_Hz` is unsupported; choose the speed/accuracy strategy. Display zoom adds no measurement resolution. |
| Channel power, OBW and ACPR | Results use the selected displayed absolute trace and its bin spacing. Reduce span or increase available points for narrow signals. Normalized traces do not supply absolute measurements or absolute CSV exports. |
| Normalization | The external sweep must cover the bandwidth and dwell at least one analyzer hop. CW normalizes only its own frequency effectively. |
| Phase noise | The 100 Hz and 10 MHz offset boundaries are inaccurate; minimum input is about −50 dBm. Check using a sweep span twice the maximum offset. |
| RTA level trigger | Requires a crossing; a steady signal already above threshold may keep waiting. The bus trigger source is supported. Timeout alone proves neither waiting nor completion; fixture input does not simulate hardware hits. |
| SWP level trigger | Browser detection compares consecutive sweeps, so one sweep is the minimum time unit. RTA delay, debounce, pre-trigger and POI do not apply. |

Use Free Run or Escape to release a confirmed trigger hold. Auto Scale does not release an
armed trigger. Missing upper-edge capture bins stay blank; RTA/SDR do not fill them using SWP
GapFill. Vendor ADM SINAD/SNR/THD require a dominant modulation tone and are not general
broadcast-audio quality measurements.

## Receiver and decoder limits

Opening Decode and Overview adds subscriptions, not receivers. Current and a saved same-frequency channel run independently. Digital reception does not depend on companion audio squelch. Use diagnostic JSON Copy to retain the current fault, recovery history and processing backlog.

The service permits sixteen receiver definitions and sixteen recording tasks. Disabled receivers
still occupy definition slots, and every full passband/transition must fit the shared capture.
Sustained sixteen-channel real-RF acceptance remains pending. CPU saturation, SDK stalls and
shared-source faults can interrupt reception; configured capacity is not a throughput guarantee.

| Mode | Current boundary |
|---|---|
| AM | Depth is single-tone-equivalent RMS envelope depth. Overmodulation can fold the envelope and under-report signed depth. Carrier fading invalidates metrics; Sync enabled does not imply lock. Carrier recovery is bounded by `min(IF bandwidth × 0.2, IQ rate × 0.1, 3000 Hz)`. |
| WFM/RDS | RDS text quality is limited; stereo indication means pilot detection, not proof of different left/right content. |
| FT8 | Supports standard message types 1/2 within its search window, not all message types or unrestricted search. |
| SSTV | Frequency offset comes from a valid VIS header and is not continuously tracked within a picture. Internal passband is 300–2700 Hz; post-image FSK callsigns are unsupported. |
| Radiofax | Automatic IOC 288/576 and 60/90/120/240 LPM; 180 LPM is manual. Segments contain at most 2400 rows. Other combinations, long-page drift, offset and fading require further qualification. |
| DRM | Short/looped audio can underrun. Codec audio bypasses AGC, NR, squelch and de-emphasis; volume and enable remain available. Full signalling reconfiguration and next-multiplex timing are incomplete. |

External providers have their own input requirements and supported catalogs; installation does
not establish signal lock. OnAir integration covers AIS/APRS/FT4/WSPR/Meteor/ADS-B plus a separate
RTTY companion, not its entire desktop catalog, digital voice or TV output. Its private loopback
IQ is fixed-scale 8-bit; AIS requires the fixed pair's 162 MHz centre. FT4/WSPR require UTC-aligned
live input and reject unaligned replay. EOF has bounded draining without upstream completion
acknowledgement; Meteor emits partial strips without an end-of-picture signal.

The optional GNSS-SDR bridge consumes exactly 4 MSps IQ, normally at 1575.420 MHz. It is separate
from the SDK GNSS status. `WEBSA_GPS_BENCH_HZ` permits one explicit translated 400–450 MHz receive
frequency; it does not configure transmission. Tracking, navigation and valid PVT are distinct,
stale fixes expire, and FIFO EOF completion remains unknown. Translated or synthetic reception
does not establish passive sky reception. See [provider setup](../../tools/README.md) and the
[protocol reference](reference/API.md) for rates and result fields.

## Browser retention and support evidence

Radiofax history is browser-session local, bounded to 20 MiB of allocated Gray8 per channel.
Current/viewed pages are protected; when they fill the budget, retention stops with a notice.
Save and delete pages to free space; stopped retention is not backfilled. Backend snapshots can
also omit older segments. Whole-page PNG export is limited to 32767 pixels per dimension and
16777216 pixels in total; export larger or incomplete pages by segment. Receipt dates identify
the first received row, not station transmission time or chart validity.

Generic provider history is limited to 200 results / 8 MiB per channel, including decoded strips,
and 512 strips per picture. Evictions are visible, late subscribers have no historical snapshot,
and generic images have no PNG export action. Native SSTV/Radiofax have separate contracts.
Popups share the opener's session and close with it. Acquisition rate does not guarantee browser
paint rate; display cost depends on the retained data, layers and host.

Direct `Pipeline`/`AnalogDemod` callers must provide finite input; production `Receiver` and
offline entry points validate it. For reproducible bug reports and new hardware/performance
evidence, follow [Development](DEVELOPMENT.md). Old run results are available in Git history and
do not certify the current build.
