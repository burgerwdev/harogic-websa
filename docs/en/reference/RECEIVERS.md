# Receiver reference

[Documentation](../INDEX.md) · [Protocol](API.md) · [Recordings](RECORDINGS.md)

This describes the production receiver API. Fixture-only scenes and the experimental v1 archive are identified separately; they are not production capabilities.

Receiver snapshots include `created_at_ms`, a decimal Unix-millisecond timestamp assigned
when the service creates the logical UID. Renaming, tuning, stopping and recovery preserve
it; a newly created UID receives its own timestamp. Older servers may omit it; clients display
an unknown time instead of inferring creation from connection time or slot number.

## Capture and tuning

The capture snapshot reports `state:running|idle|failed` separately from device connectivity. `parameters_state:live|last_known|unknown` identifies current source geometry, retained previous geometry, or an unopened source. Bandwidth and input rate are null when unknown; in that case `center_hz` is only the requested center and epoch `"0"` is a sentinel, not a live stream. Independent fixture lanes use `parameters_state:simulated`, a nominal shared display window/rate and sentinel epoch `"0"`; their per-receiver clocks describe the actual baseband inputs. For a hardware source, input rate is the PFB input rate and epoch is the DSP source epoch (not the hardware configuration generation or downstream DDC rate). Failure/stop retains source geometry; a reopened source gets a new DSP epoch. Enabled hardware receiver changes require a live, healthy capture. Older servers omit `parameters_state` and may report legacy defaults after failure.

Entering Center moves shared capture and Current together. Main tuning, mode and shared PCM controls always target the reserved temporary source (internal slot 1); saved channels are edited independently in the workbench. Current plus up to fifteen saved definitions share the sixteen-slot capacity. Out-of-band and manually stopped saved channels retain their definitions and quota. Current cannot be retired, and create allocates only slots 2–16. Both views pin Current first unless a saved frequency represents it, in which case all matching saved channels are highlighted independently of editing selection.
Receiver snapshots expose `coverage.state:in_band|out_of_band`. Changes to actual capture center or bandwidth pause uncovered saved channels while preserving `confirmed.enabled:true`, UID, frequency, mode and controls. Full passband and transition coverage restores execution in the same definition; manually stopped channels remain disabled. Display-only geometry never enters this decision. Planned coverage gaps finish the current recording segment and retain a waiting task; new coverage resumes a fresh segment with `coverage_interruptions` timestamps. Stopped tasks remain stopped. Unexpected source/processor faults keep their existing failure behavior.


The Rust production IQS path uses the versioned shim's validated source rate, `62.5 MHz / capture_decimate`. Usable RF bandwidth independently bounds the passband and cannot determine the sample clock through a fixed ratio. Live capture and `last_known` parameters use the same source rate; Python reference and Soapy sample-rate validation belong to their respective paths.

## Receiver presets

`capabilities.receiver_presets:true` enables `GET /api/sdr/receiver-presets`, returning `{version:1, revision:"0", presets:[]}`. Entries contain `{name, settings:{purpose, baseband, processors, continuity?}}`. Limits are 32 entries, 256 KiB per file and 1–128 bytes per nonblank name. Storage is `WEBSA_RECORDING_DIR/receiver-presets.json` under the instance's existing exclusive directory lock. A read or storage-format validation failure preserves the file and disables the capability instead of resetting it. Storage validates decimal u64 revisions, duplicate names and nested baseband/processor/continuity fields. Omitted optional controls from older files and unknown mode names remain round-trippable; actual device and provider availability are checked when loading.

POST to the same path to save: `{type:"save", service_id, expected_revision, name, receiver_uid, receiver_revision}`; delete: `{type:"delete", service_id, expected_revision, name}`. Revisions are decimal u64 strings. Saving extracts only the specified receiver's confirmed revision, resolves omitted AGC/gain fields to the source's actual settings, accepts no arbitrary candidate settings, and replaces the same name. List/receiver revision conflicts or an old service return 409; invalid input 400, missing item 404, unavailable storage 503, write failure 500. Success returns the new list after syncing and atomically replacing the file. Disk I/O holds no acquisition lock. Clients explicitly refresh after conflict and do not automatically retry writes.

Loading merges saved settings into a target channel and uses existing `configure` admission and explicit confirmation. Target name, color, tags and reception enabled state remain; UID, source clocks, tasks and local monitoring are excluded. A receiver revision/service change after preview rejects confirmation and requires another preview. Saved out-of-band settings are retained while waiting for coverage; unavailable modes still fail admission. Presets do not move shared capture or apply automatically.

The browser validates the response's editing structure before allowing preview. Drafts retain reviewed settings such as `continuity` that have no form input, including through ordinary Apply settings and channel switches; Reset clears them. Independent reception and color changes still use current confirmed state. Closing the editor, replacing the service or applying Preset cancels its requests and invalidates old callbacks; a submitted server write may still complete.

## Runtime diagnostics

Decode and overview windows subscribe to existing receivers. Enabled receivers start acquisition and processing without browser/output subscriptions. Current and a saved channel are separate receivers even at the same frequency; each external receiver owns its decoder process. Digital reception is independent of companion audio squelch. Modes without an analog listener report `squelch:null`. Fault, recovery and backlog status take priority over audio/protocol waiting. Diagnostic and raw-field JSON panels include a Copy button.

Each receiver's `processing_statistics` contains bounded history keyed by receiver UID, service ID, processing generation and stream identity. `packets` and `searches` distinguish accepted, started, succeeded, failed, cancelled, rejected and in-flight work; only successful input contributes to media seconds. `occupancy` includes queued, executing, pending output and search return references with byte/sample counts and monotonic ages. `wall_ns`, available `thread_cpu_ns`, and queue wait are separate. Watermarks describe source blocks; `service_consumed` means drained by the service, not played by a remote speaker. `source_watermarks` connects shared input/PFB observations and fine channel clocks; observations need not be simultaneous. Model requested/effective state, squelch and protocol window retention are reported independently. Missing upstream decoder consumption is null. UTC changes, closed squelch and protocol window waiting do not imply missing source samples.

`sdr.actual.ddc_batch` reports the current number of physical IQS packets per PFB/fine job: 4 for 62.5 MSps hardware capture without a direct raw-IQ route, otherwise 1. Each physical packet is still checked for continuity; batching does not change output clocks or frame formats.

`sdr.actual.fine_owners.channels` reports each independent fine/direct owner with its stream identity, accepted/started/succeeded/failed/cancelled/rejected counts, retained bytes (including executing references), queue wait, wall time and available thread CPU time. `fine_faults` identifies failed source branches and their last completed / first unavailable input boundaries. A fine-source fault stops IQ, processors and recordings for that identity; other channels continue. An authorized explicit `recover:true` creates a new IQ identity even with unchanged settings. For a recorded interruption, respect `reception.manual_recovery_allowed`; device or internal faults require source or configuration repair first. Local reconfiguration waits up to 100 ms for its target to finish accepted input, collecting ready output from all lanes before closing the target queue. A busy rejection keeps its original owner and queue usable without creating a source fault or replacement owner. Input handoffs coalesce up to eight blocks or four milliseconds, with partial batches flushed at control/stop boundaries. Fine input is bounded to 1,024 queued blocks plus one executing block. Fine and processor byte budgets use 500 ms at the actual input rate and representation, clamped to 4–32 MiB per lane, including execution. Processor input also retains its 64-entry and two-second media limits. Per-lane fine diagnostics expose `input_bytes_limit`. Fine output (32 batches / 8 MiB) and publisher delivery (32 frames / 8 MiB per identity) are separate bounds; neither waits for downstream capacity. Display replacement is permitted; IQ gaps stop the affected source explicitly. Fine/framer aggregate wall time sums independent owners and is **not** single-thread utilization.
`processing_resources` is admission-time geometry under `policy:"observed_runtime"`; it is
not a reserved CPU or model-memory budget. Current `host_resources` observations and
`model_preparation` queue/RSS diagnostics are separate. Preparation refreshes headroom
and CPU parallelism at each queue-head grant attempt (maximum 64 waiting, 30 seconds).
Unknown memory cost starts one preparation; measured process RSS growth can limit parallelism
but cannot prove exclusive model size. Commit revalidates configuration revision; stale requests
leave current identities and transport epochs intact.
Queued tickets are removed on timeout or unwinding resource observation. Stop or identity invalidation revokes publication of stale results; queue waiting does not immediately respond to stop, and an already started native model call releases its objects on its owner thread after returning. Stopping therefore does not guarantee synchronous model release or an immediate process RSS reduction. cgroup v2 observations use the process's matching mount hierarchy and inherited quotas.

## Executable definitions and ownership

- [TypeScript state, commands and events](../../../frontend/src/core/multichannel.ts) define the adapter boundary.
- [Fixture adapter](../../../frontend/src/dev/fixtureReceiverAdapter.ts) simulates a server in memory with `transport=fixture`.
- [Client session](../../../frontend/src/ui/workbenchSession.ts) owns selection, monitoring and desired/in-flight/confirmed edits.
- [Shared IQ blocks and archive validation](../../../backend/src/iq_archive.rs) retain the v1 experiment; production uses the separate v2 module.
- [Language-neutral JSON Schema](../../../tests/fixtures/multichannel/draft-v1.schema.json) are available for backend integration.

The server owns capture, receivers, processors, outputs and recordings. The client owns view geometry,
fixed/follow bindings, selection and monitor routes. Closing views sends no recording or receiver command.
Local volume, pan and Solo are absent from server commands. The fixture opens no audio, writes no recording
files and resets tasks on reload. Backend IQ recording is available from actual capabilities.

## Identity, clocks and snapshots

Stable `receiver_uid` and `source_id` are independent of array order and display numbers.
Actual streams use `(source_id, epoch, channel_id)`. Baseband requirements include absolute frequency,
full passband, transition and a consumer minimum rate. An empty processor set is a valid IQ-only receiver.
Native rates retain their actual values rather than rounding 24414.0625 or 48840.130763116205 to 24000/48000.

Receiver configuration/service revisions, stream identities and exact integer/fractional sample positions use decimal JSON strings, including values above JavaScript's
safe integer range. This is not a universal encoding for diagnostics: `source_watermarks.input_samples` and fine-worker counters remain JSON integers. They cannot replace exact source clocks, and ordinary `JSON.parse` loses precision above the safe integer range.
Clocks retain integer and fractional positions, input/output rates, known or unknown
delay and UTC reliability. Without reliable hardware time association, `utc_reliable=false`.
Fixture clocks are simulated and cannot establish RF correctness.

Receiver, output and recording collections are dynamic. Capabilities publish `max_channels`; native operation
limits receivers to sixteen, while 128 remains a capacity target. The browser demo has its own configurable limit. Recording tasks contain independent
tracks with explicit source, format, compression, stream segments and end reasons. Production audio uses
`published_audio`, retaining actual backend processing and excluding browser listening gain/routing.
The legacy experimental `analysis_audio` label does not describe the current production source.

## Configuration, preflight and atomic commit

`configure` carries a request ID, expected receiver revision and complete candidate configuration.
Preflight returns acceptance, faults, candidate capture center, affected receivers/tracks and resource estimates.
Drafts belong to the editing view, service and receiver; preview precedes explicit apply. Switching channels keeps separate drafts; Reset, retirement or a service restart discards the affected draft.
Each receiver has one in-flight request and the latest queued candidate. Create/configure/retire commands share an ordered client queue because native admission commits against a shared configuration revision. Repeated retirement requests coalesce; deletion blocks later configuration, recording-start and output-start intents for that receiver. Selection cannot retarget a request.
Rejection preserves confirmed configuration and stream identity. Older snapshot revisions cannot overwrite newer state.
The native adapter adds transport timeouts, request-ID deduplication and service restart generations.
Session generations invalidate queued commands, late replies, edits and UID-bound monitor routes on service restart or disposal. A main-panel tune after retirement restores monitoring only after its exact successful command acknowledgment and a healthy receiver snapshot; rejection, disconnect and expiry cancel that intent. New receivers start with fresh audio defaults, and confirmed processor settings include their effective gain, AGC, squelch and NR parameters.

Fixture `scene` carries the full receiver set, shared center and expected service revision; production does not expose atomic scene commit. Preflight rejection changes
nothing; accepted fixture changes commit together. Loading configuration does not restart recording tasks.
Baseband changes create segments; shared capture changes update epoch. Physical commit failures must produce
capture faults and real boundaries, rather than a promise of seamless hardware rollback.

## Commands, events and fault examples

The `configure` example targets `POST /api/sdr/receivers`; substitute `service_id`, UID and revision from the current snapshot. Stream boundaries and faults illustrate adapter events, not `/api/config` commands:

```json
{"type":"configure","service_id":"service-current","request_id":"client-a-42","expected_revision":"7","receiver_uid":"rx-stable","config":{"name":"Unknown","color":"#76c9ed","tags":[],"enabled":true,"purpose":"iq","baseband":{"frequency_hz":7150000,"passband_hz":3000,"transition_hz":1000,"minimum_rate_hz":6000},"processors":[]}}
```

Other commands are `create`, `retire`, `output`, `record_start`, `record_stop` and `scene`.
Retiring a native receiver drains and ends its recording tasks; retained history is independent. Task states are preparing,
waiting/recording, finalizing, completed or failed. Adapter events include snapshot, stream_boundary and fault;
the fixture currently publishes observable state as snapshots rather than separate boundary events.

```json
{"type":"stream_boundary","receiver_uid":"rx-stable","previous":{"source_id":"rf-1","epoch":"8","channel_id":"29"},"next":null,"reason":"gap","clock":null,"drained":false}
```

```json
{"scope":"recording","code":"DISK_FULL","message":"No space left on device","receiver_uid":"rx-stable","task_id":"task-7","retryable":true}
```

Fault scopes are request, consumer, recording, receiver, receiver_iq and capture. Slow consumers fail independently;
disk errors do not stop healthy receivers. Capture faults affect dependent streams. Gaps end old segments;
recovery requires a new stream boundary. Delayed callbacks cannot revive completed or failed tasks.

## Native receiver API and IQ subscriptions

The production SDR **Channels** button opens one shared `NativeReceiverAdapter` and session for all workbench windows.
The independent Vite fixture entry remains available. Native UI uses actual mode capabilities for playback and decoder output. Backend IQ recording follows actual capabilities; fixture recordings and scenes remain disabled.

| Interface | Behavior |
|---|---|
| `GET /api/sdr/receivers` | Confirmed snapshot, boot `service_id`, capabilities, dynamic receiver list and actual clocks when samples exist |
| `POST /api/sdr/receivers/preflight` | Validate a complete create/configure/retire candidate without committing it |
| `POST /api/sdr/receivers` | Commit create/configure/retire; reply `{request_id,snapshot,fault}` |
| `/ws/iq?receiver_uid=...` | One receiver's ordered IQBF/IQAX v2 with JSON start and boundary metadata |

Native operation admits sixteen logical definitions. Boot-unique UIDs map explicitly to private legacy slots;
retiring/reusing a slot creates a new UID. A stopped definition retains its UID. `processors=[]` selects genuine IQ-only
operation through `RatePlan::baseband`; no dummy demodulator or receiver worker is started. Processors are admitted against actual provider and mode availability.

Commands have bounded idempotency receipts (128 recent request IDs); reusing an ID with different content is rejected.
`configure.expected_revision` is the receiver revision as a decimal string. The adapter namespaces request IDs and sends
`service_id`; a previous boot cannot submit a command to a restarted service. Admission is checked again before commit.
Rejected resource/RF/filter plans preserve confirmed state and identities. Metadata-only edits also advance revision.
A physical capture failure produces a capture fault and a non-drained boundary; rollback of arbitrary SDK failures is not promised.

A subscription starts only for an enabled UID. `stream_start` precedes its first binary frame and carries `receiver_uid`,
`source_id`, `stream`, `format=cf32_le`, `baseband`, string `first_sequence` and `clock`. The clock includes exact input/output
positions, fractional position, real input/output rates, filter delay in seconds and UTC reliability. Native integer DDC positions
have fraction 0/1; no rounded nominal rate is substituted. Late subscribers receive their actual first source position.
`stream_boundary` includes previous/next identity, clock, reason and `drained`; stop drains old tails before `next=null` and close.
Retune sends the old tail before boundary/new start. A source failure ends with `drained=false`; transport overflow closes with 1013.
Unexpected transport loss is a failed consumer. Unaffected lanes keep their identities and sample sequences.

IQ is encoded once and shared as immutable `Bytes` between transport consumers. Each native subscription has a 4 MiB pending
byte limit, 64 FIFO entries and a 2-second age/send limit; overload terminates that subscriber. The existing processing path still
parses encoded IQ for mixed demodulation; migrating it to typed shared sample blocks remains future work. This is no 128-lane capacity claim.
Snapshot `outputs=[]` is complemented by the adapter's local subscription states, actual received sample/byte counts and stream acknowledgements.
Closing, Dock, Pop out and selecting views send no receiver enable/stop commands; page disposal closes only its subscriptions.
Entering SDR no longer implicitly disables auxiliary slots, and reconnects do not replay an old receiver set. STATUS advertises `sdr.receiver_api=multichannel-draft-v1`; the browser then follows the actual service mode on first load instead of replaying its saved mode or AM detector setting.

With `--fake`, `capture.source_kind=fixture`: the service emits synthetic baseband at each actual output rate, with its own
honest baseband input clock and zero filter delay. It does not exercise RF PFB filtering. Native Rust tests separately feed
CS16 samples through PFB/fine DDC, compare an untouched lane sample-for-sample across another lane's cross-bin retune,
and verify old tails, rates and source positions. Hardware reception and sustained capacity still require a dedicated bench.

Run the isolated protocol/browser gate after frontend and native builds:

```sh
python tools/e2e/native_multichannel.py --artifacts /tmp/websa-p3-native
```

It owns an independent synthetic service and requires Playwright plus websockets. Existing service processes remain untouched.

## Channel monitoring and decoders

`capabilities.mode_descriptors` publishes mode/provider availability and reasons, minimum rates, default bandwidth,
audio companions and result types. The workbench Signal settings tab previews and submits actual analog/digital modes;
unavailable dependencies remain visible and disabled. Each receiver supports zero or one primary processor.
Optional `bandwidth_hz`, `pcm_gain` (0..2), `agc` and `squelch_dbm` configure backend processing.
`pcm_gain` changes shared PCM for every consumer; protocol input continues to bypass monitor enhancement.

| Interface | Behavior |
|---|---|
| `/ws/audio?receiver_uid=...` | Existing AUD2 for one enabled logical receiver with audio capability; mutually exclusive with `channel` |
| `/ws/decode?receiver_uid=...&ack=1` | Ordered SDE2, retained image snapshots and application ACKs for one receiver; mutually exclusive with `channel` |

On a healthy capture, UID audio/decode subscriptions close with `1000 / receiver retired` after normal retirement,
or `1000 / receiver stopped` when the channel is explicitly disabled or its processor is removed. Clients clear playback
buffers and meters, then wait for the directory update or a new enable action. These two explicit terminal reasons do not
report a fault or trigger automatic reconnection. Verified decode events drain through the bounded local FIFO before its
terminal notification; other closes retain their existing transport/failure handling.

SDE2 adds `receiver_uid`, `service_id` and `baseband_stream`. Attribution follows the actual stream mapping,
including retiring tails. Reusing a slot cannot redirect an old subscription to a new UID. IQ-only processed-output
requests return 409. Binary layouts and legacy slot endpoints remain compatible. Independent-baseband processors
have their own `(epoch, channel_id)`; processing identity must not be assumed to equal IQ identity.
Joining continuous IQ starts processing at the actual first consumed source position without backfilling missed samples.
Source positions, fractions, actual rates and known/unknown delays retain their existing semantics.

Native PFB/DDC compares actual source plans. When frequency, route, full passband, transition and decimation remain
equal, adding/removing/switching processors preserves IQ identity and samples. Queued old IQ is consumed before
processing tails drain and replacement begins; squelch status follows processing identity.
Decoder recovery does not restart healthy IQ. Actual baseband/filter changes retain old tails and create new identities.
Changing fake modes can replace the generated signal/replay file and actual rate, so it produces a real IQ boundary;
that behavior does not validate RF continuity.

The browser shares one AudioContext and one existing worker/worklet per monitored UID. Local GainNode and
StereoPannerNode feed a master gain and peak compressor. Gain/pan ramp over 15 ms; leaving Solo restores the manual
mix. Follow selection changes only local consumption. Master mute keeps audio consumption; suspended autoplay does
not subscribe to PCM, and gestures in either the parent or Pop out resume playback. Main-panel audio subscribes by
confirmed service identity and UID. When the workbench takes speaker ownership, it reuses matching main-panel
workers/worklets and buffered PCM. Mismatched legacy consumers still stop to prevent duplicate output or slot reuse
from transferring playback to another receiver. Legacy panel volume remains shared backend PCM
gain; workbench Local speakers controls are the current user's gain. Native audio has a 4 MiB/64-entry/2-second
budget; decode retains bounded snapshots, queues and ACKs.

Fixed/follow Decode uses bounded UID logs and the existing result renderer for FT8 tables, CW/provider text, status,
provider PNG strips and native SSTV/fax RGB/gray rows. Results retain processing identity and source timing; interrupted
pictures remain partial. Native UID logs retain at most 200 entries/24 MiB, with a 20 MiB native picture limit; retired
UID caches are released. Legacy slot logs retain their original budgets and specialized panels. Result consumption is
independent of views; opening windows creates no producers. Closing the parent only ends local consumers.

UID audio uses capped backoff for temporary transport loss. A new actual start boundary and cleared buffers are required before playback resumes, with reconnecting shown in the UI. Disabling monitoring cancels retries; receiver/protocol/slow-consumer failures do not retry automatically. Retirement and service generation changes remove consumer owners before releasing history and reject stale callbacks. A direct per-channel Listen action during follow switches to manual mixing while preserving the editor target and previously monitored source.

The processing worker still parses encoded IQ once. Processing identity/sequence translation now passes typed metadata,
avoiding a full IQ copy or re-encoding. Sharing native sample blocks all the way from fine/framing to processing is
still pending; this is no claim of an entirely zero-copy pipeline or sustained capacity.

```sh
python tools/e2e/native_multichannel_processing.py --artifacts /tmp/websa-p4-processing
python tools/e2e/native_multichannel_recovery.py --artifacts /tmp/websa-p4-recovery
```

This gate owns a synthetic/replay service and checks UID transport, actual rendered browser PCM gain/pan/master mute,
manual mix, Solo/follow, FT8 text and SSTV pixels. It does not validate RF hardware, subjective speaker quality,
every external provider or sustained capacity.

## Recording and experimental boundaries

See [Recording reference](RECORDINGS.md) for task lifecycle, current audio formats, range reads
and SigMF export. Experimental WBIQ0001 lives in `backend/src/iq_archive/v1.rs` and builds
only with `iq-experiments`; production uses WBIQ0002. Fixture scenes do not establish native atomic-scene support.
