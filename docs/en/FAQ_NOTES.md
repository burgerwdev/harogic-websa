# FAQ and Operations

## Starting and Accessing the Service

Run project scripts from the repository root:

```bash
make native sdk-shim
./build.sh
./run.sh
```

Root `./run.sh` starts the `websad` systemd user service with hardware on `127.0.0.1:8080`. Missing hardware prerequisites are errors. `WEBSA_FAKE=1 ./run.sh` explicitly selects fixture input. Direct invocation of the `websad` binary defaults to port 8099 and fixture input when no SDK shim is specified; this differs from the root launcher.

| Launcher variable | Default | Purpose |
|---|---|---|
| `WEBSA_HOST` | `127.0.0.1` | Listen address |
| `WEBSA_PORT` | `8080` | Root launcher port |
| `WEBSA_UNIT` | `websad` | User service unit |
| `WEBSA_FAKE` | `0` | Explicit fixture selection (`1`) |
| `WEBSA_RUST_IQS_SHIM` | `~/.local/share/harogic-websa/shims/libwebsa_san90_sdk_session.so` | Hardware shim |
| `WEBSA_STATIC` | Repository `frontend` | Directory containing `dist/index.html` |

`WEBSA_HOST=0.0.0.0 ./run.sh` enables LAN access at `http://device-address:8080/`. Product endpoints do not require authentication. Use `./status.sh` and `journalctl --user -u websad.service -n 100` to inspect the service; substitute the configured unit if overridden. `./stop.sh` stops the same unit.

## SDK Ownership and Recovery

Only one SDK call may access a device at a time. The retained native session serializes SDK work on its owner thread. Close SAStudio4 and other API clients before starting WebSA: the device cannot be shared. Configuration fields not explicitly set inherit SDK Profile defaults; continuous acquisition requires repeated Get calls.

Inspect STATUS errors and journal output before recovery. Stop competing applications, restart with `./stop.sh && ./run.sh`, and replug USB if the link remains unavailable. Wedged firmware may require a system restart. Process restarts are managed by systemd; device recovery is handled by native mode owners. No recovery timing is guaranteed here.

Harmonic/PNM sessions reject SWP-owned configuration commands while active, including frequency, reference, bandwidth, sweep, frontend and reference-clock changes. Stop the measurement before changing those settings.

## Spectrum and SDR Operation

SWP and RTA retain independent acquisition settings. SDK-effective points and bandwidths may differ from requests. The native SWP shim restricts the returned sweep to the requested frequency range, skips negative frequencies and marks unfilled zero-power bins as NaN. RTA Auto RBW uses the SDK actual value, not `span/2000`. See [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md) for ownership and transitions.

Frequency, resolution, sweep and clock controls retain the latest pending intent until matching acceptance or error; delayed STATUS must not overwrite a newer edit. Frequency acceptance uses hardware-effective bounds, and rapid span steps build on the latest requested window. Preset clears pending drafts only after a matched successful confirmation; failure preserves the prior UI. RTA disables SWP-only points, FFT window, detector, grid, spur and bandwidth-ratio controls; RTA bandwidth remains Auto or Manual.

SDR restores stored tuning, capture bandwidth, demodulator, IF bandwidth, de-emphasis, volume, squelch and AGC. Shift+click or “listen here” explicitly selects a new listening frequency. Leaving SDR stops playback but preserves the audio-enabled preference; Preset resets preferences only after successful confirmation. Non-SDR STATUS does not overwrite SDR preferences. WFM stereo defaults to enabled and delivers native two-channel audio; explicitly setting stereo false selects mono. DRM/DRM+ codec audio disables AGC, noise reduction, squelch and de-emphasis controls; audio enable and volume remain usable. SSTV Auto/pinned mode is one saved preference, replayed on SDR entry. Audio diagnostics are exposed in `#spectrum.dataset.sdrAudio`, including enabled/muted state, buffered time, underruns and RMS.

The AM window shows recovered carrier frequency (MHz), carrier offset (Hz), modulation depth (%), raw input level and final audio RMS/peak (dBFS), plus an actual final-PCM waveform on a fixed ±1 scale. Unknown values display “—”. The read-only title badge reports Sync off, searching or locked. Hovering it shows source timing, alignment, delay and decoder errors; AM has no separate timing label. The Synchronous toggle sits at the right of the carrier-frequency row. A pending request adds “enabling…” or “disabling…” with a dashed outline; it does not change the confirmed detector state or claim carrier lock. Rejection or timeout restores the confirmed state. Sync enabled means synchronous detection is selected; carrier lock additionally requires valid DSP carrier evidence. Carrier offset, modulation and input power are smoothed using source time. Reload restores the saved confirmed `am_sync` preference on SDR entry; successful Preset resets it to false. Start, disconnect, leaving AM or a stream warning clears old telemetry.

Synchronous AM mixes IQ with the recovered carrier; envelope AM takes its magnitude. With a clean carrier and no envelope reversal, both recover nearly the same programme audio, so switching need not sound different. Controlled overmodulation or quadrature contamination distinguishes the paths; synchronous detection is not a general noise-reduction switch. The recovered carrier resolves the coherent path's polarity during acquisition and retains it while locked, preventing opposite-polarity cancellation during detector crossfades.

The audio RMS display averages linear power over 300 ms. Audio peak indication attacks immediately and releases at 20 dB/s. These display ballistics leave PCM, waveform samples and raw telemetry unchanged; stream resets and unavailable telemetry clear the meter immediately.

The squelch slider uses 0.1 dB steps and displays the requested RF threshold. Its far-left position (−160 dBm) disables squelch; higher thresholds require stronger signals. The receiver uses the SDK spectrum's tuned-bin peak before audio AGC, with hysteresis, hold and a soft audio fade. This is a nominal SDK dBm measurement, not integrated channel power. SDR step quick-selection buttons keep Left/Right tuning available immediately after a click; numeric inputs retain their own editing keys.

Trace detector selection applies only to SWP and is rejected during RTA or harmonic/PNM measurements. Auto Sample follows signal type; Sample, Pos Peak, Neg Peak and RMS select fixed detectors. Auto Peak is intended for pulsed signals and is available through the API rather than the UI. Hold span, RBW and points constant when comparing detector results.

## Reference Level and Overflow

Manual `SET_REF` requests use -160 to +40 dBm. Edits remain pending until matching acceptance; a correlated error or timeout restores the accepted display. SDR `sdr.ref` confirms the stored hardware request, while `ref` reports SDK-effective reference and can differ after clamping. SDR display reference is separate from IQS reference. Level controls are disabled during harmonic/PNM measurement.

Explicit `AUTO_SCALE` starts a bounded session (20 seconds, at most 24 steps). Only fresh acquisition evidence from the same mode and configuration advances it. An SDK IF-overflow warning (`APIRETVAL_WARNING_IFOverflow`, -12) allows a recovery step from the active SDK actual reference by +10 dB, capped at the reported reference maximum; an actual reference already above that maximum is never lowered. Attenuation, preamp, IF gain and strategy choices are preserved. Valid IQ returned with the warning is retained. Ordinary missing data or trigger waiting does not raise reference, and manual operation has no background reference adjustment.

Auto fits only a clean spectrum and requires fresh clean evidence after configuration changes before confirming completion. SDR explicit Auto can reconfigure IQS for overflow recovery; SDR entry uses `display_only:true` and never changes hardware. Manual configuration, frequency, mode and Preset changes cancel old sessions and targets. `auto_ref` exposes progress and terminal reasons, including limit, stalled, failed, no_data and timeout; unsuccessful completion has no target. See [API](API.md) for the full result schema.

Amplitude offset is applied once to displayed absolute levels (axis, markers, peaks and channel readouts). Differences such as dBc remain differences; device reference, attenuation and trigger thresholds retain instrument units. Actual gain readbacks belong to the active SDK mode; unavailable fields remain null. Preamp Auto/Off indicates the selected policy, not physical on/off state. Waterfall level ranges must be finite and valid; changing the level mapping clears history even while paused.

## Reference Clock and Firmware

- `ReferenceClockSource`: 0=Internal, 1=External with fallback when unlocked, 2=Internal + DOCXO, 3=External Forced. Premium clock availability depends on the device.
- The SAN-90 external reference input is 10 MHz. The native shim sets `ExternalSystemClockFrequency` and the external reference frequency to 10 MHz.
- Reference output support depends on the model. Clock changes configure the active profile; RTA remains RTA.
- Do not select `external_forced` without a valid external reference. `SystemClockSource=External` is a separate vendor setting and is not exposed by WebSA.
- GNSS 1PPS calibration requires a real locked 1PPS input; vendor calls can block without it.
- Some firmware reports zero `RefClkFreqOffset`; a zero report does not establish clock accuracy. Use a calculated calibration result when available.
- SAN-45/60/90 nominal upper frequency limits are 4.5/6/9 GHz, with a 9 kHz lower limit; specifications and supported features vary by model.

## Trigger and Measurement Questions

RTA uses device triggering; SWP software triggering keeps the plot live until a crossing between consecutive sweeps. Debounce, delay and pre-trigger timing apply only to RTA. RTA requested settings and SDK-applied readback are separate; capture is held only after a complete frame is rendered. Vendor timeout alone does not prove trigger waiting, and unsupported or unknown capture state is shown explicitly. Fixture input does not simulate hardware hits. A confirmed hit remains frozen until Free Run or Escape; Auto Scale does not release an armed trigger.

PNM results update incrementally on successive Get calls. Results near the 100 Hz and 10 MHz offset boundaries can be inaccurate; a sweep span twice the maximum offset provides a cross-check. Typical minimum PNM input power is -50 dBm.

Each harmonic/PNM Set clears the old plot, table and accumulators. Results must match the newest confirmed command session; old acquisition cycles cannot populate a new measurement. PNM averaging has an independent control row, and disabling PNM disables all its Set/Apply actions. Limits and channel measurements use the selected processed absolute trace; normalized traces are unavailable for these absolute readings and absolute CSV export. RTA/SDR disable these measurement controls, with standard mode restoring them. dBc readings do not receive amplitude offset.

The n-dB keypad edits a comma-separated list of integer thresholds. Backspace deletes at the caret, C clears the entry, and the decimal key enters a comma. OK normalizes leading, trailing and repeated commas.

Development checks and hardware verification procedures are documented in [DEVELOPMENT.md](DEVELOPMENT.md); current limitations are in [KNOWN_ISSUES.md](KNOWN_ISSUES.md).
