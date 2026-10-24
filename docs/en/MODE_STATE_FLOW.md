# Mode, Parameter and State Ownership

## Parameter Ownership

The Rust service in `native-ddc/` owns device configuration and acquisition. The browser owns display settings, editors and playback.

| Setting | SWP | RTA | SDR |
|---|---|---|---|
| Center / bandwidth | Independent sweep center/span | Independent capture center/span | Capture center/decimation and receiver listen frequency |
| Reference | SWP SDK reference | RTA SDK reference | IQS reference set explicitly with `SET_REF`; separate browser display reference |
| RBW / VBW / sweep | SWP request and actual values | RTA request and actual values | Capture and receiver rate plans |
| Points / FFT window / spur | SWP only | Not applicable | Not applicable |
| Attenuation / preamp / IF gain / gain strategy | Shared RF frontend | Shared RF frontend | Shared RF frontend |
| Reference clock / clock output | Device-level | Device-level | Device-level |

SWP and RTA requests remain independent across mode changes. Display averaging depth is also independent (`avgTarget` / `avgTargetRta`): finite N uses exponential averaging with α = 2/(N+1); infinity uses a cumulative mean.

## Defaults and Preset

| Setting | SWP startup | RTA startup |
|---|---|---|
| Center / span | 1 GHz / 100 MHz | 1 GHz / 50.78125 MHz |
| Reference | 0 dBm | 0 dBm |
| RBW / VBW | 100 kHz / 100 kHz, manual | Auto / Equal |
| Sweep | minSWT (`0`) | minSWT ×4 (`2`) |
| Points | 1000 requested; SDK selects actual | 1001 display points |

SWP starts with Blackman-Nuttall window (`1`), spur bypass, automatic attenuation/preamp, IF gain 2 and Low Noise gain strategy. SDK-effective RBW, VBW and point counts must be read from STATUS rather than derived from display geometry.

`SET_PRESET` restores cached device SWP defaults from `SWP_ProfileDeInit`, normalizing full span to model frequency limits, and restores the fixed RTA defaults. Frame Avg returns to `device_default`; the SDK detector default is reported separately in `preset_defaults`. Preset resets SDR capture/receiver parameters and clears the explicit IQS reference flag (`sdr_ref_set=false`). The browser owns the SDR display scale and requests a fresh display fit.

Preset keeps the confirmed acquisition mode, including its browser reload preference. Reload restores that mode from storage and waits for backend confirmation. SDR entry arms its display-only fit after entry tuning commands complete. If `sdr.ref_set` is already true, entry preserves the stored hardware level and does not fit; pressing Auto explicitly requests a new decision.

## STATUS and Commands

- Top-level frequency, reference, bandwidth and sweep fields describe the active mode.
- `req.swp` / `req.rta` retain mode-specific requests; `swp_actual` / `rta_actual` report successful SDK-effective settings. `actual` identifies the active actual object.
- `config_version` identifies configuration changes; `response_to` associates a command-response STATUS with its command.
- Frequency editors retain dirty text across periodic STATUS. A successful command response supplies effective frequency values.
- Gain readbacks describe the active mode's successfully configured SDK values and are null when unavailable. Preamp `0` is Auto and `1` is Off; Auto does not prove physical On.

Continuous edits accumulate from the latest desired value. Frequency commits carry `request_id`; only the matching latest ACK/ERROR settles that commit, while a newer unsent draft remains protected. Their own three-second deadline is independent of `config_version`. A matching ACK displays the SDK-effective clamp. RBW/VBW numeric and mode edits share one pending epoch and survive blur/periodic STATUS; sweep mode, time and visibility settle or roll back together. Clock source/output follow the same latest-intent rule. Browser Preset resets occur only on the matching successful STATUS, clearing acquisition drafts/pending state; failure preserves the previous UI state.

RTA disables points, FFT window, detector, frame/grid and spur controls. RBW ratios belong to SWP; RTA offers Auto/manual RBW using device-supported choices.

`SET_FREQ` accepts either `{center, span}` or `{start, stop}`, never both. Center/span preserves the requested center and reduces span to fit the device limits. Start/stop requires Stop > Start and a span of at least 100 Hz. Full Span explicitly selects the full-band midpoint.

In RTA, `SET_RTA` sets center/span; span maps to `50.78125 MHz / 2^n`. `SET_RBW`, `SET_VBW`, `SET_SWEEP` and `SET_REF` update RTA-owned settings. Shared RF and reference-clock changes configure the active mode's profile.

## Mode Transitions

The browser sends `SET_MODE` and waits for STATUS before changing views. It does not replay stored SWP/RTA acquisition settings on entry. The service stops the previous acquisition owner and configures the destination from its retained settings. Returning to SWP restores its independent request; returning to RTA restores its RTA request.

SDR preferences include tuning, capture bandwidth, demodulator, IF bandwidth, de-emphasis, volume, squelch and AGC. A normal return restores these preferences. Shift+click or “listen here” explicitly hands over the selected frequency; first entry without stored settings derives tuning/demodulation defaults from the swept view. Leaving SDR stops playback while retaining the audio-enabled preference; Preset resets preferences.

The AM `am_sync` preference defaults to false (envelope detection), remains stored when another demodulator is selected and is restored with SDR preferences. A successful Preset restores false. Changing only this preference is a live detector update, preserving capture configuration, stream identity and source/audio clock continuity.

## Reference and Display State

Manual `SET_REF` configures the active SDK reference in the -160 to +40 dBm request domain. The editor tracks a pending request, confirms the SDK-effective STATUS value and rolls back on failure. Capability reference bounds constrain Auto targets rather than manual requests.

Explicit `AUTO_SCALE` runs a bounded recovery/placement operation (at most 20 seconds and 24 steps), consuming only fresh observations from its current mode and configuration. On SDK IF overflow (`-12`), it raises the active actual reference by 10 dB, capped by the capability reference maximum, without changing attenuation, preamp or IF gain. Clean data permits display fitting; a fresh clean observation confirms the result. Manual edits cancel the operation. Outside explicit Auto, overflow is a warning and preserves manual reference. Display-only entry fitting never changes hardware reference.

Amplitude offset is applied once to displayed absolute levels, including markers, peaks and absolute measurement results. Device reference and trigger thresholds remain in dBm. RTA/SDR density uses processed active-trace samples and native frequency positions, sharing trace offset/reference mapping; Off stops accumulation and display. Waterfall bounds must be finite with low < high; invalid edits are rejected. Mapping changes clear waterfall history.

SDR initially loads with AVERAGE 16; custom 64/infinite averaging survives leaving and returning to that acquisition family. Preset restores T1 CLEAR_WRITE 16 for standard/SDR without switching mode. SWP waterfall rows come only from accepted new POWR data, using raw power with its matching frequency grid and at least 100 ms between samples; redraws do not add rows. RTA/SDR waterfall consumes raw RTA frames. CSV exports the active family's selected trace and matching frequency array; an Off or missing active trace reports no data instead of falling back to another trace.

## Measurement State

Measurement controls and their handlers are disabled in RTA/SDR. In standard mode, limits and channel measurements consume only the selected mode-processed absolute trace; normalized traces are N/A and produce no absolute CSV rows. They never fall back to another trace or raw data. Offset is not applied to relative dBc results. Cal Wnd alone rebuilds the existing absolute calibration reference; Normalize/Reset repaint the resulting state.

PNM has an independent visible averaging row. Turning measurement Off disables all three PNM Set/Apply controls. Every PNM/Harm Set clears the old curve, table and accumulators, and sends a unique `session_id`. Only a successful STATUS matching `response_to` and `session_id` confirms that application; results must carry the newest confirmed session ID before acceptance. Delayed hardware cycles retain their original ID, so stale or inactive results cannot repopulate the display.

## Trigger State

| Mode | Free | Waiting | Hit |
|---|---|---|---|
| RTA device trigger | Bus source; live | Level Capture armed; Stop | Hold only after rendering a packet with confirmed completion |
| SWP software trigger | Live; Capture | Plot remains live; Stop | Frozen crossing with frequency/level; Capture again |

`Free Run` or Escape releases capture. SWP compares consecutive sweeps using level/edge; grid changes clear crossing history. RTA confirms and renders all applied trigger fields from STATUS without sending commands during GET/STATUS synchronization. Advanced native controls are disabled in SWP; trigger controls are disabled in SDR, harmonic and phase-noise views. External/timer sources are reflected read-only rather than expanded into new UI workflows.

RTA2 carries a per-frame `TRIG` trailer with applied trigger state, configuration generation, successful packet count and completion metadata. Only finite Level captures with established continuity can report completion; adaptive/timer/retrigger or lost continuity may remain unknown. A frame arrival or 500 ms delay never implies completion. A missed trigger is distinct from a normal hit; fake data reports unsupported and cannot establish a hardware hit. Legacy RTAF decoding remains for compatibility; SDR still uses RTAF. Finite Level capture spectra aggregate maxima across the capture. Valid accepted capture packets are rendered before holding; the canvas is not cleared speculatively. Confirmed mode changes and successful Preset clear local arm/hold, private phase and pending trigger state.

Tracking markers use the active processed SWP/RTA trace. Disabling a marker retains its frequency and tracking preference; axis changes relocate markers by frequency before tracking resumes.

See [API.md](API.md) for wire fields and commands, and [FAQ_NOTES.md](FAQ_NOTES.md) for operating guidance.
