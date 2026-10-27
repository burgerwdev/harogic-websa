# Architecture

## System and ownership

The native Rust service owns device configuration, acquisition, channelization and receiver DSP. A C++ shim serializes the official HAROGIC SDK. The browser owns controls, spectrum rendering, decoder presentation and audio playback. Python modules under `tools/reference/` are offline numerical references and fixture helpers.

```text
Browser controls → HTTP/WS commands → native mode owner → retained SDK session
SDK IQ + FFT → bounded capture queue → ordered PFB workers → fine DDC/framer
                                                   ├→ IQBF/IQAX consumers
                                                   └→ receiver workers → AUD2/SDE2 → browser
SDK SWP/RTA traces → display publisher → browser spectrum/density/waterfall
```

SWP, RTA, SDR, harmonic and phase-noise modes keep their own configuration while sharing one physical device session. Effective device values and capabilities are returned in STATUS. Command validation and candidate-plan admission complete before replacing active state; a rejected request preserves the prior plan and stream.

## Device session and acquisition

One persistent SDK owner thread serves SWP/RTA/IQS/PNM. All SDK calls are serialized. Healthy changes among acquisition modes stop acquisition and release mode state while retaining the physical handle. `Device_Open` receives the permanent session's stable `void**` handle slot: vendor PNM workers retain that address until physical close. A stale/disconnected handle must not be passed to `Device_Close`.

IQS configuration and arming are separate. The service prepares PFB, fine DDC, receiver workers and metadata before explicit arm, including recovery. It keeps the first packet and checks packet order and hardware timestamps. Host sample indices do not substitute for hardware continuity checks.

A valid IQS packet returned with `-12` is retained with its timestamp; saturation is reported separately as `status_warning=-12`. Outside an explicitly active Auto operation, saturation preserves manual Ref and does not turn a valid packet into a bus failure. Explicit Auto recovery is bounded and uses fresh observations from the same mode/configuration; see [MODE_STATE_FLOW](MODE_STATE_FLOW.md). IQ, FFT, warning and timing return through one `san90_next_v3` call; routine metadata polling does not consume IQ. Malformed packets and real timestamp gaps fail explicitly.

A sustained SDK stall ends capture with the original cause. Recovery requires a new capture epoch; it cannot continue the old hardware timestamp sequence in place. Queue backlog waits under bounded backpressure without reopening the device, until an explicit backlog failure. A stuck vendor owner remains parked; SDK finish and reopen are refused while the owner remains in a vendor call.

Routine GNSS status uses a cached SDK snapshot. Device and optional clock capabilities come from successful open/hardware-state results; absent data stays unknown. External reference configuration uses 10 MHz. Recovery invalidates the physical session explicitly; shutdown joins mode owners before closing the session and SDK thread.

Server is an exclusive handoff rather than another Web IQ consumer. Entering stops and joins Web acquisition/receiver work, then `san90_session_release_v1` closes the idle physical handle while leaving the SDK owner thread reusable. A missing release ABI or parked vendor owner refuses handoff; already retired streams stay retired and recovery creates a new epoch. `backend/src/bin/websad/server_mode.rs` owns the SoapySDRServer child, dependency probe, bounded raw logs and lifecycle status. Its dedicated spawning thread remains alive until the exact child is reaped and arms parent-death cleanup. Exit must stop and reap that child before Web acquisition can reopen. Browser lifetime does not own the process. Server publishes `connected=false`, `gnss=null` and empty active actuals, and does not poll the SDK or share Web IQ. SoapyRemote clients open the device through the patched official SoapyHarogic module; listening readiness alone does not prove a device open or RF reception.

SoapyRemote starts its reader during setup. The official-driver compatibility patch therefore exposes a stable, nonzero 16384-sample MTU before activation and makes reads before activation or after deactivation honor the caller's timeout. Without these contracts, remote clients receive zero samples or stop on a premature `STREAM_ERROR`. This transfer bound is independent of the SDK packet size.

The patched driver retains SDK samples in their raw CS8/CS16/CS32 width and writes the requested CF32/CS16/CS8 output, bulk-copying matching integer formats. SDK width is selected at stream setup and stays fixed until close; CF32 conversion normalizes by the source width. Before setup, the native-format query defaults to CS16 through 60 MSps (`fullScale` 32768) and CS8 above it (`fullScale` 128). After setup it reports the selected output format and its `fullScale` (CF32: 1, CS16: 32768, CS8: 128). `native_format=AUTO` matches integer output width; for CF32 it selects CS16 through 60 MSps and CS8 above it using the setup rate. Explicit integer output requires a matching SDK width. USB enumeration uses `Device_List` without opening devices. The constructor opens one streaming-capable handle for identity, capabilities and subsequent streams; deactivation and stream close retain it, and device destruction releases it. The host ring holds about 50 ms, with a two-SDK-packet/16384-sample floor, and reuses allocated storage. Active reads return available samples up to the requested count. Unchanged settings skip SDK reconfiguration. OnAir can destroy and recreate its Soapy device on mode/rate changes, which still requires a physical SDK open. See the [tools guide](../../tools/README.md#soapy-server-driver-setup).

## SDR channelizer and receivers

### Stage map

These are current implementation paths. `tools/checks/check_doc_paths.py` checks the concrete paths in this table.

| Stage | Module |
|---|---|
| Device session and acquisition | `backend/sdk/san90_sdk.cpp`, `backend/src/bin/websad/service_capture.rs` |
| Service, commands and publication | `backend/src/bin/websad.rs`, `backend/src/bin/websad/service_ddc.rs` |
| Coarse PFB and ordered workers | `backend/src/pfb.rs`, `backend/src/pfb_pool.rs` |
| Fine DDC and sample clocks | `backend/src/fine.rs`, `backend/src/narrow.rs`, `backend/src/rate_plan.rs` |
| Pipeline orchestration and admission | `backend/src/bin/websad/sdr_pipeline.rs`, `backend/src/bin/websad/service_preflight.rs` |
| Native receiver orchestration | `backend/src/bin/websad/receiver_workers.rs`, `backend/src/receiver.rs` |
| Analog and digital DSP | `dsp/core/src/analog/`, `dsp/core/src/digital/`, `dsp/core/src/pipeline.rs` |
| Native noise reduction | `backend/src/nr.rs`, `dsp/deepfilter/src/` |
| Binary wire and subscriber boundaries | `backend/src/wire.rs`, `backend/src/bin/websad/service_stream.rs`, `backend/src/bin/websad/subscriber_stream.rs` |
| Browser stream and decoder presentation | `frontend/src/sdr/iqStream.ts`, `frontend/src/sdr/decodeWorker.ts`, `frontend/src/ui/decodeWindow.ts` |
| Playback | `frontend/src/audio/sdrAudio.ts`, `frontend/src/audio/sdrAudioWorker.ts`, `frontend/src/audio/sdrAudioWorklet.js` |
| Offline reference and fixtures | `tools/reference/`, `tools/fixtures/gen_dsp_fixtures.py`, `tests/fixtures/dsp/` |

### Coarse PFB and fine DDC

At full capture bandwidth the input is 62.5 MSps with 50 MHz usable bandwidth. The shared oversampled-by-two PFB has 128 bins and hop 64: bin spacing is 488.28125 kHz, while each coarse stream runs at 976.5625 kSps. Narrow device captures adjust geometry. Bin count, coarse spacing and coarse sample rate are different quantities; none is the number of product receivers.

The prototype supports 4, 8 or 16 taps per phase, with 16 as the service default. Passband and alias rejection depend on this selection and frequency offset. Fine plans must fit the selected coarse bin's usable passband. A wide signal cannot be cut at a bin edge and treated as equivalent reception. Wideband modes beyond the supported channel envelope are rejected.

Each receiver has its own frequency, mode, occupied bandwidth, `RatePlan`, oscillator phase, filter history and output rate. Fine DDC mixes and performs multistage anti-alias decimation; an exact protocol rate is adapted at the receiver input when needed. Audio playback rate is not a universal lower bound on IQ rate. USB/LSB envelopes include the sideband edge, CW includes pitch, FT8 retains at least the positive 3.2 kHz band, and SSTV the positive 3 kHz band. The UI selects a 12 kHz DRM receive window, while a direct API request can use another admitted value; the shown window follows `±confirmed_ifbw/2`, not FAC occupancy, so it does not imply automatic reception across 4.5–20 kHz. The DRM+ UI uses a fixed 100 kHz channel; this is not a claim that its outermost OFDM carriers lie exactly at ±50 kHz.

The final fine-channel FIR uses the requested RF transition width, independently of the selected output sample rate. A decoder rate floor or integer decimation step does not widen this filter. WFM retains the processing cadence needed by stereo/RDS, but a narrow RF filter can remove those components. RF bandwidth is distinct from the fixed 15 kHz WFM programme-audio low-pass.

A main-channel retune outside the capture range validates every enabled channel before moving the shared capture center. Aux channels cannot move that center independently. A rejected configuration retains all active channels. A change inside the current plan updates only the affected fine receiver where possible.

### Workers, admission and backpressure

The raw acquisition queue holds 64 packets. PFB uses four fixed workers with at most six tasks in flight and collects results in input order. The fine/framer handoff is bounded at two. Two fixed receiver workers own mutable channel DSP state; a separate FT8 worker runs searches without blocking real-time monitoring. DFN templates are prepared before capture, then mutable processing state is owned by each receiver.

The baseband producer and worker accept at most 65536 complex samples per block. IQ values must be finite, `input_index` must not be `u64::MAX`, and the source rate must be in 1..1e9 Sps. Receiver workers release each queued item's pending-work count exactly once on stop or failure. Disabling the primary channel preserves active auxiliary tails during recapture and stop. Framer finish processes only active channels.

The backend supports three independently configured slots. The current product UI exposes the main slot and disables auxiliary slots on entering SDR. Coarse bins and internal benchmark channel counts do not advertise additional product channels. Mixing, recording and dynamic channel counts are not delivered features.

Admission checks channel count, aggregate output rate and fine/framer cost before changing capture, identity or registry. Fine/framer calibration runs before capture and is reused in the process. Its cost policy reserves a factor of two: `2 × sum(cost_per_coarse_sample × coarse_rate) <= 1e9 ns/s`. This is a conservative internal budget, not proof of SDK, PFB, receiver, network or recording capacity.

Display delivery can replace stale frames for slow viewers. Raw IQ and spectrum processing use bounded queue backpressure until explicit failure. Ordered IQ, PCM and decoder outputs fail on queue overflow without dropping samples. Audio capacity feedback counts queued and unacknowledged PCM. Decoder batches have bounded size and are acknowledged after FIFO application. See [API](API.md) for wire and feedback contracts.

Shared receiver worker failures stop audio and decoder processing for all receivers. The service retains the diagnostic and worker counters, stops receiver workers and closes PCM/event subscriptions. Capture, coarse PFB, fine DDC, raw IQ, spectra and waterfall continue. `sdr.receiver_error` reports the failure; a dedicated `RECEIVER_ERROR` event carries `{error, failed:true, scope:'all_receivers'}`. Receiver failure does not set global `last_error` or `ddc_error`, or show a spectrum error overlay. Capture and device failures still use the global failure path.

Tuning and zoom remain available. Explicit `mode` or `demod` selection requests recovery, including selection of the current mode. Recovery requires stopped workers to exit and failed subscriptions to close. Earlier requests return `receiver_retiring` and require a later retry; no automatic restart is queued. Replacement thread count remains bounded. Recovery assigns fresh stream identities and start boundaries without joining IQ across gaps. Healthy reconfiguration publishes old tails in order; failed receivers cannot guarantee complete tails. Audio and decode controls show the error inline. Hardware link state remains independent.

Before SDC supplies a multiplex configuration, DRM retains at most 13 complete equalized frames for DRM30 or 16 for DRM+. These limits cover long-interleaver warm-up and superframe alignment. Retained frames keep their FAC markers and source provenance. Selected-mode timing trims unsynchronised IQ history on the absolute sample grid. These history limits do not drop input frames or change stream identity. Configured decoding is unchanged; loss recovery clears decoder state.

DRM rejects malformed short SDC entities even when their CRC is valid. Loss recovery reopens the robustness-mode search and clears old station and decoder state. An MSC gap clears interleaver and audio carry. xHE CRC failures are counted and are not delivered. FAC short interleaving is honored. Full signalling reconfiguration and next-multiplex timing remain unsupported.

### Stream identity and clocks

IQBF/IQAX v2 identify a stream by `(epoch, channel_id)`. Retune changes channel identity; a new DSP/capture plan changes epoch. Old output finishes before the new empty `seq=0` boundary. Rejection does not create a new identity. Registry and channel clocks are published from one immutable pipeline snapshot; leaving SDR clears them.

IQ `input_index` is the causal position of the first output in capture complex samples, not the packet start. Group delay is separate, in output complex samples. Alignment is `input_index / input_rate - group_delay_samples / output_rate`. Framing keeps the first nonempty segment's clock. Consumers preserve u64 indices exactly.

AUD2 carries interleaved f32 PCM with actual rate/channels, stream identity, PCM `audio_index`, capture input position and fractional time. SDE2 carries decoder events with the same identity and clock semantics. Unknown total delay and causal presentation are explicit flags: neither permits claiming exact source alignment. The decode pane exposes these clock details in the mode-title tooltip rather than a routine header label. For DRM/DRM+, this timing is independent of station lock and audio playback; decoder errors remain visible in the header. FT8 results retain the original search-window time. New subscribers get their own start boundary; late SSTV/radiofax subscribers get a bounded current-image snapshot without resetting the receiver. Full layouts and ordering are in [API](API.md).

### DSP and playback boundaries

The current mode registry covers AM, DSB, USB, LSB, CW, NFM, WFM, PM, FT8, SSTV, radiofax, DRM and DRM+. Native receivers reuse the shared Rust DSP directly. Analog listening can use AGC, squelch, gain and noise reduction; digital decoding consumes its original protocol input. Listener enhancements must not alter FT8/SSTV/radiofax/DRM input.

`Receiver` validates audio rate (8000..192000 Hz), IF and mode-band geometry, and CW decoder pitch (100..1600 Hz) before DSP construction. It also checks finite IQ, source clocks and finite output PCM. Production receiver and offline entry points validate input; direct raw `Pipeline` and `AnalogDemod` trait callers must still supply finite input.

A runtime DFN error propagates through `Receiver` to the scoped receiver failure path; it does not silently switch algorithms mid-stream. Initialization failure or unsupported DFN operation uses an explicit Wiener selection. DRM/DRM+ retain NR preferences with effective NR Off. An NR toggle keeps decoder history and continuous `audio_index`; source provenance advances over any discarded reducer tail.

SSTV emits picture boundaries in order. Pinning a mode again starts a fresh canvas, and the long-silence timer is bounded.

Radiofax uses a 12 kHz IQ path, a 300–2700 Hz complex band-pass and linear f32 PCM without pre-decoder normalization or AGC. Signal level is measured from that raw band-passed PCM; scale-independent carrier evidence gates reception separately. `FaxPaperDecoder` owns APT, phasing and IOC/LPM detection. The product adapter stores Gray8 pages, applies paper correction once and emits `radiofax-image` and `radiofax-state`. A frequency, mode, disconnect or IQ discontinuity closes the active page as partial and starts a new stream identity. Signal loss and acquisition rejection remain decoder state; they do not latch the shared receiver or alter spectrum publication. The vendored decoder is rasterwave.rs 0.5.0 (`65a334c8`), MIT, under `dsp/core/src/digital/radiofax/vendor/`.

FM keeps its current line clock during a fade for at most 4.5 s. Recovery needs 0.3 s of consecutive signal evidence at the existing gate. The raster advances on input samples without learning from erased samples. Affected rows remain at their original positions and use Gray8 128 after paper correction; `meta.missingRows` identifies them. The state is degraded during this hold. A longer loss closes the logical page as partial. AM black can suppress the carrier: AM retains its separate 2.5 s carrier hold and decodes those black intervals.

The browser groups segments by stream identity and transmission ID. It retains images when the receiver resets, changes frequency or mode, or disconnects. A logical page enters history only after it has rows. The channel budget is 20 MiB of allocated Gray8 storage. The current page and the page being viewed are protected; other pages are evicted oldest first. Eviction and blocked retention are visible. Deleting the active page suppresses later rows from that transmission. Explicit clear and Preset can remove images. History lasts only for this browser session. A late subscriber can restore only the backend's bounded current-page snapshot. PNG names use UTC; menu dates use the persisted local/UTC choice and describe the browser's first image receipt, not the station's transmit time.

The Radiofax window gives the image most of the space. A compact toolbar holds received-page selection, New page, Save, zoom and Follow. The RX line describes the live receiver; the footer describes the selected page, so a complete historical page remains distinct from a current signal loss. Settings hold acquisition and correction controls. Details hold clocks, signal measurements, browser receipt time and memory use. Multi-segment pages expose segment navigation and whole-page/current-segment save choices. Retention failures and missing-prefix warnings stay visible when Details is closed; deletion and clear remain confirmed actions under More.

Follow is a display toggle. Turning it off pins the current page and segment without stopping decoding or moving the scroll position. Turning it on selects the newest page and segment and scrolls to its latest rows. Scrolling up also suspends following; an explicit off remains off when already at the bottom.

The DSP retains at most 20 MiB of raw Gray8 rows and the corrected rows for the same segments (at most 20 MiB more). The service snapshot has its own 20 MiB Gray8 budget. It removes whole old segments, preserves their original indices, and reports `snapshot_prefix_missing` when a late subscriber cannot restore the page prefix. It never crops rows and renumbers a segment. Old retained segments remain eligible for clock and manual correction.

WFM retains stereo and RDS processing. FM analysis is in Hz; fixed NFM ±5 kHz and WFM ±75 kHz normalization precedes adaptive AGC. Disabling AGC does not remove that unit conversion. Stereo indication reports pilot presence, not differing L/R programme content.

AM keeps envelope and carrier-recovery paths in the shared DSP; `am_sync` selects their output with a soft crossfade. Live detector switching preserves capture, filter/resampler state and stream clocks. Pre-AGC carrier/depth analysis and final-PCM level/scope events use a 20 Hz source-sample grid; the browser displays these results rather than deriving metrics from playback callbacks. Enabling sync and achieving carrier lock are separate states.

The browser receives AUD2 and SDE2 rather than running a duplicate production IQ demodulator. Each playback chain has one producer. Mode changes flush old PCM and wait for the new start boundary. Actual codec rate/channel changes also start a new audio boundary. The worklet's ready handshake determines playback rate; resampling accommodates the device and sound-card clocks. Overflow fails the playback chain explicitly. The shared crate also supports committed browser WASM artifacts for offline diagnostics; they do not replace the native service build.

AudioContext setup and resume errors affect audio playback only. The browser ignores stale worker callbacks. Local decoder or setup failures remain latched until explicit retry or a healthy fresh identity. A receiver failure clears only with an explicit healthy STATUS and fresh active identities.

## Frontend state and rendering

User-set, backend-confirmed parameters use `core/params.ts` slots: user actions set desired values and STATUS confirms them. Client-only preferences are authoritative locally. Measurement results use `core/results.ts` with explicit setters, without desired/confirmed semantics. Shared types and registries remain leaf modules; upper layers may depend on leaves. Registration modules must be reachable from `main.ts`.

Decode refreshes the selected WFM receiver on pane, channel and stream changes. Control or device disconnect cancels active spectrum gestures and releases pointer capture before resetting receiver state; a later pointer release cannot submit a tune.

### SDR filter and tuning controls

The panel, numeric field and spectrum overlay share `sdrDemod` and `sdrIfbw`; those parameter slots remain the only desired/confirmed values. A custom value remains visible without forcing a preset highlight. A STATUS confirmation also records the last confirmed value for that mode. Selecting a mode restores its stored confirmed value, or the mode default when none is valid; ordinary tuning retains the active filter, and Preset clears these mode preferences with the existing SDR preferences.

| Modes | Default (Hz) | Presets (Hz) | Editable |
|---|---:|---|---|
| AM/DSB | 6000 | 3000, 4500, 6000, 9000, 12000 | yes |
| USB/LSB | 2400 | 1800, 2400, 2700, 3000, 6000 | yes |
| CW | 500 | 100, 200, 500, 1000, 2000 | yes |
| NFM | 12000 | 9000, 12000, 12500, 15000, 25000 | yes |
| WFM | 180000 | 100000, 150000, 180000, 200000, 250000 | yes |
| PM | 12000 | 6000, 9000, 12000, 15000, 25000 | yes |
| FT8/SSTV/radiofax | 3000 | UI fixed | no |
| DRM | 12000 | UI selected/locked | no |
| DRM+ | 100000 | UI fixed | no |

Preset buttons use compact k/M suffixes and expose the full Hz value on hover. The numeric input offers the full Hz/kHz/MHz choice through the shared units control; unit selection changes only the representation, not the underlying Hz value. Typed input and the virtual keypad use the same validation and commit path. Custom values are not capped by common presets: AM 100 kHz, SSB 50 kHz, CW 10 kHz and other candidates remain allowed when admitted. The numeric editor and backend enforce a finite minimum of 50 Hz, or 101 Hz for USB/LSB to prevent guard collapse. STATUS publishes `sdr.filter_limits:{mode,min_hz,max_hz}` for the current mode: `min_hz` is the real DSP floor (protocol-locked or unknown modes report 0 with a null max) and `max_hz` is the integer-sampling ceiling from the current capture center/bandwidth, input rate, PFB taps, listen frequency and CW pitch, null outside SDR, with channel 1 disabled, or with no feasible range. The UI checks these dynamic bounds, which are neither fixed hardware limits nor recommended preset ranges; remaining sample-rate, channel and cost admission checks can still reject a value inside them.

The Filter row offers numeric entry, a bandwidth slider, presets and View. The visible readout shows the actual passband and request state. CW adds numeric and slider tone controls (100–1600 Hz); changing tone shifts the receive passband relative to the unchanged dial.

Preset clicks, Enter and field blur commit once; Escape cancels the draft. Filter edges and sliders preview locally and submit once on release; pointer cancellation, window blur and mode changes discard drafts. Bandwidth and CW tone edits share one request in flight and one newest queued candidate. A matching request-ID acknowledgement completes it and sends the queued candidate; ERROR or three seconds without acknowledgement restores confirmed values. Frontend limits never replace backend admission.
Tuning steps are tiered per demodulation mode: the step box and plain arrows use the `normal` tier, Shift+arrows the `coarse` tier and Ctrl+arrows the `fine` tier, with an inline settings panel for the two held-key tiers whose typed value commits on Enter, change or blur while Escape or a mode switch cancels the draft. Defaults follow each mode's channel plan — AM/DSB 1/5/0.1 kHz, USB/LSB 100/1000/10 Hz, CW and FT8 10/100/1 Hz, NFM/PM 12.5/100/1 kHz, WFM 100/1000/10 kHz, SSTV/DRM 1/10/0.1 kHz and DRM+ 10/100/1 kHz — and an unlisted mode reads a generic conservative set. Remembered values (at least 1 Hz) are stored per mode with no coupling to the capture width: changing the bandwidth leaves the ladder alone, and Preset clears the three tier maps with the other SDR preferences, removing the retired per-bandwidth step key as well.

Listen-frequency edits use the same one-in-flight discipline: a single `SET_SDR_CHANNEL` tune request is outstanding while the newest target waits queued, an acknowledgement matching its request ID completes it and sends the queued target, ERROR or three seconds without a reply restores the confirmed frequency with a timeout notice, and replies from an older request or an older configuration snapshot are ignored. Window blur or a hidden tab drops only the queued repeat and lets the outstanding request settle; demodulator changes, capture changes, band presets, Preset and disconnect cancel it outright. The tune keys keep native behaviour where it belongs: focused inputs, selects, textareas, contenteditable regions, dialogs, the keypad and the zoom overview keep their own keys, a focused button keeps everything except the tune arrows, Shift and Ctrl modify arrows only, Alt, Meta and Ctrl+Shift stay with the browser, IME composition is skipped, and key repeat stops on blur or once the tab is hidden.

Each pointer press has one owner until release or cancellation. Axes own axis gestures. The listening-frequency badge above the plot owns an explicit tuning drag, including with Zoom on. In the SDR plot, Ctrl+click or Ctrl+drag places or moves a marker; an existing marker diamond can be dragged directly. With Zoom on, a normal plot drag selects a zoom rectangle, including when it starts on the passband or tuning dial. With Zoom off, a valid Filter edge starts a filter drag; other plot drags tune with Free semantics. Marker and zoom gestures never tune. A mode, capture, view or Zoom change, pointercancel, lost pointer capture, Escape, hidden tab or window blur cancels the badge gesture without a release action. Pointer capture allows a badge drag to finish outside the canvas.

An ordinary SDR plot click uses Locate: Free selects the clicked frequency; Channel selects `anchor + round((f − anchor)/spacing) × spacing`; Signal selects a credible carrier. Selecting Channel also reveals its spacing (Hz, at least 1 Hz, fractional values allowed) and anchor (MHz, any finite value) inline. There is no separate Grid button. Steps opens the separate tuning-step settings.

Signal is available in AM and CW. Other modes disable it with a tooltip and retain its stored preference. Carrier evidence comes from raw native spectrum frames, before Average or Max Hold. Acquisition needs at least three consecutive credible frames within the last 350 ms, spanning at least 120 ms. A stream gap over 350 ms or a reversed timestamp resets history. The peak must be at least 8 dB above the higher mean of the two side regions, each 3–8 bins away. Both shoulders must descend outwards and reach at least 4 dB below the peak at a distance of 2–4 bins. Bins wider than 1 kHz in AM or min(250 Hz, IF bandwidth/2) in CW cannot supply a target.

The acquisition radius is 40 screen pixels; the retention radius is 52 pixels. Both are capped at 10 kHz in AM or 3 kHz in CW. The selected target stays at its exact native bin centre, without interpolation, while supporting peaks can move by one bin. A dropout can retain the preview for at most 300 ms after the last local evidence. A click requires current-frame support for that same carrier, evidence less than 300 ms old, and an unchanged source, configuration and view. A cached target within an age limit is not sufficient. CW tuning subtracts max(100 Hz, confirmed sidetone pitch) from the carrier. Capture-axis, demodulator or pitch changes clear carrier evidence.

Only a valid Channel or Signal target has a preview line and one frequency label at the bottom of the plot. Free has no moving preview line. There is no permanent Signal hint row. The click target is frozen at pointerdown and validated before submission. Changed view, mode, pitch, IF bandwidth, channel settings or listen frequency invalidates that target. An invalid Signal target falls back to the clicked frequency with a short notice.

The listening-frequency badge above the plot shares measured geometry for drawing and hit testing. Dragging preserves the pointer offset, previews within the view and RF limits, and submits on release. Badge, plot-tuning and filter-edge drags show the matching waterfall passband and dial line; release or cancellation restores clean history pixels, including frozen history.

Passband fill is green at 24% opacity in both themes. Thin solid edges appear only during dragging, at the actual filter cutoffs.

The SDR level badge reserves 10ch and centers its text; unavailable IQ level remains `--- dBFS`. The squelch badge reserves 9ch, keeps its dBm unit when enabled and shows localized Off at −160 dBm or below. AGC sits to the left of Mute on the volume row. Mute On means playback is disabled; Mute Off means playback is enabled. The Chinese labels are 静音 开 and 静音 关. These controls change listening only, not protocol decoding.

Filter gestures change only the filter. View Passband saves the display window and Zoom state, and highlights the toggle. A second press restores both. A manual view change, capture change, mode change or reset discards the saved state. The view uses the original spectrum bins; zoom adds no spectrum resolution.

The backend sends native frequency grids. The frontend preserves Hz for markers and export, uses peak-preserving resampling for display, and treats unsupported edge bins as gaps. Bin spacing is not RBW. Local zoom changes the visible window without retuning or changing receiver clocks. Density and waterfall histories are bounded. Read parameter slots once outside hot loops.

The spectrum CSS box owns canvas geometry; the backing store follows it through ResizeObserver. With the waterfall enabled, both rendering and STATUS-driven measurement UI refreshes keep the marker table hidden. A STATUS report must not temporarily restore that table: doing so shrinks the spectrum and clears its resized bitmap before the next render hides the table again.

SWP and normal RTA spectrum updates target 30 fps using one trailing timer anchored to the last frame draw. SWP still processes every accepted sweep for traces, waterfall and trigger detection; RTA processes only the latest pending normal packet. First frames, changed axes and armed RTA captures remain immediate. Mode changes, retunes, trigger resets and disconnects cancel pending work. UI redraw requests remain synchronous, and SDR retains its existing processing and redraw cadence.

Axis gestures preview locally and commit when settled. Manual Ref owns a local viewport (-200..+100 dBm plus Offset) separately from its clamped hardware request (-160..+40 dBm); raw acknowledgements preserve the chosen display. Gain readbacks come from the active mode's successful SDK configuration and remain null when unavailable; preamp `0` means Auto and `1` means Off, not physical On. RTA capture uses device trigger metadata carried with each RTA2 frame; SDR retains RTAF, whose legacy decoding is compatibility only. A frame arrival or elapsed delay cannot prove capture completion. SWP level crossing is evaluated between consecutive sweeps in the frontend. Parameter, Auto, trace, measurement and trigger contracts are in [MODE_STATE_FLOW](MODE_STATE_FLOW.md).

SDR frequency-axis pans keep one `SET_SDR` request in flight and only the latest unsent window. Only the matching request ID confirms that pan; periodic STATUS and older acknowledgements cannot rebase a newer target or a held pointer's origin. Rejection discards the unsent window, and reset cancels the pending gesture state. RF-range errors include the channel number, rejected frequency and allowed range.

## Quality limits and verification

Three backend slots do not imply three exposed UI receivers or unlimited listening/recording. FT8 supports standard types 1/2 within its supported search window; SSTV does not decode the post-image FSK callsign. RDS text quality and short/looped DRM codec playback remain constrained. [KNOWN_ISSUES](KNOWN_ISSUES.md) records current limitations and SDK boundaries.

The browser's 30 fps SWP/RTA scheduling target depends on incoming frames and rendering cost; it does not establish measured hardware throughput or change backend acquisition timing. These descriptions summarize implementation; documentation maintenance does not revalidate hardware.

Reproducible entry points are `make wasm-test` for shared DSP, `make ci` for local non-hardware gates, and `make e2e-fake` for the native fixture UI. `tools/bench/sdr_pipeline_check.py` and `sdr_pipeline_controls.py` exercise live IQv2 continuity and controls; native `websad --sdr-pipeline-bench` exercises the pipeline. Command options and prerequisites are in [tools](../../tools/README.md). Hardware acceptance must check content, hardware timestamps, queue/failure deltas and drained sample conservation; fake-source throughput or a short window cannot prove sustained RF operation. Keep browser checks separate from quiet hardware performance windows; see [DEVELOPMENT](DEVELOPMENT.md).

## References and licences

Project code is MIT. Vendor SDK binaries are proprietary and obtained separately. DSP techniques are informed by liquid-dsp, tpt-dsp, sdr-web, Radioband and pffft.wasm; those references are not claims that their implementations were copied. Offline numeric parity uses this repository's reference kernels and committed fixtures.

FT8 protocol tables originate from [ft8_lib](https://github.com/kgoba/ft8_lib) (MIT), with provenance in `tools/fixtures/port_ft8_tables.py`. SSTV includes the MIT-licensed vendored decoder under `dsp/core/src/digital/sstv/vendor/`. CW uses [ggmorse](https://github.com/ggerganov/ggmorse) under `tools/vendor/ggmorse/` and its licence. DeepFilterNet integration retains the licences under `dsp/deepfilter/`. Codec build/provenance and licence boundaries remain in `dsp/core/fdk/README.md`, `dsp/core/xaac/README.md`, [DRM_BENCH](DRM_BENCH.md) and [DRM_AAC_RESERIALIZATION](DRM_AAC_RESERIALIZATION.md). Preserve source attribution when changing fixtures or vendored code.
