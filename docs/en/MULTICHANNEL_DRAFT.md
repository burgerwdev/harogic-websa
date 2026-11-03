# Multichannel P2–P5 state and interface draft

The capture snapshot reports `state:running|idle|failed` separately from device connectivity. `parameters_state:live|last_known|unknown` identifies current source geometry, retained previous geometry, or an unopened source. Bandwidth and input rate are null when unknown; in that case `center_hz` is only the requested center and epoch `"0"` is a sentinel, not a live stream. Independent fixture lanes use `parameters_state:simulated`, a nominal shared display window/rate and sentinel epoch `"0"`; their per-receiver clocks describe the actual baseband inputs. For a hardware source, input rate is the PFB input rate and epoch is the DSP source epoch (not the hardware configuration generation or downstream DDC rate). Failure/stop retains source geometry; a reopened source gets a new DSP epoch. Enabled hardware receiver changes require a live, healthy capture. Older servers omit `parameters_state` and may report legacy defaults after failure.

Entering Center in the main panel sets the shared capture center and tunes C1 to that frequency in one admitted change; an existing zoom window keeps its width and slides to that point. IQ BW changes capture bandwidth while retaining receiver frequencies and modes. To Center moves capture to C1's current frequency. Other main tuning/demodulation/shared PCM controls still target C1; select and edit each UID in Channels for multichannel work. Capture changes can interrupt every receiver and create recording boundaries.


The first P6 slice is `receiver_presets`: server-persisted single-channel confirmed `purpose/baseband/processors` with an independent decimal-u64 list revision. Saving/deleting changes no receiver plan. Explicit loading reuses `configure`, preserves target metadata/enabled state, and carries no UID, active task or local monitor state. A stale preview cannot overwrite a later configuration. See [API](API.md) for protocol and limits; persistent receiver definitions and atomic multi-channel scenes remain unimplemented.

`audio_archive_read` adds read-only sample-frame selection and bounded native-media playback for finished published-audio tasks, including original-identity failed prefixes. `audio_range_max_samples` is a u64 string (`"1048576"`), and `audio_range_max_seconds` is 60. Output keeps original Float32 bits, actual rate/channels/configuration and unchanged AUD2 clock rows with exact decimal trim offsets. No live UID is rebound, no protocol/analysis source is added and no recording resumes. Original task validation, snapshot preparation and transmission share the existing single-reader lease. The dedicated file player/download is independent of live listening. See [API](API.md), [DEVELOPMENT](DEVELOPMENT.md) and `native_multichannel_audio_playback.py`.

`iq_file_demodulate_offset` adds range-local, phase-continuous signed frequency tuning before native analogue file processing. Offset defaults to zero; original recorded frequency and clocks remain in source SigMF, while render provenance separately records selected frequency and translated band. Both band edges must fit original IQ and Nyquist. The browser enables these controls only on capable services. Bounds and cancellation are unchanged. See [API](API.md) and `native_multichannel_tuning.py`.

Capability `iq_file_demodulate` adds read-only, range-bounded analogue file processing; `iq_file_demodulate_modes` advertises AM/USB/LSB/NFM and the duration cap is 60 seconds. Original task/service/UID and per-block u64/fraction clocks remain in provenance. New relative-time Float32 audio is a derived snapshot, not a resumed recording, protocol input or live receiver. No slot is allocated or rebound. The common reader lease/bounded transport covers native DSP and delivery. The file player and exported tar share the same render; view edits/hide/disposal cancel and release it. See [API](API.md) and `native_multichannel_demod.py`; digital file decoding remains subsequent work.

`audio_flac_export` adds a read-only FLAC export for finished published-audio tasks, including original-identity recovered prefixes. Each existing source segment becomes signed-24 PCM FLAC at its original rate/channels, using fixed-scale rounding and explicit saturation counts. This is lossless for the quantized integer input; original Float32 bits remain in WBAU0001 and the WAV export. Task/UID/service and unchanged per-frame provenance accompany the files. The bounded offline transform shares the single download lease without creating live receivers. No new source kind, automatic recording restart or Opus support is implied. See [API](API.md) and `native_multichannel_flac.py`.

P5 also supports `kinds:["audio"]` and `audio_archive_export` for actual published AUD2 PCM, including digital companions and codec output by descriptor capability. `source:"published_audio"` / `format:"f32_le"` distinguish it from protocol-input or analysis audio. Packet-owned configuration, precise source clocks and original sample bits survive source/format boundaries and verified-prefix recovery. Finished tasks download as Float32 WAV segments plus a frame timeline; browser speaker controls are excluded. These are backend tasks sharing the existing three-worker budget. Protocol-input recording, Opus and digital file decoding remain future work. See [API](API.md) and the independent `native_multichannel_audio.py` gate in [DEVELOPMENT](DEVELOPMENT.md).

P5 now also delivers backend `kinds:["results"]` tasks and `results_archive_export`, using the actual published SDE2 branch. A separate task may record IQ concurrently on the same UID. The workbench derives availability from mode outputs, including analog telemetry, rather than `purpose`. See [API](API.md) for WBRS0001 recovery, exact incremental image payloads and checked tar/JSONL download semantics. Browser message/PNG exports remain bounded session snapshots. Protocol-input audio, Opus and digital file decoding remain unimplemented.

Status: draft-v1, P2 fixture/experiments, native P3 receiver/IQ and P4 monitoring/results plus P5 backend IQ recording slices; an evolving service contract.
Baseline: `3978ed2439f0bf9385414122e3e720b0a63f2b99`.

## Executable definitions and ownership

- [TypeScript state, commands and events](../../frontend/src/core/multichannel.ts) define the adapter boundary.
- [Fixture adapter](../../frontend/src/dev/fixtureReceiverAdapter.ts) simulates a server in memory with `transport=fixture`.
- [Client session](../../frontend/src/ui/workbenchSession.ts) owns selection, monitoring and desired/in-flight/confirmed edits.
- [Shared IQ blocks and experimental archive](../../backend/src/iq_archive.rs) retain the v1 experiment; production uses the separate v2 module.
- [Language-neutral JSON Schema](../../tests/fixtures/multichannel/draft-v1.schema.json) are available for backend integration.

The server owns capture, receivers, processors, outputs and recordings. The client owns view geometry,
fixed/follow bindings, selection and monitor routes. Closing views sends no recording or receiver command.
Local volume, pan and Solo are absent from server commands. The fixture opens no audio, writes no recording
files and resets tasks on reload. Backend IQ recording is available from actual capabilities.

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
Retiring a native receiver drains and ends its recording tasks; retained history is independent. Task states are preparing,
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
The independent Vite fixture entry remains available. Native UI uses actual mode capabilities for playback and decoder output. Backend IQ recording follows actual capabilities; fixture recordings and scenes remain disabled.

| Interface | Behavior |
|---|---|
| `GET /api/sdr/receivers` | Confirmed snapshot, boot `service_id`, capabilities, dynamic receiver list and actual clocks when samples exist |
| `POST /api/sdr/receivers/preflight` | Validate a complete create/configure/retire candidate without committing it |
| `POST /api/sdr/receivers` | Commit create/configure/retire; reply `{request_id,snapshot,fault}` |
| `/ws/iq?receiver_uid=...` | One receiver's ordered IQBF/IQAX v2 with JSON start and boundary metadata |

Native operation initially admits three logical definitions. Boot-unique UIDs map explicitly to private legacy slots;
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

## P4 channel monitoring and existing decoders

`capabilities.mode_descriptors` publishes mode/provider availability and reasons, minimum rates, default bandwidth,
audio companions and result types. The workbench Signal settings tab previews and submits actual analog/digital modes;
unavailable dependencies remain visible and disabled. Each receiver supports zero or one primary processor.
Optional `bandwidth_hz`, `pcm_gain` (0..2), `agc` and `squelch_dbm` configure backend processing.
`pcm_gain` changes shared PCM for every consumer; protocol input continues to bypass monitor enhancement.

| Interface | Behavior |
|---|---|
| `/ws/audio?receiver_uid=...` | Existing AUD2 for one enabled logical receiver with audio capability; mutually exclusive with `channel` |
| `/ws/decode?receiver_uid=...&ack=1` | Ordered SDE2, retained image snapshots and application ACKs for one receiver; mutually exclusive with `channel` |

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
not subscribe to PCM, and gestures in either the parent or Pop out resume playback. The workbench owns speakers in
this page and pauses legacy playback to prevent duplicate output. Legacy panel volume remains shared backend PCM
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

## P2 archive constraints

WBIQ0001 contains a file header, BLK1 records and a final IDX1 index. Blocks are bounded to 1 MiB with a
0.25-second candidate duration. Zstd levels 1–3 fall back to raw storage when compression is larger.
Each block retains track/receiver/stream identity, position, fractional clocks, actual rates, sample count,
revision, codec, lengths, offset and checksum. FNV-1a64 detects corruption, not authenticity; P5 versions the
final format. Finishing consumes the writer, appends the index and flushes; the experiment also syncs the file.
Scanning incomplete tails preserves earlier complete blocks and reports incomplete status. Corruption fails
explicitly. Indexed seeks validate both metadata and payload; compressed bytes are not ordinary SigMF samples.

The offline scanner retains decoded blocks for verification; reported RSS is cumulative process high-water memory. The real FT8 fixture has an actual sample rate but no original RF timestamp; archive positions are fixture-relative with a test origin above 2^53, not recovered hardware time.
The v1 experiment has no real-time worker or product playback. Production IQ recording uses v2 below; product playback remains subsequent work.

The workbench and receiver Decode preserve live DOM nodes through status updates, including buttons activated on keyboard release,
drafts, selection, scrolling and disclosures. The Decode view combines message batches into a sortable/filterable table, exposes UTC,
keeps picture history and reuses the live raster/PNG nodes. AM/WFM/DRM and decoder progress use direct readings; native status snapshots
are keyed by event type so monitor NR cannot overwrite decoder telemetry. Clear view confirms before hiding this view's messages and
pictures; live status and other views' shared bounded history remain available. It never changes reception or backend recording.
Times use browser receipt time unless the result explicitly supplies a protocol slot UTC. Native FT8 preserves its reported time offset;
source positions remain in the retained event metadata, without inferring absolute RF UTC.

The product treats processing choice, local speakers, backend recording and output consumers as independent decisions.
Signal settings has one mode selector, including no processor; it derives the internal `purpose` field for API compatibility.
Separate Processing/Results tabs and duplicate mode/removal controls are removed. Existing saved tabs migrate to Signal settings.
Live outputs identifies actual per-UID IQ/audio/results WebSocket endpoints; a local IQ delivery preview is folded and explicitly does not save files.
Action tooltips distinguish channel selection, separate settings, signal/results viewing, speaker routing and backend recording.

Audio-only views omit text filtering and clear actions. AM/WFM reuse `audioScope.ts` from the legacy Decode window,
CW retains a separate `cw-level` snapshot alongside incremental text, and DRM exposes station, services and lock readings.
Message CSV/JSONL downloads apply the current mode/text filter to every retained page; they are session snapshots.
JSONL preserves service/UID, source identity and precise source timing, with browser receipt time labelled separately from explicit protocol UTC.
CSV escapes text and prevents formula interpretation while leaving numeric measurements numeric. PNG exports the selected retained picture,
labels unfinished/partial pictures in filenames, preserves neutral missing rows and bounds RGBA memory to 20 MiB.
These exports send no receiver/recording command and remain separate from backend continuous recording and IQ archive download/playback/SigMF.

## P5 backend IQ recording

The service publishes `record_start`/`record_stop` and `recording_kinds:["iq","results"]` before the workbench enables recording. Tasks progress through preparing, recording, finalizing and completed/failed, currently with one IQ baseband or published-results track per task. Storage is exclusive per instance. Tasks retain receiver_uid, service_id, track_id and actual epoch/channel_id. Tuning/rate changes create segments; receiver stop drains and completes its task. Gaps/capture errors fail the task and retain verified data. Views and listening do not configure recording. Restart restores history as SERVICE_INTERRUPTED without resuming.

Before acknowledging a successful start, the worker syncs the first volume, initial index and task manifest, then enables sample admission. The API releases acquisition state before waiting for up to two seconds; concurrent retries share preparation and one receipt. An immediate service kill after acknowledgement still restores the preparing task.

Production **WBIQ0002** is separate from experimental **WBIQ0001**; neither is SigMF. Each `.wbiq` volume starts with the 8-byte magic. Record prefixes contain a 4-byte kind, u32le JSON length, u32le payload length and u32le CRC32. CRC32/IEEE covers the first 12 prefix bytes, original JSON bytes and payload. BLK2 JSON contains meta, codec and raw_crc32; payload is exact CF32 LE or an independent Zstd level 1 frame, falling back to raw unless compression reduces size. raw_crc32 checks decompressed bytes. Metadata includes original service, segment, sequence, frequency/baseband and IqBlockMeta (actual rate, source/output positions, fraction, delay, revision and decimal u64 strings). Native integer decimation uses fraction 0/1; UTC remains unreliable.

IDX2 pages hold 128 block indexes; only one page stays in memory. JSONL sidecars sync each block. EVT2 stores segment boundaries. END2 verifies block/sample totals and CRC32 of the preceding record CRC32 little-endian sequence; a checked index is required for completion. Volumes roll at 256 MiB or 4096 blocks without changing source segments. Metadata is limited to 256 KiB, blocks to 1 MiB and each task to 4096 segments, with explicit failure on excess. Stop seals admission, drains accepted blocks, writes indexes/footer, fsyncs, then atomically replaces and syncs task.json.

Recovery verifies one block at a time and rebuilds JSONL indexes. A torn final record recovers its checked prefix; corruption fails explicitly without skipping damaged records or modifying the original archive. Each task has 4 MiB/64 items/2 seconds including in-flight work, with at most three workers. Backlog failures report admitted/saved bytes and cannot claim unwritten samples complete. Compression/disk errors do not stop reception or healthy network consumers. Finished IQ tasks support bounded waveform playback and sample-range SigMF export as described below; protocol-input audio, Opus and digital file decoding remain subsequent work.


Finished-task reading uses `/api/sdr/recordings/{task_id}/range` (see [API](API.md)). Each request selects a single segment and 1–1,048,576 complex samples, with at most 4096 checked blocks and 4 MiB of provenance. Sidecars locate the blocks; v2 verifies their full metadata, compressed record CRC and raw bytes before returning any success. Gaps, overlaps, changed identity/rate/frequency/baseband and out-of-bounds ranges fail. Reads span volumes without modifying originals or receivers. One blocking read/download is admitted at a time, with its permit retained through response delivery; the browser owns one waveform buffer across all windows.

The `.sigmf` download is a POSIX tar containing `recording/recording.sigmf-data` (bare interleaved CF32 LE) and `recording/recording.sigmf-meta` (SigMF 1.2.6). Actual fractional sample rate is `core:sample_rate`; capture frequency is `core:frequency`. Every export rebases sample indices to zero, with no invented `core:datetime`. Optional `websa` extension version 1.0.0 carries task/service/UID, source segment, selected start/count, original task fault and baseband configuration in Global. Each Annotation gives dataset-relative `core:sample_start`/`core:sample_count`, the full original `websa:clock` (`IqBlockMeta`), decimal `websa:skip_samples` within that block, original sequence and configuration revision. This preserves exact u64/fraction/delay provenance even for partial blocks, without rounding source positions through JavaScript. Unknown absolute UTC stays explicit. Different segments/rates remain separate Recordings; the collection endpoint packages them together.

Independent-reader gate: `python tools/e2e/native_multichannel_archive.py --artifacts /tmp/websa-iq-archive --sigmf-python /path/to/sigmf-venv/bin/python`. Install official `sigmf==1.6.1` in that separate environment (schema 1.2.6); Python/browser tools remain development-only. The gate checks actual network/archive/range bytes, official SigMF parsing, real waveform/seek, 420px downloads/Dock and history after restart. Waveform playback inspects recorded complex samples; it does not run the live receiver DSP or create audio.


The streaming `/export` endpoint and `iq_archive_export` capability supplement bounded `/range` reads. Its maximum is 1,073,741,823 complex samples, with each tar member within POSIX ustar limits. It shares checked range traversal and reader admission, prepares without retaining all data/annotations, then streams and revalidates data plus provenance. Its two-item 64 KiB queue fails on two seconds of backlog. Response drop cancels the producer; terminal write errors prevent retry loops. Existing `/range` limits and exact u64/fraction provenance remain unchanged. The UI uses native browser downloads for the range or whole segment; only waveform preview owns an 8 MiB buffer. Different stream segments remain independent Recordings, including in a collection.

### Cross-segment SigMF Collection

The `/collection` endpoint exports all nonempty verified segments of one finished task, up to 4096 segments and 1,073,741,823 total samples. Capability `iq_archive_collection` gates the UI; the service publishes and enforces the collection limits. Root-level metadata/data pairs retain their own true sample rates, frequencies and original clocks; `recording.sigmf-collection` orders their basenames and SHA-512 metadata hashes. All segments are checked before success. It shares streaming export admission, cancellation and bounded transport; only per-segment plans (2 MiB prefixes/IDs) and the collection manifest (2 MiB) accumulate. Damaged later members reject preparation or terminate incomplete delivery, without substituting a healthy subset. Failed tasks expose only their recovered checked prefix with their original UID/service/fault. No DSP replay, cross-segment sample concatenation or absolute time alignment is performed. See [the specification](https://sigmf.org/#sigmf-collection-format) and the independent-reader/schema discrepancy documented in [DEVELOPMENT](DEVELOPMENT.md).
