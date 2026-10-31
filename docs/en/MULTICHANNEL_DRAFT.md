# Multichannel P2/P3 state and interface draft

Status: draft-v1, P2 fixture/experiments plus the native P3 receiver/IQ slice; an evolving service contract.
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
The native P3 adapter adds transport timeouts, request-ID deduplication and service restart generations.

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

## Native P3 receiver API and IQ subscriptions

The production SDR **Channels** button opens one shared `NativeReceiverAdapter` and session for all workbench windows.
The independent Vite fixture entry remains available. Native UI does not enable fixture recording, scenes, monitoring
mixes or generated Decode results. P4/P5 connect those capabilities separately.

| Interface | Behavior |
|---|---|
| `GET /api/sdr/receivers` | Confirmed snapshot, boot `service_id`, capabilities, dynamic receiver list and actual clocks when samples exist |
| `POST /api/sdr/receivers/preflight` | Validate a complete create/configure/retire candidate without committing it |
| `POST /api/sdr/receivers` | Commit create/configure/retire; reply `{request_id,snapshot,fault}` |
| `/ws/iq?receiver_uid=...` | One receiver's ordered IQBF/IQAX v2 with JSON start and boundary metadata |

Native operation initially admits three logical definitions. Boot-unique UIDs map explicitly to private legacy slots;
retiring/reusing a slot creates a new UID. A stopped definition retains its UID. `processors=[]` selects genuine IQ-only
operation through `RatePlan::baseband`; no dummy demodulator or receiver worker is started. Processing modes may be
retained through the compatibility layer; their complete workbench playback/results integration belongs to P4.

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
