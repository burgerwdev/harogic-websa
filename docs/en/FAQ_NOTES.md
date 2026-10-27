# FAQ and Operations

## Starting and Accessing the Service

Run project scripts from the repository root:

```bash
make native sdk-shim
./build.sh
./run.sh
```

Root `./run.sh` starts a detached `websad` background process with hardware on `127.0.0.1:8080`, without systemd. Missing hardware prerequisites are errors. `WEBSA_FAKE=1 ./run.sh` explicitly selects fixture input. Direct invocation of the `websad` binary defaults to port 8099 and fixture input when no SDK shim is specified; this differs from the root launcher.

| Launcher variable | Default | Purpose |
|---|---|---|
| `WEBSA_HOST` | `127.0.0.1` | Listen address |
| `WEBSA_PORT` | `8080` | Root launcher port |
| `WEBSA_INSTANCE` | `websad` | Checkout-scoped process instance (`--instance NAME`) |
| `WEBSA_FAKE` | `0` | Explicit fixture selection (`1`) |
| `WEBSA_RUST_IQS_SHIM` | `~/.local/share/harogic-websa/shims/libwebsa_san90_sdk_session.so` | Hardware shim |
| `WEBSA_STATIC` | Repository `frontend` | Directory containing `dist/index.html` |

`WEBSA_HOST=0.0.0.0 ./run.sh` enables LAN access at `http://device-address:8080/`. Product endpoints do not require authentication. `./status.sh` reads the saved host/port and prints the process, log path and HTTP state; use `--instance NAME` for a named instance. `./stop.sh` stops only that validated instance and succeeds when it is already stopped. State and logs live under `$XDG_RUNTIME_DIR/harogic-websa`, or `${XDG_STATE_HOME:-$HOME/.local/state}/harogic-websa` when no owned runtime directory is available.

## SDK Ownership and Recovery

Only one SDK call may access a device at a time. The retained native session serializes SDK work on its owner thread. Close SAStudio4 and other API clients before starting WebSA: the device cannot be shared. Configuration fields not explicitly set inherit SDK Profile defaults; continuous acquisition requires repeated Get calls.

Inspect STATUS errors and the instance log before recovery. Stop competing applications, restart with `./stop.sh && ./run.sh`, and replug USB if the link remains unavailable. Wedged firmware may require a system restart. The launcher does not automatically restart a failed process; device recovery is handled by native mode owners. No recovery timing is guaranteed here.

Device and command errors appear in one diagnostic slot at the top right of the plot, without a blocking browser dialog. Identical errors do not accumulate; a different error replaces the displayed diagnostic. Hover the plot for the full message. Periodic STATUS, unrelated successful commands, and transient reference notices retain the error. A rejected command clears after a later successful command in the same recovery scope, or after a confirmed `SET_MODE`/`CONNECT`. SDR capture and primary tuning (`SET_SDR`, `SET_SDR_TUNE`, and primary `SET_SDR_CHANNEL` frequency edits) share a recovery scope; other channels and non-frequency channel edits do not clear a tuning failure. The browser supplies missing request IDs and matches both the command and request ID, using send order so stale acknowledgements cannot clear newer failures. Socket reconnection alone retains the diagnostic and retires old pending replies. Errors without command identity remain device diagnostics. Command rollback and the separate receiver-failure recovery rules remain in effect.

A shared SDR receiver worker failure stops all audio and decoding. The Decode row shows “Receiver failed”; hover for the full diagnostic. The error remains visible when the floating decoder is closed. Audio and decode connections close with “receiver failed”, distinct from slow-consumer overflow. Capture, raw IQ, spectra, waterfall, tuning and zoom remain available. Capture and device faults use global error handling.

Select a demodulator with the existing controls to retry, including the current mode. `receiver_retiring` means stopped workers have not exited or failed subscriptions have not closed; retry later. Recovery is not automatic and replacement thread count is bounded. Successful recovery starts fresh streams without joining samples across gaps. Until then, STATUS retains `sdr.receiver_error`. If failure recurs, inspect this diagnostic and the service log.

Harmonic/PNM sessions reject SWP-owned configuration commands while active, including frequency, reference, bandwidth, sweep, frontend and reference-clock changes. Stop the measurement before changing those settings.

## Spectrum and SDR Operation

SWP and RTA retain independent acquisition settings. SDK-effective points and bandwidths may differ from requests. The native SWP shim restricts the returned sweep to the requested frequency range, skips negative frequencies and marks unfilled zero-power bins as NaN. RTA Auto RBW uses the SDK actual value, not `span/2000`. See [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md) for ownership and transitions.

Frequency, resolution, sweep and clock controls retain the latest pending intent until matching acceptance or error; delayed STATUS must not overwrite a newer edit. Frequency acceptance uses hardware-effective bounds, and rapid span steps build on the latest requested window. Preset clears pending drafts only after a matched successful confirmation; failure preserves the prior UI. RTA disables SWP-only points, FFT window, detector, grid, spur and bandwidth-ratio controls; RTA bandwidth remains Auto or Manual.

SDR restores stored tuning, capture bandwidth, demodulator, IF bandwidth, de-emphasis, volume, squelch and AGC. From another view, Shift+click or “listen here” enters SDR at the selected frequency. A rejected or timed-out tune request restores the confirmed frequency. Leaving SDR stops playback but preserves the audio-enabled preference; Preset resets preferences only after successful confirmation. Non-SDR STATUS does not overwrite SDR preferences. WFM stereo defaults to enabled and delivers native two-channel audio; explicitly setting stereo false selects mono. DRM/DRM+ codec audio disables AGC, noise reduction, squelch and de-emphasis controls; audio enable and volume remain usable. SSTV Auto/pinned mode is one saved preference, replayed on SDR entry. Audio diagnostics are exposed in `#spectrum.dataset.sdrAudio`, including enabled/muted state, buffered time, underruns and RMS.

In SDR, click the plot to tune with the selected Locate mode. Free uses the clicked frequency. Channel snaps to the channel plan; selecting Channel reveals spacing and anchor inline, with no separate Grid button. Steps opens tuning-step settings separately. Signal selects a stable AM or CW carrier from the live native spectrum. A valid Channel or Signal target shows one preview line and one frequency label. Free has no moving preview line, and there is no permanent Signal hint row. Signal can hold its preview through a short dropout, but clicking requires current evidence for the same carrier.

The spectrum's top frequency handle tunes; the passband edge grips adjust bandwidth. Both remain usable with Zoom on, while drags in the rest of the plot select a zoom rectangle. With Zoom off, the full editable edge adjusts bandwidth and other plot drags tune freely. Use View when the passband is too narrow to grab. Ctrl+click/drag and existing marker diamonds operate markers. Mode or Zoom changes, pointer cancellation, Escape and window blur cancel without committing.

In the SSTV window, Clear opens an inline Delete all / Cancel choice while reception continues. Delete all removes pictures for the selected channel; Cancel preserves them. Closing the window, leaving SSTV, changing channel or SSTV view/mode, or disconnecting cancels the pending choice.

The AM window shows recovered carrier frequency (MHz), carrier offset (Hz), modulation depth (%), raw input level and final audio RMS/peak (dBFS), plus an actual final-PCM waveform on a fixed ±1 scale. Unknown values display “—”. The read-only title badge reports Sync off, searching or locked. Hovering it shows source timing, alignment, delay and decoder errors; AM has no separate timing label. The Synchronous toggle sits at the right of the carrier-frequency row. A pending request adds “enabling…” or “disabling…” with a dashed outline; it does not change the confirmed detector state or claim carrier lock. Rejection or timeout restores the confirmed state. Sync enabled means synchronous detection is selected; carrier lock additionally requires valid DSP carrier evidence. Carrier offset, modulation and input power are smoothed using source time. Reload restores the saved confirmed `am_sync` preference on SDR entry; successful Preset resets it to false. Start, disconnect, leaving AM or a stream warning clears old telemetry.

Synchronous AM mixes IQ with the recovered carrier; envelope AM takes its magnitude. With a clean carrier and no envelope reversal, both recover nearly the same programme audio, so switching need not sound different. Controlled overmodulation or quadrature contamination distinguishes the paths; synchronous detection is not a general noise-reduction switch. The recovered carrier resolves the coherent path's polarity during acquisition and retains it while locked, preventing opposite-polarity cancellation during detector crossfades.

The audio RMS display averages linear power over 300 ms. Audio peak indication attacks immediately and releases at 20 dB/s. These display ballistics leave PCM, waveform samples and raw telemetry unchanged; stream resets and unavailable telemetry clear the meter immediately.

The squelch slider uses 0.1 dB steps and displays the requested RF threshold. Its far-left position (−160 dBm) disables squelch; higher thresholds require stronger signals. The receiver uses the SDK spectrum's tuned-bin peak before audio AGC, with hysteresis, hold and a soft audio fade. This is a nominal SDK dBm measurement, not integrated channel power. Arrow keys tune by the step shown in the Step box; Shift and Ctrl switch to the coarse and fine tiers, each remembered per demodulation mode and independent of the capture bandwidth. Step quick-select buttons keep Left/Right tuning available immediately after a click; numeric inputs retain their own editing keys, and the shortcut is inactive while an input, dialog or the keypad has focus.

AGC On/Off automatically levels demodulated listening audio; Off retains fixed detector-to-PCM gain. It does not change the RF front end or squelch threshold. The current implementation exposes no speed or target control. Its command changes only `agc`; matching acknowledgement settles the request, and rejection, timeout or session exit clears pending intent.

Other receivers can expose additional AGC controls. [FlexRadio's SmartSDR guidance](https://helpdesk.flexradio.com/hc/en-us/articles/360029494371-How-Does-the-Automatic-Gain-Control-AGC-Function-in-SmartSDR) describes FAST/MED/SLOW as recovery speed after a strong signal and AGC-T as maximum applied gain. Those controls belong to that receiver's implementation; this application's fixed audio AGC does not implement those settings.

Trace detector selection applies only to SWP and is rejected during RTA or harmonic/PNM measurements. Auto Sample follows signal type; Sample, Pos Peak, Neg Peak and RMS select fixed detectors. Auto Peak is intended for pulsed signals and is available through the API rather than the UI. Hold span, RBW and points constant when comparing detector results.

## Reference Level and Overflow

Manual `SET_REF` requests at the analyzer input use -160 to +40 dBm. SWP, RTA and SDR have an independent local Ref viewport from -200 to +100 dBm before external Offset. With Offset zero, entering or dragging to +100 displays +100 while requesting hardware +40; +110 clamps the display to +100. Moving between +90 and +100 reuses the same hardware request, including while its acknowledgement is pending. A matching raw acknowledgement accepts the viewport without replacing it. Rejection or timeout restores the last accepted viewport. Offset shifts the limits, axis and absolute signal readouts together, preserving trace position without a hardware command; it is not required to reach +100. SDK-effective `ref` remains separate; hover the Ref input or readout to compare it. Auto replaces manual placement; changing mode or reloading restores the active hardware request plus Offset. Preset clears manual ownership and SDR fits once on fresh data. IF overflow warns but does not block manual edits. Ref changes preserve gain selections. SDR `sdr.ref`, SWP `req.swp.ref` and RTA `req.rta.ref` store uncompensated requests. Level controls are disabled during harmonic/PNM measurement.

SWP retains complete new sweeps returned with IF overflow and keeps the warning visible. Saturated measurements can be inaccurate. If the SDK does not return a complete new sweep, no replacement frame is published; the previous buffer is not replayed. Raising Ref can clear the warning, but the application does not raise it automatically during manual operation.

Explicit `AUTO_SCALE` starts a bounded session (20 seconds, at most 24 steps). Only fresh acquisition evidence from the same mode and configuration advances it. An SDK IF-overflow warning (`APIRETVAL_WARNING_IFOverflow`, -12) allows a recovery step from the active SDK actual reference by +10 dB, capped at the reported reference maximum; an actual reference already above that maximum is never lowered. Attenuation, preamp, IF gain and strategy choices are preserved. Valid IQ returned with the warning is retained. Ordinary missing data or trigger waiting does not raise reference, and manual operation has no background reference adjustment.

Auto fits only a clean spectrum and requires fresh clean evidence after configuration changes before confirming completion. SDR explicit Auto can reconfigure IQS for overflow recovery; SDR entry uses `display_only:true` and never changes hardware. Manual configuration, frequency, mode and Preset changes cancel old sessions and targets. `auto_ref` exposes progress and terminal reasons, including limit, stalled, failed, no_data and timeout; unsuccessful completion has no target. See [API](API.md) for the full result schema.

Amplitude offset is applied once to displayed absolute levels (axis, markers, peaks and channel readouts). Differences such as dBc remain differences; device reference, attenuation and trigger thresholds retain instrument units. Actual gain readbacks belong to the active SDK mode; unavailable fields remain null. Preamp Auto/Off indicates the selected policy, not physical on/off state. Waterfall level ranges must be finite and valid; changing the level mapping clears history even while paused.

Attenuation, preamp policy, IF gain grade and gain strategy are independent requests. Each selector confirms its own command; an older reply cannot settle a newer edit. Rejection, timeout or disconnect clears the pending selection. Actual readbacks describe the configured SDK profile. Preamp Auto is a policy, not proof of a physical switch state, and IF grade is not SDR audio AGC.

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

The n-dB keypad edits a comma-separated list of integer thresholds. Backspace deletes at the caret, C clears the entry, and the decimal key enters a comma. OK normalizes leading, trailing and repeated commas. If no threshold is within 1–60 dB, an inline error appears beside the input and the previous measurement stays intact; correcting the input clears the error.

Development checks and hardware verification procedures are documented in [DEVELOPMENT.md](DEVELOPMENT.md); current limitations are in [KNOWN_ISSUES.md](KNOWN_ISSUES.md).
