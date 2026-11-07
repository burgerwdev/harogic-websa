# Architecture

[Documentation](INDEX.md) · [State ownership](reference/STATE.md)

## System boundaries

The Rust service owns the device, acquisition, channelization, receiver processing and durable
recording. The C++ shim serializes the proprietary SDK. The TypeScript browser owns controls,
visualization, decoder presentation and local audio playback. Python supplies offline numerical
references, fixtures and bench tools. Retained WASM artifacts are optional diagnostics.

```text
Browser commands → admission / mode owner → retained SDK session
SDK IQ → bounded capture queue → PFB → per-channel fine DDC → immutable IQ frames
                               └──── direct wideband DDC ───┘
IQ frames → independent receiver workers → published PCM / decoded results
          → recording workers             → bounded HTTP / WebSocket consumers
SDK traces → spectrum / density / waterfall renderer
Published PCM → browser buffer / resampler → local gain / routing → audio output
```

SWP, RTA, SDR, harmonic and phase-noise modes share one physical device. Server mode releases
that owner to SoapySDRServer. Entering server mode and starting its process are separate actions.
Requests are validated before active state is replaced. Actual hardware values are distinct from
requested settings; [state ownership](reference/STATE.md) defines their transitions.

## Acquisition and signal processing

### Stage map

The path guard checks these concrete implementation paths.

| Stage | Modules |
|---|---|
| SDK session and capture | `backend/sdk/san90_sdk.cpp`, `backend/src/bin/websad/service_capture.rs` |
| Routes, command commit and lifecycle | `backend/src/bin/websad/service_routes.rs`, `backend/src/bin/websad/service_commands.rs`, `backend/src/bin/websad/service_lifecycle.rs` |
| Device state, RTA commits and admission | `backend/src/bin/websad.rs`, `backend/src/bin/websad/rta_control.rs`, `backend/src/bin/websad/service_preflight.rs` |
| Pipeline orchestration | `backend/src/bin/websad/sdr_pipeline.rs`, `backend/src/bin/websad/service_ddc.rs` |
| Coarse PFB | `backend/src/pfb.rs`, `backend/src/pfb_pool.rs` |
| Fine DDC and rates | `backend/src/fine.rs`, `backend/src/narrow.rs`, `backend/src/rate_plan.rs` |
| Receiver workers | `backend/src/bin/websad/receiver_workers.rs`, `backend/src/receiver.rs` |
| Analog/digital kernels | `dsp/core/src/analog/`, `dsp/core/src/digital/`, `dsp/core/src/pipeline.rs` |
| Noise reduction | `backend/src/nr.rs`, `backend/src/nr_neural.rs`, `dsp/deepfilter/src/` |
| Wire/subscribers | `backend/src/wire.rs`, `backend/src/bin/websad/service_stream.rs`, `backend/src/bin/websad/subscriber_stream.rs` |
| Recording | `backend/src/bin/websad/recording.rs`, `backend/src/bin/websad/recording_audio_files.rs` |
| Browser receiver state | `frontend/src/core/multichannel.ts`, `frontend/src/ui/workbenchSession.ts` |
| Playback | `frontend/src/audio/sdrAudio.ts`, `frontend/src/audio/sdrAudioWorker.ts`, `frontend/src/audio/sdrAudioWorklet.js` |
| Reference kernels | `tools/reference/`, `tools/fixtures/`, `tests/fixtures/` |

The production SAN shim validates finite configured rates and fixed 16,240-sample CS16 packets,
including format, byte count and pointer before copying. Its fake-SDK contract compiles the
actual shim with vendor headers; no physical acquisition is performed. Soapy is a separate
adapter: its transport MTU stays 16,384, SDK packets are at most 1,048,576 samples, and
the host ring plus conversion scratch are bounded by 64 MiB before allocation.
Soapy also rejects device counts beyond the SDK's 64-entry Ethernet array before iteration;
reference gain must be finite and fit `int` before stopping a stream or converting the value.
The Python reference reader independently caps CS16 packets at 4 MiB and checks size/format
before `ctypes.string_at`; its SDK-free tests do not qualify Rust capture.

The production RTA shim validates frame dimensions and finite frequency ranges before allocation.
Its trace, bitmap and finite-capture accumulation arrays share a 64 MiB limit, including retained
capacity and replacement peaks; open, reconfiguration and rollback share the checks. Rust SWP/RTA/PNM
also validate shim-reported sizes. SWP/RTA frames are limited to 65,536 points and reject non-finite
numeric readback. These are specific buffer limits, not bounds on all SDK memory or service RSS.

### PFB, fine DDC and clocks

At 62.5 MSps, the 50 MHz usable capture uses a two-times oversampled PFB with 128 bins and
hop 64: bin spacing is 488.28125 kHz and each coarse stream is 976.5625 kSps. This is not a
128-receiver capacity claim. Product capacity comes from `contracts/service-capacity.json`.
Narrow captures use their actual geometry. Wideband provider input can take the direct IQ route.

Ordered PFB workers share immutable plans, while each channel owns its oscillator and mutable
filter history. Fine DDC performs the remaining translation/filtering/rate conversion. Admission
uses the full passband and transition geometry, not only a center frequency. Actual fractional
sample rates must survive API and file round trips.

PFB uses four workers and retains at most six items across executing jobs, results waiting for
their sequence and ready output. A finished worker can accept another job; each job carries its
full history and absolute input position, and output remains ordered. At the retained limit,
available output makes submission return backpressure so the caller drains before retrying.
Receiving a result alone cannot release capacity that the caller has not yet drained.

IQ allocations are immutable and shared with consumers; encoding occurs only when needed by
wire/file subscribers. Source identities and exact sample positions distinguish capture epochs,
baseband streams and processing generations. Receiver revisions, stream identities and exact-clock u64 values use decimal strings;
diagnostic counters and RTA trigger metadata also contain JSON integers and cannot all be parsed as strings. Gaps create
explicit boundaries; no missing samples or absolute UTC are invented. See [receiver contracts](reference/RECEIVERS.md)
and [binary formats](reference/API.md) for exact layouts and diagnostics.

## Concurrency and failure ownership

Capture, PFB, fine DDC, receiver, recorder and subscriber boundaries have bounded queues.
Shared capture failure affects all dependent receivers. A receiver processing or consumer failure
is isolated where its owner permits; it must not silently discard IQ and continue the old clock.
Display frames can be replaced. Explicit retry creates the required new identity.

Fatal PFB control errors and Rust panics explicitly end the output queue. Control replies deliver collected tails before publishing the terminal cause; polling, terminal diagnostics and stop preserve that cause. After panic, diagnostics use the published snapshot and the original owner is dropped after closing the queue, without processing the affected DDC again. This boundary does not catch native aborts or segmentation faults.

Fine workers preserve ordering within each lane. CPU-intensive inference and external decoder
processes do not execute on acquisition locks. Thread wall-time sums across owners are not
single-thread CPU utilization. Admission limits are structural; available algorithms do not imply
arbitrary combinations fit the host. Unit/synthetic passes do not qualify sustained RF performance.

HTTP and WebSocket command paths share one commit owner for state publication, identities
and stream retirement. A stale preflight ticket cannot advance the transport epoch.
Lifecycle code reserves the listener before starting owners and cooperatively joins the
publisher before teardown. A fatal publisher failure, panic in its acquisition/preparation/GNSS task, or unexpected successful return
closes the HTTP listener and preserves the original error instead of serving stale healthy status.
The loop's unwind boundary retains pending model preparation handles: it signals shutdown before
waiting for that work, then releases SDK and recording owners. Ordinary receiver errors remain isolated to their branches.
SWP/RTA/PNM control requests share one deadline for enqueue and reply;
a transport timeout prevents further requests. All four SDK modes wait at most 250 ms on retirement,
then keep an unfinished owner behind the exclusive session gate until its original thread finishes teardown.
No other thread may close or reopen that session. Vendor calls and process static destructors may still hang,
so the launcher stop deadline remains necessary.

RTA profile and trigger changes share one commit path: applied values change after SDK acceptance.
Link loss immediately retires the owner, clears capture completion and retries after a cooldown.
Phase noise validates the complete offset window before changing the next cycle's request;
rejection preserves both the running and subsequent cycles.

Model preparation refreshes host/cgroup observations when the head of the queue attempts to
start, and takes its RSS baseline at that point. Observed process growth only controls
preparation concurrency; it is not exclusive model memory or a receiver admission guarantee.

The optional impulse blanker repairs at most `window` consecutive high samples, carrying
its budget across blocks. Longer signals pass through and update the per-sample RMS estimate.
Repair uses a known clean endpoint or holds the previous output, without adding samples or delay.
First-block initialization and available lookahead remain block-dependent.

The native CW owner accepts 8–192 kHz mono PCM and sends at most 100 ms per bridge call.
ggmorse grows its input buffers to the actual resampling request before the capture callback;
buffer sizing does not change the analysis window or sample timing. The local patch and pinned
upstream provenance are recorded in `tools/vendor/ggmorse/UPSTREAM`.

The optional diagnostic demodulation ABI checks finite rates before construction, accepts output
rates of 8–192 kHz and retains valid fractional rates. Before processing, it uses the existing
resampler state to bound output to 1,048,576 PCM or adapted complex samples per call. Insufficient
capacity does not consume input; the stereo difference block is likewise retained for a larger
buffer to retry. These diagnostic ABI constraints do not replace native receiver admission.

Recording workers perform encoding, compression, checksums and sync outside acquisition locks.
IQ, results and audio share sixteen task slots. Stop seals admission before draining. Startup
recovers completed data or verified prefixes with the original task identity; it never resumes a
retired receiver. New audio tasks write media files directly; legacy Float32 journals remain
readable. Archive operations share a bounded reader lease. See [Recordings](reference/RECORDINGS.md).

## Browser state and audio

One native receiver adapter and session owns the workbench's server state. Windows own geometry,
selection and editor drafts. They do not acquire independent physical receivers. Request identities,
expected revisions and service generations prevent late replies from replacing newer state.

`workbenchApp.ts` owns window coordination and forms; `workbenchChannelStrip.ts` owns
compact-view drafts, readouts, ticker and listeners. During a deferred view switch, actions
and playback levels stay bound to the displayed service/UID. Disposal removes listeners
and timers and invalidates pending completions.
The session samples existing UIDs when its serial queue dispatches a create command, so consecutive
creates return their own identities. Ambiguous multiple additions fail explicitly. Preset, service
replacement and disposal invalidate old listening previews. Cancelling edits also resets focused
form DOM so cancelled drafts cannot re-enter the cache.

Backend audio processing affects published PCM and recordings. Browser gain, pan, mute and Solo
are local monitoring. RF IQ power, demodulated PCM level and local output level are different
measurements; level displays must state their source. Browser buffering/resampling means receipt
time is not playback time. Decoder history and images have explicit retention budgets.

Rendering stays downstream of state and frame contracts. Device/display parameter ownership is
centralized; new controls should reuse intent/acknowledgement paths rather than write confirmed
state optimistically. The dependency and registration guards help detect structural regressions.

## Maintenance priorities

The largest risks are broad service/frontend coordination modules, duplicated UI projections,
legacy archive compatibility and experimental tools beside production paths. Split by ownership
and stable interfaces when a behavioral change provides a testable seam; avoid cosmetic file
splits that preserve hidden coupling. Keep numerical reference tests, clock/identity tests and
bounded-resource regressions when consolidating test files.

[Development](DEVELOPMENT.md) defines validation and maintenance priorities;
[Operations](OPERATIONS.md) describes current capability boundaries. Third-party sources and
licenses stay with their components; see [dependencies and notices](DEVELOPMENT.md#dependencies-and-notices).
