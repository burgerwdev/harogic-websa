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

One persistent SDK owner thread serves SWP/RTA/IQS/PNM. All SDK calls are serialized. Healthy mode changes stop acquisition and release mode state while retaining the physical handle. `Device_Open` receives the permanent session's stable `void**` handle slot: vendor PNM workers retain that address until physical close. A stale/disconnected handle must not be passed to `Device_Close`.

IQS configuration and arming are separate. The service prepares PFB, fine DDC, receiver workers and metadata before explicit arm, including recovery. It keeps the first packet and checks packet order and hardware timestamps. Host sample indices do not substitute for hardware continuity checks.

A valid IQS packet returned with `-12` is retained with its timestamp; saturation is reported separately as `status_warning=-12`. Outside an explicitly active Auto operation, saturation preserves manual Ref and does not turn a valid packet into a bus failure. Explicit Auto recovery is bounded and uses fresh observations from the same mode/configuration; see [MODE_STATE_FLOW](MODE_STATE_FLOW.md). IQ, FFT, warning and timing return through one `san90_next_v3` call; routine metadata polling does not consume IQ. Malformed packets and real timestamp gaps fail explicitly.

Routine GNSS status uses a cached SDK snapshot. Device and optional clock capabilities come from successful open/hardware-state results; absent data stays unknown. External reference configuration uses 10 MHz. Recovery invalidates the physical session explicitly; shutdown joins mode owners before closing the session and SDK thread.

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

Each receiver has its own frequency, mode, occupied bandwidth, `RatePlan`, oscillator phase, filter history and output rate. Fine DDC mixes and performs multistage anti-alias decimation; an exact protocol rate is adapted at the receiver input when needed. Audio playback rate is not a universal lower bound on IQ rate. USB/LSB envelopes include the sideband edge, CW includes pitch, FT8 retains at least the positive 3.2 kHz band, and SSTV the positive 3 kHz band. DRM and DRM+ use their protocol bandwidths.

A main-channel retune outside the capture range validates every enabled channel before moving the shared capture center. Aux channels cannot move that center independently. A rejected configuration retains all active channels. A change inside the current plan updates only the affected fine receiver where possible.

### Workers, admission and backpressure

The raw acquisition queue holds 64 packets. PFB uses four fixed workers with at most six tasks in flight and collects results in input order. The fine/framer handoff is bounded at two. Two fixed receiver workers own mutable channel DSP state; a separate FT8 worker runs searches without blocking real-time monitoring. DFN templates are prepared before capture, then mutable processing state is owned by each receiver.

The backend supports three independently configured slots. The current product UI exposes the main slot and disables auxiliary slots on entering SDR. Coarse bins and internal benchmark channel counts do not advertise additional product channels. Mixing, recording and dynamic channel counts are not delivered features.

Admission checks channel count, aggregate output rate and fine/framer cost before changing capture, identity or registry. Fine/framer calibration runs before capture and is reused in the process. Its cost policy reserves a factor of two: `2 × sum(cost_per_coarse_sample × coarse_rate) <= 1e9 ns/s`. This is a conservative internal budget, not proof of SDK, PFB, receiver, network or recording capacity.

Display delivery can replace stale frames for slow viewers. Ordered IQ, PCM and decoder outputs use bounded queues and explicit failure on overflow. They must not silently discard samples, grow queues or block acquisition to make a throughput check pass. Audio capacity feedback counts queued and unacknowledged PCM; decoder batches remain bounded and are acknowledged after FIFO application. See [API](API.md) for the wire and feedback contracts.

Receiver worker failures are terminal for the SDR session. The service retains the first diagnostic and worker counters, retires the failed workers and capture, emits one `receiver_failed` error, and closes PCM/event subscriptions with a receiver-failure reason. It never resumes a discontinuous stream or repeatedly polls a sticky worker failure. Healthy reconfiguration still publishes retiring tails in order; a failed session makes no complete-tail guarantee. Leaving SDR and returning explicitly starts fresh workers and identities, while the hardware link state remains independent.

DRM acquisition keeps only recent complete equalized frames while SDC has not supplied a multiplex configuration: 13 frames for DRM30 and 16 for DRM+, covering long-interleaver warm-up and superframe alignment. Trimming preserves each retained frame's FAC marker and source provenance; configured decoding is unchanged. Selected-mode timing also trims unsynchronised IQ history using the existing absolute sample grid. These are decoder acquisition histories, not dropped input frames or a new stream identity; loss recovery still clears the existing decoder state.

### Stream identity and clocks

IQBF/IQAX v2 identify a stream by `(epoch, channel_id)`. Retune changes channel identity; a new DSP/capture plan changes epoch. Old output finishes before the new empty `seq=0` boundary. Rejection does not create a new identity. Registry and channel clocks are published from one immutable pipeline snapshot; leaving SDR clears them.

IQ `input_index` is the causal position of the first output in capture complex samples, not the packet start. Group delay is separate, in output complex samples. Alignment is `input_index / input_rate - group_delay_samples / output_rate`. Framing keeps the first nonempty segment's clock. Consumers preserve u64 indices exactly.

AUD2 carries interleaved f32 PCM with actual rate/channels, stream identity, PCM `audio_index`, capture input position and fractional time. SDE2 carries decoder events with the same identity and clock semantics. Unknown total delay and causal presentation are explicit flags: neither permits claiming exact source alignment. The decode pane exposes these clock details in the mode-title tooltip rather than a routine header label. For DRM/DRM+, this timing is independent of station lock and audio playback; decoder errors remain visible in the header. FT8 results retain the original search-window time. New subscribers get their own start boundary; late SSTV subscribers get a bounded current-image snapshot without resetting the receiver. Full layouts and ordering are in [API](API.md).

### DSP and playback boundaries

The current mode registry covers AM, DSB, USB, LSB, CW, NFM, WFM, PM, FT8, SSTV, DRM and DRM+. Native receivers reuse the shared Rust DSP directly. Analog listening can use AGC, squelch, gain and noise reduction; digital decoding consumes its original protocol input. Listener enhancements must not alter FT8/SSTV/DRM input.

WFM retains stereo and RDS processing. FM analysis is in Hz; fixed NFM ±5 kHz and WFM ±75 kHz normalization precedes adaptive AGC. Disabling AGC does not remove that unit conversion. Stereo indication reports pilot presence, not differing L/R programme content.

AM keeps envelope and carrier-recovery paths in the shared DSP; `am_sync` selects their output with a soft crossfade. Live detector switching preserves capture, filter/resampler state and stream clocks. Pre-AGC carrier/depth analysis and final-PCM level/scope events use a 20 Hz source-sample grid; the browser displays these results rather than deriving metrics from playback callbacks. Enabling sync and achieving carrier lock are separate states.

The browser receives AUD2 and SDE2 rather than running a duplicate production IQ demodulator. Each playback chain has one producer. Mode changes flush old PCM and wait for the new start boundary. Actual codec rate/channel changes also start a new audio boundary. The worklet's ready handshake determines playback rate; resampling accommodates the device and sound-card clocks. Overflow fails the playback chain explicitly. The shared crate also supports committed browser WASM artifacts for offline diagnostics; they do not replace the native service build.

## Frontend state and rendering

User-set, backend-confirmed parameters use `core/params.ts` slots: user actions set desired values and STATUS confirms them. Client-only preferences are authoritative locally. Measurement results use `core/results.ts` with explicit setters, without desired/confirmed semantics. Shared types and registries remain leaf modules; upper layers may depend on leaves. Registration modules must be reachable from `main.ts`.

The backend sends native frequency grids. The frontend preserves Hz for markers and export, uses peak-preserving resampling for display, and treats unsupported edge bins as gaps. Bin spacing is not RBW. Local zoom changes the visible window without retuning or changing receiver clocks. Density and waterfall histories are bounded. Read parameter slots once outside hot loops.

The spectrum CSS box owns canvas geometry; the backing store follows it through ResizeObserver. With the waterfall enabled, both rendering and STATUS-driven measurement UI refreshes keep the marker table hidden. A STATUS report must not temporarily restore that table: doing so shrinks the spectrum and clears its resized bitmap before the next render hides the table again.

SWP and normal RTA spectrum updates target 30 fps using one trailing timer anchored to the last frame draw. SWP still processes every accepted sweep for traces, waterfall and trigger detection; RTA processes only the latest pending normal packet. First frames, changed axes and armed RTA captures remain immediate. Mode changes, retunes, trigger resets and disconnects cancel pending work. UI redraw requests remain synchronous, and SDR retains its existing processing and redraw cadence.

Axis gestures preview locally and commit when settled. Manual Ref is a device control; display scale/offset has separate ownership. Gain readbacks come from the active mode's successful SDK configuration and remain null when unavailable; preamp `0` means Auto and `1` means Off, not physical On. RTA capture uses device trigger metadata carried with each RTA2 frame; SDR retains RTAF, whose legacy decoding is compatibility only. A frame arrival or elapsed delay cannot prove capture completion. SWP level crossing is evaluated between consecutive sweeps in the frontend. Parameter, Auto, trace, measurement and trigger contracts are in [MODE_STATE_FLOW](MODE_STATE_FLOW.md).

## Quality limits and verification

Three backend slots do not imply three exposed UI receivers or unlimited listening/recording. FT8 supports standard types 1/2 within its supported search window; SSTV does not decode the post-image FSK callsign. RDS text quality and short/looped DRM codec playback remain constrained. [KNOWN_ISSUES](KNOWN_ISSUES.md) records current limitations and SDK boundaries.

The browser's 30 fps SWP/RTA scheduling target depends on incoming frames and rendering cost; it does not establish measured hardware throughput or change backend acquisition timing. These descriptions summarize implementation; documentation maintenance does not revalidate hardware.

Reproducible entry points are `make wasm-test` for shared DSP, `make ci` for local non-hardware gates, and `make e2e-fake` for the native fixture UI. `tools/bench/sdr_pipeline_check.py` and `sdr_pipeline_controls.py` exercise live IQv2 continuity and controls; native `websad --sdr-pipeline-bench` exercises the pipeline. Command options and prerequisites are in [tools](../../tools/README.md). Hardware acceptance must check content, hardware timestamps, queue/failure deltas and drained sample conservation; fake-source throughput or a short window cannot prove sustained RF operation. Keep browser checks separate from quiet hardware performance windows; see [DEVELOPMENT](DEVELOPMENT.md).

## References and licences

Project code is MIT. Vendor SDK binaries are proprietary and obtained separately. DSP techniques are informed by liquid-dsp, tpt-dsp, sdr-web, Radioband and pffft.wasm; those references are not claims that their implementations were copied. Offline numeric parity uses this repository's reference kernels and committed fixtures.

FT8 protocol tables originate from [ft8_lib](https://github.com/kgoba/ft8_lib) (MIT), with provenance in `tools/fixtures/port_ft8_tables.py`. SSTV includes the MIT-licensed vendored decoder under `dsp/core/src/digital/sstv/vendor/`. CW uses [ggmorse](https://github.com/ggerganov/ggmorse) under `tools/vendor/ggmorse/` and its licence. DeepFilterNet integration retains the licences under `dsp/deepfilter/`. Codec build/provenance and licence boundaries remain in `dsp/core/fdk/README.md`, `dsp/core/xaac/README.md`, [DRM_BENCH](DRM_BENCH.md) and [DRM_AAC_RESERIALIZATION](DRM_AAC_RESERIALIZATION.md). Preserve source attribution when changing fixtures or vendored code.
