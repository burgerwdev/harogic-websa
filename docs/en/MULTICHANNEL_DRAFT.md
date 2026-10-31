# Multichannel P2 state and interface draft

Status: draft-v1, a development prototype and offline experiments, not a published service API.
Baseline: `3978ed2439f0bf9385414122e3e720b0a63f2b99`.

## Executable definitions and ownership

- [TypeScript state, commands and events](../../frontend/src/core/multichannel.ts) define the adapter boundary.
- [Fixture adapter](../../frontend/src/dev/fixtureReceiverAdapter.ts) simulates a server in memory with `transport=fixture`.
- [Client session](../../frontend/src/ui/workbenchSession.ts) owns selection, monitoring and desired/in-flight/confirmed edits.
- [Shared IQ blocks and experimental archive](../../backend/src/iq_archive.rs) use the optional `iq-experiments` feature.
- [Language-neutral JSON Schema](../../tests/fixtures/multichannel/draft-v1.schema.json) are available for backend integration.

The server owns capture, receivers, processors, outputs and recordings. The client owns view geometry,
fixed/follow bindings, selection and monitor routes. Closing views sends no recording or receiver command.
Local volume, pan and Solo are absent from server commands. The fixture opens no audio, writes no recording
files and resets tasks on reload. Persistent recording belongs to P5.

## Identity, clocks and snapshots

Stable `receiver_uid` and `source_id` are independent of array order and display numbers.
Actual streams use `(source_id, epoch, channel_id)`. Baseband requirements include absolute frequency,
full passband, transition and a consumer minimum rate. An empty processor set is a valid IQ-only receiver.
Native rates retain their actual values rather than rounding 24414.0625 or 48840.130763116205 to 24000/48000.

Every u64, revision and sample position uses a decimal JSON string, including values above JavaScript's
safe integer range. Clocks retain integer and fractional positions, input/output rates, known or unknown
delay and UTC reliability. Without reliable hardware time association, `utc_reliable=false`.
Fixture clocks are simulated and cannot establish RF correctness.

Receiver, output and recording collections are dynamic. Capabilities publish `max_channels`; the fixture
initially limits operation to three, while 128 remains a capacity target. Recording tasks contain independent
tracks with explicit source, format, compression, stream segments and end reasons. Analysis audio precedes
local listening gain and routing.

## Configuration, preflight and atomic commit

`configure` carries a request ID, expected receiver revision and complete candidate configuration.
Preflight returns acceptance, faults, candidate capture center, affected receivers/tracks and resource estimates.
Drafts belong to the editing view; preview precedes explicit apply. Cancel or target changes discard a draft.
Each receiver has one in-flight request and the latest queued candidate. Selection cannot retarget a request.
Rejection preserves confirmed configuration and stream identity. Older snapshot revisions cannot overwrite newer state.
Transport timeouts, server request-ID deduplication and restart generations remain P3 work.

`scene` carries the full receiver set, shared center and expected service revision. Preflight rejection changes
nothing; accepted fixture changes commit together. Loading configuration does not restart recording tasks.
Baseband changes create segments; shared capture changes update epoch. Physical commit failures must produce
capture faults and real boundaries, rather than a promise of seamless hardware rollback (P3/P6).

## Commands, events and fault examples

These are adapter messages, not commands already accepted by `/api/config`:

```json
{"type":"configure","request_id":"client-a-42","expected_revision":"7","receiver_uid":"rx-stable","config":{"name":"Unknown","color":"#76c9ed","tags":[],"enabled":true,"purpose":"iq","baseband":{"frequency_hz":7150000,"passband_hz":3000,"transition_hz":1000,"minimum_rate_hz":6000},"processors":[]}}
```

Other commands are `create`, `retire`, `output`, `record_start`, `record_stop` and `scene`.
Stop active tasks before retiring definitions; retained history is independent. Task states are preparing,
waiting/recording, finalizing, completed or failed. Draft events include snapshot, stream_boundary and fault;
the fixture currently publishes observable state as snapshots rather than separate boundary events.

```json
{"type":"stream_boundary","receiver_uid":"rx-stable","previous":{"source_id":"rf-1","epoch":"8","channel_id":"29"},"next":null,"reason":"gap","clock":null}
```

```json
{"scope":"recording","code":"DISK_FULL","message":"No space left on device","receiver_uid":"rx-stable","task_id":"task-7","retryable":true}
```

Fault scopes are request, consumer, recording, receiver and capture. Slow consumers fail independently;
disk errors do not stop healthy receivers. Capture faults affect dependent streams. Gaps end old segments;
recovery requires a new stream boundary. Delayed callbacks cannot revive completed or failed tasks.

## Compatibility and the P3 entry

Existing `/ws?iq=1`, `/ws?iqaux=1`, IQBF/IQAX, AUD2 and SDE2 remain unchanged. P3 will map stable UIDs
to legacy slots explicitly; list position is not a slot. No unified subscription endpoint is implemented yet.
Start P3 at `channel_controls.rs`, `service_ddc.rs` and `receiver_workers.rs`, aggregate processor/consumer
demands through `RatePlan::baseband`, publish confirmed snapshots and stream-start metadata, then replace
the fixture adapter. SharedIqBlock currently carries immutable CF32 LE bytes in an Arc; production may use
Arc-backed f32 samples and encode only at network/archive boundaries. Experimental copies are not an optimization.

## P2 archive constraints

WBIQ0001 contains a file header, BLK1 records and a final IDX1 index. Blocks are bounded to 1 MiB with a
0.25-second candidate duration. Zstd levels 1–3 fall back to raw storage when compression is larger.
Each block retains track/receiver/stream identity, position, fractional clocks, actual rates, sample count,
revision, codec, lengths, offset and checksum. FNV-1a64 detects corruption, not authenticity; P5 versions the
final format. Finishing consumes the writer, appends the index and flushes; the experiment also syncs the file.
Scanning incomplete tails preserves earlier complete blocks and reports incomplete status. Corruption fails
explicitly. Indexed seeks validate both metadata and payload; compressed bytes are not ordinary SigMF samples.

The offline scanner retains decoded blocks for verification; reported RSS is cumulative process high-water memory. The real FT8 fixture has an actual sample rate but no original RF timestamp; archive positions are fixture-relative with a test origin above 2^53, not recovered hardware time.
Bounded recording workers, queue budgets, volumes, durable recovery indexes and product playback remain P5 work.
