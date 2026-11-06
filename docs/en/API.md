# API Documentation

Audio recording formats and file management: `record_start` accepts optional `audio_format:"wav"|"flac"|"opus"` for `kinds:["audio"]` (default WAV). The task retains `audio_format` and an initial channel `name` across restarts. Checked original Float32 journals remain the source for playback and later export; this setting chooses the initial download format. `audio_opus_export:true` enables `GET /api/sdr/recordings/{task_id}/audio/opus` without query parameters. The server needs FFmpeg with `libopus`; capability probing occurs at startup. The tar contains `task.json`, unchanged `frames.jsonl` and one Ogg Opus file per nonempty source segment: lossy VBR, 48 kHz, 48 kb/s mono or 96 kb/s stereo. Source boundaries remain separate, including very short segments. Encoded files use a private temporary directory (2 GiB total encoded limit; 64 MiB provenance limit), removed on completion, failure or cancellation. Preparation/streaming share the existing reader lease; live acquisition does not run the encoder.

`record_rename` (`task_id`, `name`) and `record_delete` (`task_id`) use the existing `/api/sdr/receivers` command endpoint with `request_id` and service identity. Only finished tasks and fully drained failed tasks can be managed. Names are trimmed, 1–128 characters, with no control characters. Rename persists metadata without renaming source files; delete removes the whole recording. Both refuse an occupied archive reader lease, perform filesystem work outside acquisition state, and retain idempotent command receipts. Deletion first moves the directory to a hidden tombstone so interrupted cleanup cannot resurrect a task. The workbench file library collects current and retired channels, with search, type/status filters, playback/export, inline rename and explicit delete confirmation. Checkboxes support multi-select and select all of the finished recordings in the current filtered list. Batch deletion shows the count and combined size before confirmation, commits one file at a time, and leaves failed files selected for retry. File, total and selected sizes use decimal B / KB / MB / GB / TB units with at most one decimal place.

The capture snapshot reports `state:running|idle|failed` separately from device connectivity. `parameters_state:live|last_known|unknown` identifies current source geometry, retained previous geometry, or an unopened source. Bandwidth and input rate are null when unknown; in that case `center_hz` is only the requested center and epoch `"0"` is a sentinel, not a live stream. Independent fixture lanes use `parameters_state:simulated`, a nominal shared display window/rate and sentinel epoch `"0"`; their per-receiver clocks describe the actual baseband inputs. For a hardware source, input rate is the PFB input rate and epoch is the DSP source epoch (not the hardware configuration generation or downstream DDC rate). Failure/stop retains source geometry; a reopened source gets a new DSP epoch. Enabled hardware receiver changes require a live, healthy capture. Older servers omit `parameters_state` and may report legacy defaults after failure.

Entering Center in the main panel sets the shared capture center and tunes C1 to that frequency in one admitted change; an existing zoom window keeps its width and slides to that point. IQ BW changes capture bandwidth while retaining receiver frequencies and modes. To Center moves capture to the selected receiver's frequency. Main Listen, demodulation and shared PCM controls target the channel shown beside Listen; its arrows select any defined channel without retuning. Workbench editing selection remains local to its views; main-panel selection also focuses that receiver in the workbench. Capture changes can interrupt every receiver and create recording boundaries.


`audio_archive_read:true`, `audio_range_max_samples:"1048576"` and `audio_range_max_seconds:60` enable `GET /api/sdr/recordings/{task_id}/audio/range?segment=audio-000000&start=0&count=20000`. Start/count are unsigned decimal **sample frames** relative to one audio segment (a stereo frame includes both channels), with checked u64 arithmetic. Only finished audio tasks or finalized failed prefixes are readable. The selection must contain 1–1,048,576 frames and at most 60 seconds at the original 8000–192000 Hz integer rate, preserving mono/stereo. Unknown parameters/invalid counts return 400; active/finalizing 409, missing 404, bad segment/range/data 422, common reader busy 429.

The bounded `audio-range.tar` has `selection.json`, `frames.jsonl` and `audio.wav` (IEEE Float32 LE with fact). PCM is bit-exact published audio: original shared processing remains; this read applies no gain, resampling, quantization, normalization or silence filling. `selection.json` preserves the original task/service/UID/fault and selection-relative start/count, exact first audio index, actual format and PCM/timeline SHA-256. Each selected timeline row retains the complete original AUD2 header and source clocks unchanged; its additional `selection` records decimal `record_skip_frames`, `frames` and zero-based `range_sample_start`. No fractional source-clock interpolation or absolute UTC is inferred from a partial frame. Native media time starts at the first selected audio sample.

`recording_audio_read.rs` stream-validates the **entire** existing journal and segment summaries before success; journals currently have no random-access audio index, so preparation work scales with total task length. Memory retains one checked record, at most 8 MiB selected PCM plus its 58-byte WAV header, 4096 selected records / 4 MiB timeline and 2 MiB manifest. The complete immutable snapshot then uses the existing preparation/producer/body reader lease, two 64 KiB chunks and 2-second transport backpressure. Cancelled requests release the lease after both owners exit. Original recording files remain unchanged. The browser lazily loads one dedicated native-media player, checks a 16 MiB envelope before allocation and bounds streamed bytes; edits, hiding, task replacement and disposal abort/release prior playback. Pop out/Dock moves the same media node; download saves the exact loaded snapshot. Live monitor controls and file playback are independent. Old services hide this entry. This is bounded original-PCM playback; continuous long-file playback, direct Opus playback and digital file decoding remain separate work. Opus export is available through the separate endpoint described above.

`iq_file_demodulate_offset:true` additionally enables optional finite `offset_hz` on the file-demodulation endpoint (default `0`). Positive offsets select higher frequencies relative to the original recorded IQ centre. The shared native NCO applies `IQ * exp(-j*2*pi*offset_hz*n/actual_iq_rate_hz)` before the original IF FIR, with zero phase at the selected range start and continuous phase across checked blocks/volumes. Every nonzero offset, including sub-Hz values, is applied; zero bypasses mixing and preserves the original render. Both shifted band edges must lie inside the recorded symmetric passband and actual IQ Nyquist, with no frequency wrapping. USB/LSB retain their asymmetric native sidebands. `render.json.processing.tuning` records offset, original centre, selected frequency, selected input-band edges and phase policy. `source.sigmf-meta` and its CF32 hash still describe the unchanged original IQ at its original centre. No sample/time shift, pre-roll or new delay estimate is introduced. Old servers omit the offset capability; the browser hides offset controls and omits the parameter. Non-finite/malformed offsets return 400; an unavailable shifted band or invalid selected frequency returns 422.

`iq_file_demodulate:true`, `iq_file_demodulate_modes:["am","usb","lsb","nfm"]` and `iq_file_demodulate_max_seconds:60` enable `GET /api/sdr/recordings/{task_id}/demodulate?segment=<id>&start=0&count=20000&mode=am&if_bw=1500`. Only finished IQ tasks or finalized verified failed prefixes are readable. The selection is segment-relative decimal integers, at most 1,048,576 complex samples / 60 seconds / 4096 blocks / 4 MiB provenance. Actual IQ rates must be 1000–192000 S/s. `if_bw` is 50–20000 Hz (USB/LSB minimum 101 Hz for the native carrier guard) and no greater than 0.45 times the actual IQ rate; the entire selected AM/NFM symmetric band or USB/LSB sideband must fit the original symmetric recorded passband. Unknown parameters/modes or malformed counts return 400; invalid source/filter/data 422, active 409, missing 404, shared reader busy 429.

The response is a bounded POSIX tar `iq-demod.tar` with `render.json`, `source.sigmf-meta` and `audio.wav`. Native `AnalogDemod::new` applies its IF/audio FIR and stateful linear resampling at the recorded actual rate, producing 48000 Hz mono IEEE Float32 WAV with fact, unity gain, AGC/NR/squelch off and the native mode's de-emphasis. This is a new offline render, not the old published PCM. Each request starts fresh causal filters at the selected sample, keeps the startup transient, emits only actual resampler output, and adds no pre-roll, tail zeros or cross-segment stitching. Audio sample index / 48000 is relative playback time; total demodulator delay is explicitly unknown. Original u64/fraction/delay clocks and trim offsets stay unchanged in the source metadata; no absolute UTC is invented. SHA-256 covers selected CF32, metadata and generated PCM. This metadata file describes the selected original IQ; the tar does not contain an IQ data member. Original files and identities remain unchanged.

Processing runs outside acquisition locks and allocates no live receiver slot. One checked source block, short 8192-sample DSP turns, at most 11,520,008 PCM bytes (plus bounded WAV construction), 4 MiB source metadata and 2 MiB manifest are allowed. The existing IQ/results/audio single reader lease covers preparation, producer and HTTP body; cancellation and two 64 KiB / 2-second transport bounds also apply. The complete derived snapshot is validated before success and emitted from memory, so later source changes cannot change its bytes. The browser retains one bounded tar/player (20 MiB envelope), cancels superseded requests, clears audio on edits/hide/task replacement and downloads the same render with provenance. Old services hide file DSP. Digital file decoding and continuous long-file playback are separate future work.

`audio_flac_export:true` enables `GET /api/sdr/recordings/{task_id}/audio/flac` without query parameters for finished audio tasks and verified failed prefixes. The native `audio-flac-s24.tar` download contains `task.json`, unchanged provenance `frames.jsonl`, and one `.flac` per nonempty source segment. The source is the same published PCM, including shared backend processing and excluding browser speaker controls. Convert each finite Float32 channel value with `q = clamp(round_away_from_zero(x * 8388608), -8388608, 8388607)` to signed 24-bit PCM. No dither, normalization, gain adjustment, resampling or gap filling occurs. The manifest declares the conversion and per-segment `clipped_values` (rounded interleaved values outside the integer range), sample counts and encoded lengths as decimal strings. Positive 1.0 saturates; negative -1.0 is exactly representable. FLAC preserves the selected integer PCM, not the original Float32 bits; the original journal and Float32 WAV endpoint remain available. Actual sample rates, channels, configurations and segment boundaries remain unchanged; no protocol-input source or absolute UTC is implied.

`recording_flac.rs` uses the pure Rust `flacenc` 0.5.1 encoder with parallel features disabled, fixed predictors/Rice coding, 4096-sample fixed blocks and an unpadded final short frame. STREAMINFO contains exact sample counts and the MD5 of packed signed-24 LE PCM as specified by [RFC 9639](https://www.rfc-editor.org/rfc/rfc9639.html#name-streaminfo). One checked journal record, one encoder frame and bounded summaries are retained; the combined manifest/plan is at most 2 MiB with at most 4096 segments. Preflight encodes to a sink to calculate fixed tar lengths, STREAMINFO and statistics. Emission re-encodes deterministically and compares SHA-512 source/frame digests before the tar terminator. Changed sources yield an incomplete response. No intermediate audio files are written. The existing single reader lease, producer/body cancellation and two 64 KiB/2-second pipe are shared with WAV/results/IQ; the same 400/404/409/422/429 semantics apply. Empty finished tasks export metadata and an empty timeline.

Native backend (`backend/`) external interfaces: **HTTP REST** + **WebSocket** (JSON commands/status + binary trace frames). Root `./run.sh` defaults to hardware on port 8080; Python clients are optional tools.

- Service: `http://127.0.0.1:8080` (WebSocket at `/ws`)
- Frontend: modern UI at `/`, static assets `/static/dist/{file}`
- Loopback is the default. Set `WEBSA_HOST=0.0.0.0` for LAN access; product endpoints
  do not require authentication. WebSockets require same-origin access or an origin
  listed in `WEBSA_ALLOWED_ORIGINS`.

---

Capabilities publish `max_channels:8` and `max_recording_tasks:8`. `recording_resources` reports `active_tasks`, `max_tasks`, `available_tasks`, `queue_bytes_per_task`, `queue_items_per_task` and `max_queue_age_ms`. Preparing and finalizing tasks retain their slot until reaped. IQ, published audio and results share the eight-task limit; recording all three kinds on one receiver uses three slots. Receiver definitions and recording tasks have separate limits.

Each receiver's `processing_statistics` contains bounded history keyed by receiver UID, service ID, processing generation and stream identity. `packets` and `searches` distinguish accepted, started, succeeded, failed, cancelled, rejected and in-flight work; only successful input contributes to media seconds. `occupancy` includes queued, executing, pending output and search return references with byte/sample counts and monotonic ages. `wall_ns`, available `thread_cpu_ns`, and queue wait are separate. Watermarks describe source blocks; `service_consumed` means drained by the service, not played by a remote speaker. `source_watermarks` connects shared input/PFB observations and fine channel clocks; observations need not be simultaneous. Model requested/effective state, squelch and protocol window retention are reported independently. Missing upstream decoder consumption is null. UTC changes, closed squelch and protocol window waiting do not imply missing source samples.

P5 IQ recording uses the same receiver API: `record_start` (`receiver_uid`, `kinds:["iq"]`) and `record_stop` (`task_id`), with `request_id` and current `service_id`. Neither receiver configuration preflight nor a browser IQ socket is required. Actual storage availability enables `recording:true`, `recording_kinds:["iq","results","audio"]` and the commands; unavailable storage omits the operations and supplies `recording_unavailable_reason`. Snapshot `recordings[]` retains original service identity, tracks, state, saved bytes and errors; refresh/reconnect reads this actual state. `GET /api/sdr/recordings/{task_id}/index?volume=iq-000000.jsonl&offset=0` returns up to 128 persisted block indexes and a decimal `next_offset`, not sample download or SigMF.

Startup loads completed IQ, audio and result tasks from their durable manifests and checks volume names, sizes and IQ index existence without decompressing payloads or rewriting indexes. Existing indexes are read lazily; range reads and exports verify content before success. Interrupted/failed tasks and completed tasks with missing or inconsistent storage run recovery. Index queries require a finished task (409 while active, 404 after deletion), share the archive reader lease with deletion, and return `Cache-Control: no-store`. Deletion removes the task from current history and syncs its hidden tombstone before success; leftover tombstones are excluded from startup.

IQ, audio and result workers coalesce queued packets for at most 50 ms into batches of up to 32 inputs / about 1 MiB (one larger bounded input may run alone). Data, the IQ sidecar index where present, and the manifest are synced before publishing the batch counters. The original 4 MiB / 64-input / 2-second admission budget includes every queued and in-flight packet; batch waiting does not reset its oldest-input deadline. Blocks, source clocks, segment boundaries and file format remain unchanged. Stop drains and syncs the final batch.

Finished-task IQ reads are advertised by `iq_archive_read:true` and `iq_range_max_samples:"1048576"`. `GET /api/sdr/recordings/{task_id}/range?segment=<segment_id>&start=0&count=4096&format=sigmf` selects a zero-based range relative to one segment. `start`/`count` are decimal integers; the count is 1–1,048,576 (8 MiB CF32), at most 4096 blocks and 4 MiB of provenance metadata. `format=cf32` returns checked raw samples, `format=meta` returns SigMF metadata, and the default `sigmf` returns a POSIX tar `.sigmf` archive containing a matching `.sigmf-data` / `.sigmf-meta` pair. Every selected block is decompressed and checksum-verified before success. Reads span volume boundaries but never join different segments/rates. Responses use no-store. Active/finalizing tasks return 409, unknown tasks 404, malformed ranges 400, invalid/missing/corrupt data 422 and a concurrent reader 429. One blocking reader owns filesystem/decompression work outside acquisition state; its permit remains held through response delivery, and aborting the response releases it. Failed recovered tasks expose only their verified prefix; original failure and service/UID remain in metadata.

Streaming export is advertised by `iq_archive_export:true` and decimal `iq_export_max_samples:"1073741823"`. `GET /api/sdr/recordings/{task_id}/export?segment=<segment_id>&start=0&count=4194304` returns a standard SigMF archive from a finished task. The selected CF32 member is smaller than 8 GiB (POSIX ustar size bound); metadata has the same individual-file bound. The existing `/range` waveform/small-read limits remain unchanged. Before HTTP success, the service checks the full selected range. It then revalidates while streaming data and annotations, retaining one checked block and a two-item 64 KiB transport pipe. A changed archive fails the response rather than completing a file with different bytes. The reader permit covers preparation, producer work and response lifetime; closing the response cancels work and releases it. Two seconds of transport queue backlog terminates that export producer; reception and recording continue. The browser uses a native download instead of buffering the export as a Blob. Each segment remains a separate Recording; use `/collection` to download them together.

Services with persistent recording also publish `state_revision`, a decimal u64 that increases within a service generation independently of receiver configuration `revision`. The browser rejects delayed older snapshots so task progress and terminal states cannot regress when configuration is unchanged.

`GET /api/sdr/recordings/{task_id}/collection` downloads all nonempty verified segments of one finished IQ task in a single POSIX tar `.sigmf` archive. It accepts no query parameters. Capability `iq_archive_collection:true` publishes decimal `iq_collection_max_samples:"1073741823"` (the total across the task) and integer `iq_collection_max_segments:4096`. Each segment becomes a distinct, zero-based `segment-000000.sigmf-data` / `.sigmf-meta` pair at the archive root, preserving its actual rate, frequency, precise clocks and original task/service/UID/fault. `recording.sigmf-collection` contains ordered `core:streams` recording objects with generated basenames and SHA-512 hashes of the exact metadata files. No cross-segment continuity, resampling or absolute time alignment is implied. Empty tasks, exceeded limits, gaps or corruption in any segment reject the entire request with 422 before success; active/finalizing tasks return 409, unknown tasks 404, query parameters 400 and reader contention 429. The collection shares the single `/range` and `/export` lease, cancellation and two-item 64 KiB transport budget. Preparation retains bounded per-segment plans (at most 2 MiB of prefixes/IDs plus a 2 MiB collection manifest); samples and annotations stream. Later source changes terminate an incomplete response. The browser's All segments action uses native downloads; the waveform remains bounded.

`record_start` also accepts `kinds:["results"]` for a healthy enabled processor whose descriptor publishes text, table, image or status results. A task currently has one kind; IQ and results may run as separate tasks on the same UID, sharing eight service-wide task slots. `results_archive_export:true` enables `GET /api/sdr/recordings/{task_id}/results` (no query parameters). This streams POSIX tar `results.tar` with `task.json` and `events.jsonl`. Every JSONL row contains `version:1`, host `received_at_ms` (decimal milliseconds), original baseband `source` metadata and a complete `envelope` with original SDE2 processing sequence/clock, service/UID and `baseband_stream`. Source sample positions/fractions remain decimal strings. Receipt time does not establish absolute sample UTC. The manifest includes task failure, prefix metadata and the explicit policy of capturing only events published after recording starts. Images retain original incremental RGB/Gray8/base64 PNG payloads and completeness metadata; missing earlier rows remain partial. The backend does not reconstruct standalone PNGs or include browser history.

Result journals use internal WBRS0001: independently Zstd-level-1-compressed JSON records with raw fallback, a 16-byte kind/raw-size/stored-size/CRC32 header, and a checked count/byte/digest footer per volume. Each record is at most 3 MiB; the common 4 MiB/64-item/2-second queue includes in-flight work. Compression, sync, recovery and downloads stay outside acquisition state. Quiet decoders do not time out; processor removal drains and ends results, processing failure fails results while healthy IQ recording continues. Processing identities, rather than IQ identities, delimit result segments; late old events retain their original segment. For interrupted tasks, startup recovers only the checked prefix without resuming or rebinding. Download preflight checks every retained record and footer, then repeats verification while streaming; later changes cause an incomplete response. It shares the single IQ reader lease and two 64 KiB/2-second transport pipe. Status codes are 400 for queries, 404 unknown, 409 active/finalizing, 422 wrong kind/data corruption and 429 contention. An empty finished result task is a valid empty JSONL export. Protocol-input audio and digital file decoding remain unavailable; Opus export uses the separate audio endpoint described above.

`record_start` now accepts `kinds:["audio"]`, admitted by the descriptor's actual `audio` output, including FT8 companions and DRM/DRM+ codec audio regardless of `purpose`. The source is published AUD2 Float32 PCM (`source:"published_audio"`, `format:"f32_le"`), including configured backend gain/AGC/NR/squelch or codec output. Browser volume/pan/mute/Solo are excluded. This is not protocol-input audio. Opus encoding is provided by the separate export endpoint described above. IQ, results and audio on one UID occupy three of the eight service-wide task slots; each task retains its 4 MiB/64-item/2-second budget. Silent codecs remain waiting without the IQ five-second timeout. Preparation syncs the manifest and first volume before acknowledgement; stop drains, processor removal ends normally, and processing failures leave healthy IQ running.

Internal WBAU0001 stores actual configuration JSON and original AUD2 headers/PCM using the common independent Zstd1/raw/CRC32 journal codec. Volumes roll at 256 MiB or 4096 frames, with at most 4096 volumes/segments, 16 KiB source JSON per frame and 2 MiB segment summaries. Processing/audio-format starts, source configuration changes and RIFF size bounds create separate segments; gaps are never joined or filled with silence. Packet-owned configuration follows queued old frames. Interrupted tasks recover verified prefixes under original UID/service/fault without automatic resume.

`audio_archive_export:true` enables `GET /api/sdr/recordings/{task_id}/audio` without query parameters. Streamed `audio.tar` contains `task.json`, per-frame provenance `frames.jsonl` and IEEE Float32 LE WAV files (including fact sample counts) for each nonempty segment. Original sample bits are preserved without resampling/quantization. Each WAV is smaller than 4 GiB; long continuous sources split before that limit. JSONL retains original 96-byte AUD2 header hex, exact source indices/fractions/delay/rates/audio positions and configurations; no absolute UTC or cross-segment alignment is implied. Full preflight and SHA-512 emission verification share the IQ/results reader lease, producer/body lifetime and two 64 KiB/2-second transport pipe; changed sources produce incomplete responses. The same 400/404/409/422/429 semantics apply, and empty finished tasks export an empty timeline. WAV follows the [Microsoft RIFF/WAVE specification](https://www.mmsp.ece.mcgill.ca/Documents/AudioFormats/WAVE/Docs/riffmci.pdf).


## Single-channel receiver presets

`capabilities.receiver_presets:true` enables `GET /api/sdr/receiver-presets`, returning `{version:1, revision:"0", presets:[]}`. Entries contain `{name, settings:{purpose, baseband, processors}}`. Limits are 32 entries, 256 KiB per file and 1–128 bytes per nonblank name. Storage is `WEBSA_RECORDING_DIR/receiver-presets.json` under the instance's existing exclusive directory lock. A corrupt file is preserved and disables the capability instead of being reset.

POST to the same path to save: `{type:"save", service_id, expected_revision, name, receiver_uid, receiver_revision}`; delete: `{type:"delete", service_id, expected_revision, name}`. Revisions are decimal u64 strings. Saving extracts only the specified receiver's confirmed revision, resolves omitted AGC/gain fields to the source's actual settings, accepts no arbitrary candidate settings, and replaces the same name. List/receiver revision conflicts or an old service return 409; invalid input 400, missing item 404, unavailable storage 503, write failure 500. Success returns the new list after syncing and atomically replacing the file. Disk I/O holds no acquisition lock. Clients explicitly refresh after conflict and do not automatically retry writes.

Loading merges saved settings into a target channel and uses existing `configure` admission and explicit confirmation. Target name, color, tags and reception enabled state remain; UID, source clocks, tasks and local monitoring are excluded. A receiver revision/service change after preview rejects confirmation and requires another preview. Out-of-band frequencies and unavailable modes still fail real admission. Presets do not move shared capture or apply automatically.

`sdr.actual.ddc_batch` reports the current number of physical IQS packets per PFB/fine job: 4 for 62.5 MSps hardware capture without a direct raw-IQ route, otherwise 1. Each physical packet is still checked for continuity; batching does not change output clocks or frame formats.

`sdr.actual.fine_owners.channels` reports each independent fine/direct owner with its stream identity, accepted/started/succeeded/failed/cancelled/rejected counts, retained bytes (including executing references), queue wait, wall time and available thread CPU time. `fine_faults` identifies failed source branches and their last completed / first unavailable input boundaries. A fine-source fault stops IQ, processors and recordings for that identity; other channels continue. Explicit `recover:true` creates a new IQ identity even with unchanged settings. Local reconfiguration waits up to 100 ms for its target to finish accepted input, collecting ready output from all lanes before closing the target queue. A busy rejection keeps its original owner and queue usable without creating a source fault or replacement owner. Input handoffs coalesce up to eight blocks or four milliseconds, with partial batches flushed at control/stop boundaries. Bounds remain 64 queued blocks plus one executing block and 4 MiB per lane. Fine output (32 batches / 8 MiB) and publisher delivery (32 frames / 8 MiB per identity) are separate bounds; neither waits for downstream capacity. Display replacement is permitted; IQ gaps stop the affected source explicitly. Fine/framer aggregate wall time sums independent owners and is **not** single-thread utilization.


## 1. HTTP Endpoints

### Native multichannel receivers

`GET/POST /api/sdr/receivers`, `POST /api/sdr/receivers/preflight` and
`/ws/iq?receiver_uid=...` implement the P3 receiver/IQ slice. See
[MULTICHANNEL_DRAFT](MULTICHANNEL_DRAFT.md#native-p3-receiver-api-and-iq-subscriptions) for commands, clocks, boundaries and limits.
P4 adds `/ws/audio?receiver_uid=...` and `/ws/decode?receiver_uid=...&ack=1`, mode/provider descriptors and local browser mixing.
Processing streams have independent identities and retain `baseband_stream` attribution in UID-scoped SDE2.
See [P4 semantics](MULTICHANNEL_DRAFT.md#p4-channel-monitoring-and-existing-decoders).
Legacy IQBF/IQAX, AUD2, SDE2 and `/api/config` remain compatible.

### `GET /api/sdr/plugins`

Returns `{plugins, sstv_modes}`. `plugins` retains the `analog`, `digital`, `audio` and `ddc`
kinds; only the first two are receiver modes. Mode entries expose `id`, `label`, `category`,
`description`, `aliases`, `provider`, `implemented`, `available`, nullable `unavailableReason`,
`audioEnhancement`, `common`, `outputs`, nullable `audioCompanion`, `sideband`, nullable
`inputRateHz`, `minimumRateHz`, `defaultBandwidthHz`, `minBandwidthHz`, `maxBandwidthHz`,
`bandwidthPresetsHz`, `bandwidthEditable` and required `startupTimeoutMs`. Stage entries keep their original minimal fields.
Bandwidth values are Hz in the mode's control convention; an upper-sideband high cutoff is
not a full two-sided RF width. STATUS filter limits and backend admission can be tighter.
Availability describes the installed provider and implemented interface, not reception lock.
The service caches dependency discovery until restart; browser loading publishes the complete
catalog and SSTV metadata atomically. Missing or failed providers carry explicit reasons.
`startupTimeoutMs` is the receiver preparation acknowledgement deadline: native modes
3000 ms, stdin/RTTY companions 5000 ms, OnAir 15000 ms and GNSS-SDR 25000 ms.
It is not a signal-acquisition or position-fix deadline and does not trigger retries.

### `GET /api/state`

Returns the last published full device state (JSON, same shape as WS `STATUS`). To read terminal capture diagnostics during a failure, send `{"cmd":"STATUS"}` to `POST /api/config` for the currently retained diagnostic instead of relying on the last healthy publication.

```bash
curl http://localhost:8080/api/state
```

Example response:

```json
{
  "cmd": "STATUS",
  "connected": true,
  "device": "SAN-90",
  "device_detail": { "uid": "5230500C00380025", "model": 67, "hw": 8, "mfw": 14184, "ffw": 14184, "bus_speed": 12, "bus_ver": 1, "api_ver": "0.55.89", "warnings": "" },
  "center": 1000000000.0, "span": 600000000.0, "ref": 0.0,
  "rbw_mode": "auto", "rbw": 100000.0, "vbw_mode": "bypass", "vbw": 1000000.0,
  "points": 1000, "window": 1, "spur": "standard", "mode": "std", "pnm_supported": true,
  "caps": { "model": 67, "name": "SAN-90", "fmin": 9000, "fmax": 9000000000,
            "ref_min": -50, "ref_max": 30, "rta_span_max": 50781250, "rta_points": 3328 },
  "preset_defaults": { "center": 1000000000, "span": 100000000, "rbw": 100000, "points": 1000, "ref": 0, "atten": -1 },
  "req": { "center": 1000000000, "span": 600000000, "points": 1000, "rbw_mode": "auto", "rbw": 100000, "vbw_mode": "bypass", "vbw": 1000000, "ref": 0, "spur": "standard" },
  "actual": { "center": 1000000000, "span": 600000000, "points": 985, "rbw": 100000, "vbw": 1000000 },
  "amp": { "atten": -1, "preamp": 0, "ifgain": 2, "gain_strategy": 0, "atten_actual": 30, "preamp_actual": 1, "ifgain_actual": 2 },
  "ref_clock": "internal", "has_docxo": false, "refclk_ppm": 0.0, "calibrating": false,
  "refclk_out": false, "last_cal_freq": 0.0, "gnss": { "lock": false, "sats": 0, "docxo": 0, "docxo_mode": -1, "antenna": -1, "latitude": 0.0, "longitude": 0.0, "altitude": 0, "time": "0000-00-00 00:00:00" },
  "last_error": ""
}
```

Field reference:

| Field | Type | Description |
|---|---|---|
| `connected` | bool | device connected |
| `device` / `device_detail` | str / obj | device name + details (uid/model/hw/mfw/ffw/bus/api/warnings) |
| `center` / `span` / `ref` | number | effective values for the active hardware mode |
| `ref_mode` | str | always `manual`: Auto Scale is a one-shot action, not a tracking mode |
| `rbw_mode` / `rbw` | str / number | RBW mode (manual/auto), resolution bandwidth |
| `vbw_mode` / `vbw` | str / number | VBW mode (bypass/equal/tenth/manual), video bandwidth |
| `points` | int | requested SWP trace points; the actual returned native count is in each FREQ/POWR frame |
| `window` | int | FFT window: 0=FlatTop 1=Blackman-Nuttall 2=LowSideLobe 3=Rectangle 4=Kaiser |
| `spur` | str | spur rejection (bypass/standard/enhanced) |
| `detector` | str | trace detector (auto/sample/pos_peak/neg_peak/rms/auto_peak), SWP only |
| `frame_detector` | str | Frame Avg request; native Preset restores `device_default`, leaving the inter-frame detector to the SDK profile default |
| `mode` | str | measurement mode (std/harmonic/pnm/rta/sdr/server) |
| `caps` | obj | model capabilities: `model`/`name`/`fmin`/`fmax` plus the numeric limits the client must not hard-code - `ref_min`/`ref_max` (dBm, the **auto-placement** row, not a limit on what a user may ask for - see FAQ_NOTES), `rta_span_max` (Hz), `rta_points` |
| `preset_defaults` | obj | device default config (used by Preset) |
| `req` / `actual` | obj | active request/actual values; `req.swp`, `req.rta` and `req.sdr` retain mode-private settings |
| `swp_actual` / `rta_actual` | obj | latest SDK effective settings for each spectrum mode |
| `sweep_ms` / `sweep_ms_meas` | number | SDK-estimated minimum SWP time (ms, in FREQ/POWR headers; 0 if SDK has no estimate) / publisher's measured acquisition/publish step duration EMA (ms, STATUS only). SWT prefers the estimate; when it is 0, SWT displays the EMA marked `measured` rather than leaving the field blank or mixing sources. |
| `sdr` | obj | Requested tuning/demod/audio settings, `actual` capture/display/DDC geometry, `level_dbfs`, squelch, vendor ADM metrics and native stream health. `sdr.level_dbfs` is channel 1's meter; each `sdr.channels[].level_dbfs` measures its own normalized complex IQ before AGC/audio gain: `10*log10(mean(I²+Q²))`, with wire full scale 1. It is null for no finite measurement (including zero power), inactive/disabled channels, changed stream identity/tuning, outside SDR mode, or input older than 1500 ms. Each channel's `channel_dbm` reports its fresh SDK tuned-bin peak; `squelch_open` reports the processed analog audio gate, and is null before processing, for stale/inactive streams or for DRM codec audio that bypasses the gate. Top-level `sdr.channel_dbm` and `sdr.squelch_open` mirror channel 1. Requested display bandwidth may be narrower than device capture bandwidth. |
| `sdr.filter_limits` | obj | Current mode's dynamic `ifbw` geometry bounds: `mode`, `min_hz`, `max_hz`. `min_hz` is the real DSP floor — 50 for AM/DSB/CW/NFM/WFM/PM and 101 for USB/LSB; protocol-locked or unknown modes report `min_hz:0, max_hz:null`. `max_hz` is an upper bound in whole Hz for the current capture center/bandwidth, input rate, PFB taps, listen frequency and CW pitch; it is null outside SDR, with channel 1 disabled, or when no feasible range exists. It is not a mode recommendation and other geometry, provider and capacity checks still apply. |
| `sdr.receiver_error` | string or null | Shared receiver worker diagnostic; null when no receiver failure is active. A failure stops all audio/decode receivers while capture, raw IQ and display streams continue. It is separate from global `last_error` and `ddc_error`. |
| `sdr_last_failure` | object or null | Latest terminal capture diagnostic, retained after recovery. Includes `error` and the failure-time `actual`; pipeline errors also include source position, sequence and capture settings. `actual.iqs_capture` and bounded `actual.iqs_events` preserve failed reads, sustained stalls and queue statistics, which the last healthy snapshot cannot establish. Null until a failure has been recorded. |
| `rta_actual.frame_points` | int | RTA device FFT frame width; RTA2 delivers returned bins independently of SWP points |
| `config_version` / `response_to` / `request_id` | int / str / scalar | successful reconfiguration sequence; command name and optional scalar request identity echoed on command STATUS/ERROR; periodic publications omit response identity |
| `auto_ref` | obj | bounded Auto session state: `adjusting`; `pending`, the hardware or display target awaiting fresh confirmation; `hardware_adjusting`, which distinguishes SDR capture recovery/audio transition and remains false for display-only fitting; `seq`, `last_peak`, `last_noise_floor`, `result`, and successful final `target` (otherwise null). See Auto Scale below. |
| `rta_health` | obj | current consecutive RTA errors and in-place recovery attempts |
| `amp` | obj | shared requested `atten`/`preamp`/`ifgain`/`gain_strategy`, plus active-mode SDK-configured `atten_actual`/`preamp_actual`/`ifgain_actual`; unavailable actuals are null, never copied from requests. Preamp 0 means configured Auto and 1 Off, not physical On/off telemetry. |
| `ref_clock` | str | reference clock source: internal/external/premium/external_forced |
| `has_docxo` / `has_premium_ref_clock` | bool | Installed DOCXO hardware / available premium internal source. Derived from hardware options, independent of GNSS lock or work mode |
| `refclk_ppm` / `calibrating` / `last_cal_freq` | - | GNSS calibration state |
| `refclk_out` | bool | reference clock output enable |
| `gnss` | obj | GNSS state: `lock`(0/1; null or missing means unavailable, only explicit 0 means unlocked) `sats` `docxo`(0/1) `docxo_mode`(0=disciplined,1=hold) `antenna`(0=external,1=internal) `latitude`/`longitude`(deg) `altitude`(m) `time`(UTC, "0000-00-00 00:00:00" when invalid). Ordinary mode transitions and query failures retain the last telemetry with `stale:true` (Updating) until a successful fresh query clears it. Null in Server; this mode has no device or client telemetry |
| `server` | obj | Always present, including when the status fixture has no `server` key. See Soapy Server below. Outside that mode the process stays stopped |
| `last_error` | str | recent global error message; receiver worker failures use `sdr.receiver_error` |

Complete parameter ownership, defaults, and mode transitions are documented in
[`MODE_STATE_FLOW.md`](MODE_STATE_FLOW.md).

### `POST /api/config`

Send a command via REST (equivalent to WS commands, see table below). Response is the full STATUS.

```bash
curl -X POST http://localhost:8080/api/config \
  -H 'Content-Type: application/json' \
  -d '{"cmd": "SET_FREQ", "center": 1090000000, "span": 600000000}'
```

### `GET /`

Returns the frontend page (modern UI `dist/index.html`).

### `GET /static/dist/{file}`

Frontend static assets (JS/CSS/fonts).

---

## 2. WebSocket Protocol

### Connect

```
ws://localhost:8080/ws
```

After connecting, the service pushes status and the active acquisition mode's trace frames (FREQ/POWR or RTAF/RTA2). Server mode pushes status without Web acquisition frames.

### 2.1 Client → Server (commands)

JSON object: `{"cmd": "<COMMAND>", ...}`

| Command | Params | Description |
|---|---|---|
| `STATUS` | - | request one full status (STATUS reply) |
| `CONNECT` | - | connect device (if not connected) |
| `SET_PRESET` | - | restore device default config (Preset) |
| `CAL_REFCLK` | `count?` | GNSS 1PPS reference clock calibration (background; `calibrating=true` meanwhile) |
| `SET_FREQ` | `center`,`span` or `start`,`stop` | atomically set the SWP frequency window |
| `SET_REF` | `mode` (manual/auto), `ref?`, `range_db?` | active-mode reference; manual requires `ref` in -160..40 dBm. `mode=auto` starts one Auto Scale session |
| `AUTO_SCALE` | `range_db?` (5..200), `current_ref?`, `display_only?` | bounded reference placement/recovery; see below. `current_ref` excludes external Offset; the browser subtracts Offset from its displayed Ref |
| `SET_RBW` | `mode?` (manual/auto/one_thousandth_span/one_percent_span), `rbw?` | SWP resolution bandwidth; RTA uses its supported manual/auto choices |
| `SET_VBW` | `mode?` (manual/equal/tenth/onethousandth/bypass), `vbw?` | set video bandwidth |
| `SET_SWEEP` | `mode?` (0..8), `time?` (schema: 0..1000) | 0=minSWT, 1=x2, 2=x4, 3=x10, 4=x20, 5=x50, 6=xN, 7=Manual, 8=minSMPxN. `time` is seconds for Manual and a multiplier for xN; mode-specific handler validation also applies. RTA updates its own profile |
| `SET_POINTS` | `points` (integer 51..16000) | requested SWP points; RTA points are SDK-owned |
| `SET_SPUR` | `mode` (bypass/standard/enhanced) | spur rejection mode |
| `SET_WINDOW` | `window` (0~4) | FFT window |
| `SET_DETECTOR` | `mode` (auto/sample/pos_peak/neg_peak/rms/auto_peak) | trace detector, SWP only |
| `SET_AMP` | `atten?` (-1~33), `preamp?` (0/1), `ifgain?` (0~3), `gain_strategy?` (0/1), `request_id?` | update supplied gain fields; STATUS/ERROR echoes request_id. Requested fields and SDK actual readbacks remain separate |
| `SET_REFCK` | `mode` (internal/external/premium/external_forced) | requested reference source; reconfigures the active profile. Unavailable premium hardware is rejected before mutation. `actual.refclk_src` reports the SDK-resolved source, including automatic external fallback |
| `SET_REFCKOUT` | `on` (bool) | reference clock output; reconfigures the active profile |
| `SET_MODE` | `mode` (std/harmonic/pnm/rta/sdr/server) | switch measurement mode (session). `server` is exclusive and does not start SoapySDRServer |
| `SET_RTA` | `center?`, `span?` | atomically set RTA center and 2^n analysis span |
| `SET_SDR` | `center?`, `listen?` (Hz), `decimate?` (power of two, 1~`caps.decimate_max`; rounded to the nearest power of two), `pfb_taps_per_phase?` (native backend: 4/8/16, default 16) | set the SDR capture window; the native backend rebuilds the channelizer for the selected PFB taps per phase. 16 taps is the default rejection profile; 8/4 are measured lower adjacent/alias-rejection profiles. Panning the capture drags the primary listen frequency with it; when that leaves a band-limited OnAir decoder's band, the channel demotes to the decoder's audio companion instead of committing an invalid pair With `listen`, capture and C1 tuning are admitted atomically and C1 is enabled, including after all receivers stop; omitting it retains axis-pan behavior and receiver enable states. |
| `SET_SDR_FFT` | `size`: 0 / 32768 / 65536 | Display analysis only; 0 retains the original SDK 16240-sample path. Requires live IQ and an updated shim. `sdr.actual.display_fft` reports applied `size`, `requested_size`, warming/active state, real input length, output points, Δf, ENBW, observation duration and failures. No receiver/capture/recording reconfiguration. |
| `SET_SDR_TUNE` | `listen` (Hz) | tune the primary receiver. On the native backend, tuning within the capture updates the channel plan; an enabled primary tuned beyond the capture moves the shared capture to the requested frequency. All defined channel configurations are preserved. Enabled auxiliary channels must fit the replacement capture; auxiliary tuning never moves the capture. Device limits, occupied bandwidth, coarse passband and admission failures are reported explicitly |
| `SET_SDR_DEMOD` | `mode?`, `deemph_us?` (-1=auto 0=off ..1000), `ifbw?` (positive finite Hz), `squelch?` (-200..0 nominal SDK dBm tuned-bin peak, before audio AGC; <=-160 selects Off), `volume?` (0~2), `pitch?` (CW 100..1600 Hz; other modes (0,20000] Hz), `agc?` (bool), `am_sync?` (bool, default false) | primary receiver settings. The native backend processes all 13 modes: am/dsb/usb/lsb/cw/nfm/wfm/pm/ft8/sstv/radiofax/drm/drmplus. It emits AUD2 PCM and SDE2 events. The browser controls playback and displays results. `ifbw` requests a receiver filter change and reconfigures the channelizer when admitted |
| `SET_SDR_CHANNEL` | `channel?` (1–8, default 1), `enabled?`, `freq?`/`listen?`, `mode?`/`demod?`, `ifbw?`/`if_bw?`, `squelch?`, `volume?`, `agc?`, `pitch?`, `deemph_us?`/`deemph?`, `nr?`, `nr_strength?`, `nr_algorithm?`, `nr_atten_db?`, `stereo?`, `am_sync?`, `audio_rate?`, `sstv_mode?`, `radiofax_acquisition?`, `radiofax_ioc?`, `radiofax_lpm?`, `radiofax_modulation?`, `radiofax_polarity?`, `radiofax_clock_recovery?`, `radiofax_phase_pixels?`, `radiofax_clock_ppm?`, `radiofax_new_page?`, `radiofax_reset_correction?` | partially update one receiver; omitted fields retain their values. Conflicting aliases are rejected. Radiofax acquisition is auto/manual, IOC auto/288/576, LPM auto/60/90/120/180/240, modulation fm/am, polarity normal/inverted and clock recovery auto/off. Phase is ±4096 px and clock correction ±5000 ppm. `radiofax_new_page` closes the active page as partial; `radiofax_reset_correction` sets both corrections to zero. Protocol changes rebuild the decoder; phase/ppm changes do not rebuild RF. Band-limited OnAir decoders (AIS, ADS-B, Meteor LRPT, APRS, FT4, WSPR) reconcile the retained frequency: selecting the decoder without an explicit `freq` retunes to its canonical center (ADS-B 1090 MHz, AIS 162 MHz, Meteor 137.1 MHz), and a `freq`-only change leaving the band demotes the channel to the decoder's audio companion (AM/NFM/USB) with that mode's default bandwidth. An explicit `mode`+`freq` conflict is rejected. |
| `SET_HARM` | `f0`, `count`, `span` | harmonic params (fundamental Hz, orders, span per harmonic) |
| `SET_PNM` | `center`, `threshold`, `traceavg`, `start`, `stop` | phase noise params |
| `SET_TRIGGER` | `source` (bus/freerun/level/external/timer), `edge` (rising/falling/double), `level` (dBm, `caps` bounds), `safetime`/`delay`/`pretime` (0~10 s), `acqtime` (0.0005~60 s), `retrigger` (0-65535), `retriggerperiod` (s), `out` (none/per_hop/per_sweep/per_profile), `outpolarity` (positive/negative) | RTA acquisition trigger (written into the RTA profile): reconfigured immediately while in RTA, otherwise applied on the next entry. The swept-mode level trigger runs in the frontend and does not use this command |
| `SET_SERVER` | `action` (start/stop/restart/configure), `bind_address?` (IP literal), `port?` (integer 1..65535) | SoapySDRServer control; requires `mode=server`, otherwise `server_mode_required`. Configure requires a stopped child; starting an existing child with a different bind returns `server_busy`. Restart stops the child and can apply a new bind. Start/restart recheck dependencies and the free SDK owner slot. Optional scalar `request_id` is echoed as on other commands. In Server, hardware commands other than `STATUS`, `SET_MODE` and `SET_SERVER` are rejected with “leave server mode before changing hardware settings”; HTTP/WS admission uses `bad_parameter`, while dispatch uses `server_mode_active` |

The squelch slider covers -160..0 in 0.1 dB steps; its minimum selects Off. Raising
the threshold requires a stronger signal to open audio. The gate opens at or above
the threshold and closes below threshold minus 3 dB after a 300 ms hold, with a
smoothed attack/release. Missing or expired RF measurements fail open. The RF
reference is the SDK spectrum's peak within one bin of the tuned frequency, not
integrated channel power or the IQ dBFS meter; absolute RF calibration accuracy
has not been independently established. Protocol decoding precedes this listener gate.

For native FM audio, `agc=false` uses fixed PCM gain. NFM deviation of 5 kHz
and WFM deviation of 75 kHz map to full scale before listener volume.
WFM waveform events use a separate 20 Hz source clock.

AM defaults to envelope detection; `am_sync:true` selects carrier-recovered synchronous detection. The preference is retained outside AM. An `am_sync`-only update crossfades polarity-aligned detectors without rebuilding capture, filters or resampling, changing stream identity, or resetting input/audio clocks. STATUS reports the confirmed value in `sdr.channels[].am_sync`, with primary mirrors `sdr.am_sync` and `req.sdr.am_sync`.

`sdr.channels[].am_sync_generation` is a server-owned read-only u32 (default 0), mirrored for the primary channel as `sdr.am_sync_generation` and `req.sdr.am_sync_generation`. It increments only when `am_sync` changes; duplicate values leave it unchanged and requests cannot set it. Preset resets it with the new stream identity.

Native WFM defaults to stereo interleaved L/R AUD2; explicit `stereo:false` selects mono.
DRM/DRM+ codec audio does not use analog AGC, NR, squelch or deemphasis; the UI
disables these controls and retains volume/playback. Disabled receiver slots accept
partial updates without a valid dormant frequency; enabling validates tuning.
SSTV `sstv_mode:null` selects automatic detection; a supported integer pins a mode.
Saved selection is replayed only after confirmed SDR entry. Display decimation,
capture `sdr.actual.iq_rate` and per-channel `actual_iq_rate` are distinct quantities.

Commands validate the complete candidate channel before changing state. CW pitch outside 100..1600 Hz returns `bad_parameter` without failing receivers. NR changes preserve decoder history and continuous `audio_index`; source position advances over discarded reducer tails. DRM/DRM+ store NR preferences with effective NR Off. Selected-model preparation or inference failure explicitly fails only that processing branch; effective NR is null on failed preparation, without Wiener/Off substitution. Explicit recovery retries the selected configuration.

`nr_algorithm` accepts `wiener`, `dfn` (DeepFilterNet3), `gtcrn`, `ul-unas` and `fastenhancer`. `nr_strength` (0–1) controls Wiener; `nr_atten_db` (0–100 dB) controls neural NR, with 0 dB returning original audio and higher values reducing the original-audio mix. Neural modes require 48 kHz listening PCM. Model versions, licenses, rate conversion and signal delays are recorded in [`backend/models/nr/README.txt`](../../backend/models/nr/README.txt).

NR, gain and AGC are independent per channel. Models are independently selectable without predicted CPU/output or algorithm-count limits. `processing_resources` reports payload geometry diagnostics; `processing_statistics`, `processing_runtime`, `host_resources` and `model_preparation` report observed execution, queues and model state. Actual per-UID overload interrupts that branch with a source gap and supports `recover:true`; other receivers and independent IQ continue.

`ifbw` is a mode-dependent request. For AM/DSB/NFM/WFM/PM it is the total bandwidth centered on the dial. For USB/LSB it is the outer high-frequency cutoff; the inner carrier guard is `clamp(0.1 × ifbw, 100, 300)` Hz, mirrored for LSB. CW is centered at `max(100, pitch)` Hz and extends by `±max(50, ifbw) / 2`. The browser editor and backend both enforce a 50 Hz minimum for other editable modes and 101 Hz for USB/LSB, preventing guard collapse. The browser also checks the current `sdr.filter_limits`; backend admission additionally applies the selected mode's envelope, capture and output sample rates, coarse passband and channel-capacity constraints. An invalid or inadmissible request is rejected rather than silently clamped. Presets are recommendations, not upper limits: custom values above common presets, such as AM 100 kHz, SSB 50 kHz and CW 10 kHz, are allowed when admitted. The input supports Hz/kHz/MHz; unit changes preserve the underlying Hz value, and the virtual keypad uses the same validation and commit path as typed input.

The browser fixes FT8 at `ifbw=3000` with a 100–3000 Hz decoder range, SSTV/radiofax at `ifbw=3000` with a 300–2700 Hz decoder range, selects and locks DRM at `ifbw=12000`, and fixes DRM+ at `ifbw=100000`. These are UI mode semantics, not wire-level restrictions: a direct API request may still use other values, subject to backend geometry and admission. DRM displays `±confirmed_ifbw/2` independently of the FAC-signalled occupancy, so it does not automatically receive every 4.5–20 kHz occupancy. DRM+'s 100 kHz UI value is the channel width, not a claim that its outermost OFDM carriers are exactly ±50 kHz.

Channel admission uses a symmetric occupied-bandwidth envelope around tuning. USB/LSB cover the sideband IF limit; CW includes pitch and IF; FT8 retains at least positive 3.2 kHz and SSTV positive 3 kHz. Integer IQ rates follow that envelope; exact protocol rates are adapted internally. WFM IQ also supports pilot/stereo/RDS, independently of PCM playback rate. A primary recapture validates all enabled channels first; rejection leaves the previous plan active.

> Config replies include `response_to=<command>` on STATUS and ERROR. A scalar
> `request_id` (string/number/bool/null) is echoed unchanged; object/array IDs are omitted.
> Periodic status and publisher errors carry no request identity. Rejected SWP/RTA
> reconfiguration preserves the prior request and applied profile when restoration succeeds.
> Invalid REST commands return HTTP 400; WS commands return `cmd:"ERROR"` with `msg`.
>
> Periodic STATUS `stream` metrics contain `clients`, `dropped_frames`, `dropped_control`,
> `dropped_audio` and `dropped_iq` for observing latest-wins drops and streaming-FIFO overruns
> caused by slow clients.

**Command examples:**

```js
// set frequency and switch to harmonic measurement
ws.send(JSON.stringify({ cmd: 'SET_FREQ', center: 1e9, span: 100e6 }));
ws.send(JSON.stringify({ cmd: 'SET_MODE', mode: 'harmonic' }));
ws.send(JSON.stringify({ cmd: 'SET_HARM', f0: 1e9, count: 5, span: 1e6 }));
```

### 2.2 Server → Client

#### JSON messages

| `cmd` | Trigger | Description |
|---|---|---|
| `STATUS` | on connect / after commands / **periodic (every 1 s)** | full status (fields in §1); periodic push keeps GNSS/time/lock states fresh without page refresh |
| `HARM` | during harmonic measurement | result: `{cmd:'HARM', list:[{n,f,amp,dBc,idx,inSpan}...]}` |
| `PNM` | during phase noise | result: `{cmd:'PNM', offset[], pn[], carrier_freq, carrier_power, progress, done}` |
| `ERROR` | device/command error | `{cmd:'ERROR', code, params?, msg, response_to?, request_id?}`; stable `code` supports localization, unknown codes fall back to `msg` |
| `RECEIVER_ERROR` | shared receiver worker failure | `{cmd:'RECEIVER_ERROR', error:string, failed:true, scope:'all_receivers'}`; retains the receiver diagnostic without a global spectrum error |

**PNM message fields:**

```json
{
  "cmd": "PNM",
  "carrier_freq": 999999900.0,   // carrier frequency (Hz)
  "carrier_power": -20.1,        // carrier power (dBm)
  "offset": [100, 1000, 10000, 100000, 1000000, 10000000],   // offset points (Hz)
  "pn": [-82.8, -95.5, -104.3, -117.1, -126.4, -132.0],      // phase noise (dBc/Hz)
  "progress": 25,                // incremental acquisition progress 0~100 (refreshes after first round)
  "done": false                  // first-round complete flag
}
```

> Unfilled segments of `pn` are -500 (invalid); the plot is complete when `offset` covers the full range and `pn` has no invalid points.

#### Binary trace frames

**FREQ/POWR — 16-byte header + data:**

```
offset  size  type    content
0       4     bytes   magic: 'FREQ' | 'POWR'
4       4     u32     version (FREQ/POWR pairing sequence)
8       4     u32     points
12      4     f32     sweep_ms (SDK-estimated minimum SWP time, ms; 0 if unavailable)
16      ...           data
```

| magic | data type | description |
|---|---|---|
| `FREQ` | float64 × points | frequency axis (Hz), sent only on version change |
| `POWR` | float32 × points | power trace (dBm), paired with same-version FREQ |

**RTA2 / RTAF — 24-byte base header:**

```
magic(4) + ver(u32) + points(u32) + wfLen(u16) + maxDensity(u16) + startHz(f64)
+ float64[points] + float32[points] + uint16[wfLen] + stopHz(f64)
```

RTA emits `RTA2`: after the base payload, append ASCII `TRIG`, a little-endian
u32 UTF-8 JSON byte length, then the trigger metadata JSON described below.
SDR panadapter emits legacy `RTAF` without the trailer. The browser can read old
RTAF recordings, but those frames cannot establish hardware capture completion.
Refresh older clients for RTA2 support. `points` is the display trace length.

#### 2.3 Native receiver subscriptions

| Endpoint | Binary output |
|---|---|
| `/ws` | display FREQ/POWR/RTA2/RTAF plus JSON commands/status |
| `/ws?iq=1` | primary IQBF |
| `/ws?iqaux=1` | auxiliary IQAX |
| `/ws/audio?channel=1` | AUD2 receiver PCM |
| `/ws/decode?channel=1` | SDE2 receiver events |

Audio/decode `channel` is optional: omit it for all channels, or select slot 1–8; invalid values return HTTP 400. These subscriptions do not carry display frames. The backend demodulates; the browser resamples PCM for playback and displays decoded results. The main panel follows its selected channel, and the workbench manages eight independent receiver definitions. Entering SDR preserves auxiliary receivers. Backend compatibility subscriptions support eight slots.

Stream identity is `(epoch, channel_id)`. Retuning assigns a new channel ID; replanning, recapture and new sessions assign a new epoch. Rejected requests preserve the previous configuration and stream. Old output tails precede the new start boundary. Each audio/decode subscriber receives its own sequence starting at zero, without resetting the receiver or other subscribers. Source sequence gaps and format changes without a start marker fail explicitly.

Streaming uses a bounded 64-message per-client FIFO. Overflow closes the slow subscription with code 1013; samples/events are not dropped to continue playback. Display traces instead use latest-wins delivery. See [ARCHITECTURE.md](ARCHITECTURE.md) for processing ownership and [KNOWN_ISSUES.md](KNOWN_ISSUES.md) for receiver limitations.

A subscriber source-sequence fault closes the subscription with code 1011 and reason `subscriber source stream invalid`. The browser treats this as a terminal stream failure. Actual queue overflow uses code 1013 and reason `IQ/audio consumer too slow`; decode acknowledgement expiry uses code 1013 and reason `decoded-output acknowledgement timeout`. Neither is reported as a receiver-worker failure.

Native receiver snapshots include an optional opaque `audio_signature` per receiver, derived from confirmed frequency and audio processing settings. It excludes receiver identity, enabled state, IQ resource planning and AM synchronization generation. The browser uses equal signatures to avoid playing identical audio twice when local gain and pan also match; reception, IQ subscriptions and recording identities stay independent. A service without this field keeps independent monitoring.

A shared native receiver worker failure stops all audio/decode receivers. The display/control stream receives `RECEIVER_ERROR` with `{error, failed:true, scope:'all_receivers'}`. STATUS retains the diagnostic in `sdr.receiver_error:string|null`. Audio/decode subscriptions close with code 1011 and reason `receiver failed`. Capture, PFB, fine DDC, raw IQ, spectra and waterfall continue. Receiver failure does not set `last_error` or `ddc_error`, add a spectrum error overlay, or change hardware `connected` state. Capture and device faults use global error handling.

Raw IQ and spectrum processing use bounded queue backpressure. Queue backlog waits until explicit failure without reopening the device. A sustained SDK stall ends capture with its original cause; recovery requires a new capture epoch.

Tuning, filter edits and zoom remain available. Recovery requires a demodulator or channel command with explicit `mode` or `demod`; selecting the current mode also retries. Frequency-only and filter-only commands, such as `SET_SDR_DEMOD {ifbw}`, retain the failure. Until stopped workers exit and the publisher closes failed subscriptions, recovery requests return `code:'receiver_retiring'`. Retry later. The service does not schedule automatic restarts or allow unbounded replacement threads.

Successful recovery clears `sdr.receiver_error` and gives each active channel a fresh stream identity and start boundary. Clients must discard old PCM and decoder state before consuming the new stream. A failed receiver cannot guarantee a complete ordered tail. Samples across a missing IQ interval are not joined.

Audio and decode controls show the error inline. The Decode row retains it when the floating decoder is closed. Retry uses the existing demodulator/channel controls. The browser clears the failure only when STATUS reports `sdr.receiver_error:null` with fresh active channel identities.

AudioContext setup/resume errors affect playback only. Stale worker callbacks are ignored. Local decoder/setup failures remain latched until explicit retry or a healthy fresh identity.

#### 2.4 IQBF/IQAX v2 header

All multibyte binary fields are little-endian. IQ frames contain normalized ADC full-scale samples (`1/32768`), with no framing AGC or clipping; filter overshoot may exceed `[-1,1]`.

| offset | size | field |
|---:|---:|---|
| 0 | 4 | magic `IQBF` or `IQAX` |
| 4 | 4 | version, u32 = 2 |
| 8 | 4 | seq, u32; 0 is the empty start marker |
| 12 | 4 | samples, u32 complex samples |
| 16 | 8 | rate, f64, output Sps |
| 24 | 8 | center, f64, Hz |
| 32 | 4 | channel_id, u32 |
| 36 | 4 | epoch, u32 |
| 40 | 8 | input_index, u64, capture complex samples |
| 48 | 8 | group_delay_samples, f64, output complex samples |
| 56 | 8×samples | interleaved I/Q, f32 |

`input_index` is the first output sample's causal position in the plan input, not the packet start; delay is separate. Alignment is `input_index / input_rate - group_delay_samples / rate`. The native path provides an exact index; synthetic sources use their own sample clock and zero delay. Offline reference fixtures may use `u64::MAX` for unavailable timing; never align that sentinel. Preserve u64 values as integers (JavaScript `bigint`).

`STATUS.channel_registry`, `actual.channel_registry` and `sdr.actual.channel_registry` share a snapshot. Entries contain `channel`, `channel_id`, `epoch`, `frequency_hz`, `output_rate`, `input_rate`, `group_delay_samples`, `state="active"`. Disabled channels are absent; outside SDR the registry and channel clocks are empty. The service does not negotiate v1.

#### 2.5 AUD2 audio header and clock

AUD2 has a 96-byte header followed by interleaved finite f32 PCM: total bytes = `96 + frames * channels * 4`.

| offset | type | field |
|---:|---|---|
| 0 | 4 bytes | `AUD2` |
| 4 | u32 | version = 1 |
| 8 | u32 | seq; 0 is an empty start marker |
| 12 | u32 | PCM frames (one sample per channel per frame) |
| 16 | f64 | actual PCM rate, Hz |
| 24 | f64 | tuned frequency, Hz |
| 32 | u32 | channel_id |
| 36 | u32 | epoch |
| 40 | u64 | input_index, capture complex samples |
| 48 | f64 | delay_seconds, finite and nonnegative; known portion if total delay is unknown |
| 56 | u32 | channels, 1 or 2 |
| 60 | u32 | low 16 bits: format = 1 (f32); bit 16: total delay unknown; bit 17: causal presentation clock |
| 64 | u64 | audio_index, PCM frame position |
| 72 | f64 | input_rate, capture Sps |
| 80 | u64 | input_fraction_num |
| 88 | u64 | input_fraction_den |
| 96 | f32 array | interleaved PCM |

Rates are finite and positive, IDs/epoch nonzero, and `0 <= num < den` with `den >= 1`. When total delay is known and the clock represents source samples, alignment is `(input_index + num/den) / input_rate - delay_seconds`. Unknown delay or causal presentation does not establish exact source alignment. Protocol blocks can share an input position or jump to the next protocol frame; validate PCM continuity with `audio_index`, rather than inventing input positions from PCM duration. Delay may vary per frame.

An NR tail reset can advance the AUD2 source position over discarded reducer samples while `audio_index` stays continuous. This forward source gap does not authorize overlapping or backward sample spans, which are rejected.

A new subscriber gets an empty `seq=0` retaining the first received block's global `audio_index` (possibly nonzero), then `seq=1`. Codec rate/channel changes may send a new start within the same IQ identity: `audio_index` resets to zero, the next sequence is 1, and the player clears queued PCM and rebuilds its rate adapter. Codec state remains valid. Mode changes mute old PCM until the new boundary.

Playback feedback is internal browser messaging, not a WebSocket acknowledgement API. The ingress sends `{type:'samples', samples, stereo, delivery, generation}`; the sink grants cumulative `{type:'credit', generation, limit}` capacity and confirms `{type:'accepted', generation, delivery}`. Only matching generations apply. Unsent and unacknowledged frames share a three-second delivery budget; overflow fails playback. Receiver mode selects buffering: analog startup 450 ms; DRM/DRM+ startup 1.6 s and queue limit 2.4 s.

#### 2.6 SDE2 decoded events

| offset | type | field |
|---:|---|---|
| 0 | 4 bytes | `SDE2` |
| 4 | u32 | UTF-8 JSON byte length, 1 byte through 2 MiB inclusive |
| 8 | UTF-8 | JSON envelope, exactly the declared length |

Envelope fields: `version=1`, `channel` (slot 1–8), `channel_id`, `epoch`, `seq`, `input_index`, `input_rate`, `input_fraction_num`, `input_fraction_den`, `delay_seconds`, `delay_known`, `causal_presentation`, `timing_kind`, `mode`, `event`. The three u64 position/fraction fields are decimal strings. Clock units and flags match AUD2. `timing_kind` is `sample`, `protocol_provenance`, or `causal_presentation`; protocol provenance does not imply a recovered sample timestamp. FT8 results retain their search-window time rather than asynchronous completion time. `event` contains mode-specific results; sequence zero carries `{type:'start'}`.

External providers emit `event:{type:'decoder-result',provider,mode,kind,data}`. `mode` must
match the SDE2 envelope; `kind` is `text`, `table`, `image` or `status`. Provider output uses
`delay_known:false` and `timing_kind:'causal_presentation'`; the current input anchor is not
a recovered message timestamp. Native stream identity, FIFO delivery and ACK rules still apply.
Multimon text is in `data.text`; rtl_433 preserves the device JSON object. OnAir retains its
versioned decoder payload (vessels, packets, decodes, image strips and status). Image strips
carry `id,w,h,y,rows,pw,ph,png`; `png` is base64 PNG with actual dimensions `pw × ph`.
`preview` and `scale` describe a reduced initial preview, and later strips overlay their declared
rows. `image_end` marks only the matching stream-bound picture complete; stopping does not.
OnAir's virtual kHz source labels are separate from actual receiver tuning:
`sourceFrequencyHz`, `onairFrequencyHz` and `frequencyOffsetHz` describe the mapping;
reported dial/RF fields are translated back while their upstream values remain available.
Its EOF drain reports `upstreamCompletionKnown:false` because the upstream protocol has no
completion acknowledgement. Live reconfiguration instead drains available results and emits a
`status` with `state:'stopped'`, `reason:'reconfigured'` and `upstreamCompletionKnown:false`,
then stops the old provider without an EOF grace interval. Ordinary retiring results never
change the confirmed selected mode.

GPS uses `provider:'gnss-sdr'`, `mode:'gps'` and `kind:'table'` with
`data.table` equal to `satellites`, `pvt` or `navigation`; `data.rows` holds objects.
Satellite and PVT tables are replacement snapshots. Satellite rows identify
`channel`, `prn`, `system`, `signal`, `cn0_db_hz`, `doppler_hz`, `acquisition`,
`tracking`, `nav_word_valid`, `tow_ms`, `pseudorange_valid` and `pseudorange_m`.
`acquisition_evidence` identifies the real upstream success-monitor or tracking
symbol evidence; GNSS-SDR's raw acquisition flag is retained separately.
`stale`, `tracking_stale`, `observables_stale` and `age_ms` qualify reception state.
PVT rows expose independent `valid`, `stale`, `age_ms`, `lat_deg`, `lon_deg`,
`height_m`, `valid_sats`, `week`, `tow_ms`, `utc_time`, DOP and local velocity.
Startup has `valid:false` and null coordinates; fixes expire after two seconds
without monitor updates. Retained stale coordinates are not a current fix.
Navigation rows are history records: PRN, TOW, the actual 300-bit GPS LNAV
subframe with parity, and the upstream parity-valid indication.
Every bridge record includes `input_samples` (samples forwarded into its FIFO),
`input_received_bytes`, `input_rate_hz`, `center_hz`, `gnss_version` and
`pre_2009_file`. These counters describe the bridge's input, not the exact decode
completion position. The surrounding SDE2 identity and clock remain authoritative.
`sourceFrequencyHz` and `center_hz` report actual RF tuning; `nominalCarrierHz`
remains 1575420000. `bench:true` identifies reception at the exact translated
400–450 MHz frequency opted into through `WEBSA_GPS_BENCH_HZ`; ordinary GPS
L1 reception reports `bench:false`. The opt-in changes no baseband decoder model.
EOF closes the FIFO, drains monitor events for three seconds, stops GNSS-SDR and
reports `upstreamCompletionKnown:false`; upstream requires additional samples to
advance some final observables and provides no consumed-input acknowledgement.
GPS retire status also sets `fix.valid:false`, clearing current snapshots.
Prepared GPS receivers emit no periodic snapshots until the first IQ input;
waiting for activation does not consume the input-stall timeout. Channel snapshots
follow acquisition sample-stamp generations, rejecting older UDP observations and
resetting on each newer acquisition even when it reuses the same PRN.

AM emits `event.type:'am-info'` on a 20 Hz source-sample grid, independent of browser rendering and audio callback sizes. Its SDE2 envelope uses the source clock and delay flags above. Payload fields are:

| Field | Type / meaning |
|---|---|
| `amSyncGeneration` | u32; actual receiver configuration generation corresponding to STATUS `am_sync_generation` |
| `syncEnabled` | boolean; actual synchronous detector state, not a pending command |
| `syncLocked` | boolean; enabled and carrier evidence passes power, coherence and phase/frequency consistency checks after a dwell |
| `carrierHz` | number or null; absolute recovered RF carrier in Hz, computed from the actual receiver frequency plus `carrierOffsetHz`; null without carrier evidence |
| `carrierOffsetHz` | number or null; recovered carrier offset in Hz, available with valid carrier evidence in either detector mode |
| `modulationPct` | number or null; `100 * sqrt(2 * variance(envelope)) / mean(envelope)`, measured before AGC over a source-clock window; single-tone-equivalent RMS depth, not programme peak depth |
| `inputDbFs` | number or null; raw IQ level `10*log10(mean(I²+Q²))` in dBFS; zero power is null |
| `rmsDbFs`, `peakDbFs` | numbers; final PCM RMS and peak in dBFS, with a -160 dB floor at silence |
| `scope` | array of at most 256 samples from actual final PCM after listener processing, NR and gain |

Unavailable carrier frequency/offset/depth are null, never fabricated zeros; deep carrier fading invalidates them. AM does not report SNR. `syncLocked:false` is expected with sync disabled even when carrier metrics are valid. Recovery uses coarse frequency acquisition followed by integral phase tracking with separate 150 ms acquisition/loss dwell; deep fades invalidate immediately. Input power uses 100 ms windows with 500 ms exponential smoothing. Envelope variance is normalized within each 100 ms window before its modulation power is smoothed over 1 s, avoiding false modulation from slower RF fading. Carrier offset uses 250 ms smoothing after acquisition. Audio waveform samples remain actual PCM.

The browser accepts AM telemetry and lock only when `amSyncGeneration` matches the authoritative STATUS generation; an unknown generation is not accepted. This rejects queued telemetry from an earlier switch even after off/on returns to the same boolean value. Device/control disconnect invalidates telemetry and generation; decode resumes only after STATUS confirms connection.

In WFM `wfm-info` events, `carrierHz` is the receive stream's channel-centre frequency (from the channelizer stream identity), not an independently measured carrier estimate; the remaining fields (deviation, pilot, RDS, scopes) are demodulated measurements.

SSTV subscribers receive a start and a bounded snapshot of the current image before live row increments. Image events contain `type:'sstv-image'`, `width`, `height`, `row`, `rows`, packed RGB byte array `rgb`, and `meta`; snapshots are split into at most 64 rows each. The service retains only the current image; browser history is local.
Picture boundaries remain ordered. Pinning an SSTV mode again starts a fresh canvas; long-silence timing stays bounded.

Radiofax subscribers receive a start, retained Gray8 segments of the current logical page, and the latest state before live events. `radiofax-image` contains `width`, zero-based `row`, `rows` (1–64), base64 `gray`, `revision` and `meta`. Decoded `gray` length is exactly `width * rows`; there is no fixed total page height. Metadata requires `serial`, `epoch`, actual receive `hz` (or null), `ioc`, `lpm`, `lines`, `complete`, `partial`, `segmentIndex`, `transmissionId`, `missingRows` and nullable `reason`. `missingRows` is a cumulative, strictly increasing list of local segment row indices below `meta.lines`; these rows contain Gray8 128 after correction. They occupy raster positions but are not valid decoded content. For each chunk, `meta.lines = row + rows`, including missing rows. Append chunks in order without row holes. A newer revision replaces that segment from row zero. Stream identity and metadata epoch isolate receptions; `transmissionId` groups their ordered storage segments into one logical page. Each segment holds at most 2400 rows. A storage boundary does not complete the logical page. Browser history is local to the session.

`transmissionId` is a canonical decimal string for an unsigned 64-bit integer, increasing within one image stream identity. The browser keeps a high-water mark for that identity: corrections can update retained pages, but deleted or evicted older pages cannot be created again. Clear preserves this mark. A new image stream identity starts a new ordering domain.

During a radiofax revision, the browser replaces received rows in place and retains the previous revision's remaining rows until their replacements arrive. Visible height does not shrink to the first chunk. Receive progress is tracked separately, so missing replacement chunks still fail validation. An interrupted revision preserves the retained raster for viewing and PNG export and marks an unfinished page partial; completion is accepted only once the revised rows cover the retained height.

`radiofax-state` reports `running`, `stage`, `hasPicture`, `signalLevel`, `carrierCoherence`, `ioc`, `lpm`, `modulation`, `polarity`, `width`, numeric integer `lines` (0–2400), `elapsedSeconds`, `clockStatus`, `clockSource`, `phasePixels`, `clockPpm`, `clockConfidence`, `complete`, `partial` and nullable `reason`. `stage` is `searching`, `fallback`, `apt`, `phasing`, `receiving`, `degraded`, `complete` or `partial`. `clockStatus` is `nominal`, `acquiring`, `locked`, `tracking` or `degraded`; `clockSource` is `nominal`, `phasing`, `image_content` or `manual`. The unlocked IOC576/120 LPM fallback and `image_content` clock estimates do not establish APT/phasing lock.

`lines` counts retained raster positions in the current segment, including explicitly missing rows. `elapsedSeconds` follows the logical page across storage segments. `reason` distinguishes `signal_lost`, `discontinuity`, `reconfigured`, `new_page` and `eof`; a confirmed protocol stop sets complete. A fade uses degraded state and does not create a new transmission. `snapshot_prefix_missing` means that earlier segments are outside the service retention budget.

`signalLevel` is the raw band-passed real PCM RMS with 0.2 s power smoothing, in linear full-scale units. It is not the complex-IQ dBFS meter. `carrierCoherence` is the smoothed 1400–2500 Hz energy divided by the 300–2700 Hz energy, limited to 0–1. Concentrated interference can also pass this energy gate; the value does not establish radiofax identity, phase lock or reception quality.


A `processing-end` event closes an old processing generation during reconfiguration. Its decimal `processing_generation`, boolean `drained`, `tail_complete`, `processing_complete`, and string `reason` describe the boundary; `tail_complete:true` does not imply that cancelled protocol work completed. Clients retain decoded history, retire live telemetry and unfinished rasters, and continue accepting the successor `start` on the same subscription. This event is not a receiver fault or a request to stop another channel.

A browser decode worker requests `/ws/decode?ack=1`. The service confirms support with `snapshotAck:true` on a `start` event, including a live start when no initial snapshot exists. The worker sends transport ACKs only after this confirmation; a server that omits the field receives none. After the page applies snapshot or live events in FIFO order, the worker sends `{type:'ack', channel, channel_id, epoch, seq}` with the highest applied sequence for that stream identity. An ACK cumulatively releases sent events through `seq` only for the same `(channel, channel_id, epoch)`; its sequence must match an outstanding sent event. Wrong identities, future sequences and duplicate ACKs grant no credit. Across the socket, at most 16 events and 4 MiB of SDE2 wire bytes may remain unacknowledged, including the 8-byte headers and `snapshotAck` start augmentation. Sending resumes as credit becomes available. Each sent event retains a 5 s acknowledgement deadline, enforced against the oldest outstanding event. Clients without `ack=1` receive the push stream.

Live receiver output is queued in shared batches, so full-image revisions do not consume one FIFO slot per image chunk. Queued batches and the batch currently being sent share a per-client byte budget derived from the advertised channel capacity (currently eight), with 20 MiB retained Gray8 per channel in base64 plus 4 MiB of envelope overhead; exceeding it still closes a slow consumer. A batch's reservation is released when sending finishes, without waiting for all its ACKs; sent but unacknowledged events remain subject to the separate 4 MiB wire-byte ceiling. Already admitted receiver audio and decoder events, including an initial snapshot, survive retunes and capture-epoch transitions and are delivered FIFO before retiring tails and new receiver identities. Decoder queue retirement preserves unaffected channels. Capture-epoch fences still apply to raw IQ, display data and admission of new packets. The oldest outstanding ACK deadline also bounds sends within multi-frame batches and snapshot loops.

The browser delivers event batches FIFO using `{type:'frames', envelopes}` and acknowledges application with `{type:'ack'}` on its internal worker port. Queued plus in-flight data is limited to 32 events / 4 MiB; overflow explicitly fails. The worker converts application acknowledgements into the sequence-bearing transport ACK described above only after the server has confirmed `snapshotAck`.

> **Spectrum detail**: POWR uses float32 levels and the paired native FREQ Hz axis. RTA/SDR retain native bins for traces, markers and export. Local zoom sends no acquisition commands and cannot improve physical RBW. Density keeps either the processed active trace or the received frames before Average/Max Hold; switching the source clears history. Afterglow uses a real-time half-life (Off, 0.2, 0.5, 2, 5, 10 s). Hit weights use elapsed time with bounded gaps. Off stops accumulation and drawing. Both, Density and Trace change visibility only; trace modes, markers, measurements and export retain their data sources. The wide trace glow is suppressed while density is drawn. Contrast changes colour independently of opacity. Waterfall history retains native frequency positions; missing bins stay blank unless SWP GapFill is enabled.

On an axis-version transition, the first frame for the new axis takes priority and
older-version frames are rejected. Density accumulates occupancy at native frequencies,
splits each amplitude contribution between two rows, and draws measured cell centres with a
centred vertical footprint of 1/3/5/9 CSS pixels (default 5). Display width changes only
the pixel footprint; it does not change occupancy, amplitude, frequency or measurements.
Contrast controls colour separately from opacity. Light mode uses a saturated palette;
measured noise remains visible at reduced strength. Grain defaults to 128; NaN gaps stay
blank. Off clears accumulation. VIEW freezes trace-source history; Live continues while
the trace is frozen. Width, theme and contrast changes preserve history.

Persist defaults to 0.2 s in SDR and 0.5 s in RTA. Saved user selections take precedence; Preset restores the active mode's default.

SDR uses a thin trace without the wide glow. Density retains native-frequency amplitude occupancy, Grain defaulting to 128, device-pixel rendering and per-revision refresh. Sparse views connect neighbouring accumulated distributions as continuous amplitude bands. A fixed-coordinate linear occupancy envelope limits their fade to the selected half-life when interpolation changes or a low-weight cell disappears; it uses max-composition, not a second accumulation. View geometry, gaps or source changes reset that envelope immediately. This display interpolation does not modify native history or measurements, replay trace frames or limit refresh rate. Dense full-span views and non-SDR rendering retain the original path. The SDR waterfall retains 16-bit amplitude levels and reconstructs valid neighbouring levels before colouring when enlarged; shrinking preserves native peaks. Missing bins remain gaps and zoom adds no measurement resolution. Zoom rail Top/Bottom change display bounds only, without writing Ref Level, attenuation or preamp; Restore or leaving Zoom clears them.

Density rasterization reports occupied amplitude bounds. Painting, pixel upload and final canvas composition use that band at full device-pixel resolution, including the footprint and interpolation margin; vacated rows are cleared. Sparse SDR bounds retain fading release history until reset. Empty plot height therefore adds no full-image upload or composition, while colors, widths, gaps and native history remain unchanged.

SWP/RTA/SDR raster paths use each device-pixel column's first, minimum, maximum and last original vertices, in source order, and split at every missing bin. Zoom reveals the retained native vertices as they occupy separate pixels; markers and export always use the complete source arrays. Optional SWP adaptive smoothing still uses the full frame, consistently with marker readouts.

The global navigator connects native vertices, retaining each bounded column's first/minimum/maximum/last samples in source order and breaking at missing bins. Sparse captures remain a continuous outline; this does not add measurement resolution.

CSV exports the selected current-mode processed trace and matching native axis;
OFF/missing/mismatched data produces a no-data notice, never an old SWP fallback.
Limits/channel measurements require the selected processed absolute trace;
normalized traces are N/A and emit no absolute measurement CSV rows. RTA/SDR
disable these measurement controls. dBc receives no external level offset.
SWP waterfall rows come from accepted raw POWR at most 10 Hz, not repaint calls.



---

## 3. Measurement Modes

### Harmonic

1. `SET_MODE {mode:'harmonic'}` → `SET_HARM {f0, count, span}`
2. Server auto-tunes per harmonic (H1~H5…), pushes `HARM` per result

### Phase Noise

1. `SET_MODE {mode:'pnm'}` → `SET_PNM {center, threshold, traceavg, start, stop}`
2. Server does incremental acquisition (`PNM_GetPartialUpdatedFullTrace`), pushes `PNM` per frame (with progress)

Each Harmonic/PNM Set starts a new `session_id`; successful command STATUS echoes it,
and HARM/PNM results carry the applied session ID. Clients reject older session results,
including an old hardware cycle finishing during reconfiguration. The browser clears
previous results on Set. PNM averaging has its own visible control.

### SDR (software-defined receive)

1. `SET_MODE {mode:'sdr'}` → optionally `SET_SDR {center, decimate}` (capture window)
2. `SET_SDR_TUNE {listen}` tunes the primary receiver (native: recaptures when needed, preserving auxiliary frequencies); `SET_SDR_CHANNEL {channel:1, freq}` uses the same native tuning behavior. `SET_SDR_DEMOD` selects the demodulator and its audio settings
3. The server pushes `RTAF` (panadapter spectrum + waterfall) and subscribed `IQBF` baseband. Native receiver audio uses `/ws/audio`; decoder events use `/ws/decode`. Level/squelch/ADM metrics are in `STATUS.sdr`.

On the native backend, SDR capture centers use the model's IQ tuning range
intersected with the device RF range (SAN-90: 9 kHz–9 GHz). The reported
`caps.fmin`–`caps.fmax` receiver range may be wider after sweep readback
(SAN-90: 8 kHz–9.02 GHz). For example, an 8 kHz receiver can use a 9 kHz IQ center.
The IQ capture may extend past either RF limit; the presented spectrum and
`sdr.actual.start/stop` are clipped to the reported RF range, so a wide window
at a low center shows bins only from the device minimum. Its half-width is not
added to the minimum center or subtracted from the maximum, and changing display
bandwidth does not force an otherwise valid center away from a device limit.
Frequency-axis drag and wheel gestures request a center within the reported RF
range; the backend confirms the IQ-compatible center. Dragging within the plot
only tunes within the displayed window; reaching an edge does not pan it. Use the
frequency axis to move the window. Listen dragging and arrow tuning stop at the
RF limits; clicks outside them do not tune a receiver. Enabled receiver
frequencies must remain inside the RF range, and their occupied bandwidth and
coarse-channel transition must still fit the capture passbands. An incompatible
auxiliary receiver rejects the change rather than moving that receiver. RTA
retains its whole-window limits.

### Exit measurement

`SET_MODE {mode:'std'}` → restore standard sweep.

### Soapy Server

`SET_MODE {mode:'server'}` stops Web capture and audio, joins the previous acquisition owner, and releases the physical SDK handle through the reusable owner thread. It does not spawn SoapySDRServer. The browser then uses separate Start, Stop and Restart actions. Configure is accepted only while stopped. Bind settings persist only until `websad` restarts; the defaults are `127.0.0.1` and `55132`.

`STATUS.server` fields:

| Field | Type | Description |
|---|---|---|
| `state` | str | `stopped`, `starting`, `listening`, `stopping`, or `failed`. `listening` means the owned process reported its bind endpoint; the device opens when a Soapy client uses it |
| `bind_address` / `port` | str / int | configured IP literal and port. `0.0.0.0` and `::` are not client destinations |
| `pid` / `uptime_seconds` / `exit_code` | int or null / int / int or null | owned child identity, elapsed seconds while that child exists, and the last reaped status (`128+signal` when signaled) |
| `error` / `dependency_error` | str or null | process or lifecycle failure / missing binary, probe failure, or unavailable `harogic` factory. Start and restart refresh the dependency check |
| `driver_available` | bool | last dependency check found `SoapySDRServer`, `SoapySDRUtil`, and the lowercase `harogic` factory |
| `graph_mode` | str | last `std`, `rta`, or `sdr` view remembered on entry; Spectrum returns there |
| `logs` | array | bounded raw stdout, stderr and lifecycle lines: `{seq, time_ms, source, text}`. Capacity is 200 lines and 64 KiB; pipe reads flush at 2 KiB. Pause, follow and clear are local display controls and do not delete the service buffer |
| `log_capacity` | int | retained line capacity (200) |

In this mode STATUS reports `connected=false`, `gnss=null`, and empty active `actual` objects. No Soapy client count, throughput, or remote telemetry is published. A browser refresh, reconnect, or close leaves the child running. `websad` shutdown stops and reaps it; the child supervisor also arms parent-death cleanup. Exit (`SET_MODE` to `graph_mode`, or another measurement mode) stops and reaps the child before acquisition resumes. A child that cannot be reaped keeps Server mode and the device reservation.

Handoff is refused when the loaded shim lacks `san90_session_release_v1` or a previous vendor owner is still parked inside the SDK. The mode change is not committed. The attempt still retires the previous capture: stream fences advance, `connected` becomes false, `gnss` is null, and the command returns `device_error`. The next capture uses a new epoch.

A client needs SoapySDR and the [SoapyRemote](https://github.com/pothosware/SoapyRemote) module. Use `driver=remote,remote=<host>:<port>,remote:driver=harogic`; replace a wildcard bind with the server host's reachable address, and bracket IPv6 hosts. SoapySDRServer exposes all installed device factories, so retain the `harogic` filter. The patched module writes CF32, CS16 or CS8 output. Before setup, the native-format query defaults to CS16 at rates up to 60 MSps (`fullScale` 32768) and CS8 above 60 MSps (`fullScale` 128). After setup it reports the selected output format and its `fullScale` (CF32: 1, CS16: 32768, CS8: 128). CS16 and CS8 payloads use 4 and 2 bytes per complex sample, versus CF32's 8. `native_format` selects SDK CS8/CS16/CS32/AUTO at setup and remains fixed until stream close. AUTO matches integer output width; for CF32 it selects CS16 up to 60 MSps and CS8 above that rate at setup. Explicit integer output requires a matching SDK width. CF32 output normalizes SDK components by `2^7`, `2^15` or `2^31`; CS32 is SDK input only. SoapyRemote can transport native integer samples while returning CF32 to the client. This uses a raw ring and bulk copies, without a zero-copy claim.

For automatic discovery, SoapyRemote publishes through mDNS/Avahi and SSDP; `avahi-browse -rt _soapy._tcp` lists advertised endpoints. WebSA does not add a duplicate publisher. Advertisements can include loopback addresses; the default `127.0.0.1` bind still accepts only local clients. For IPv4 LAN access, stop the server, configure `0.0.0.0`, restart it and use its reachable LAN address. In OnAir 3.2, stop reception, open the source selector and enable **Search the network for radios**, which is off by default. With the SoapyRemote module installed on the client, discovered radios appear through the Soapy source.

[SDRoxide](https://github.com/dividebysandwich/sdroxide#soapysdr-connectivity) connects through a build with the `soapy` feature and a client-side SoapyRemote module: `sdroxide --device 'driver=remote,remote=<host>:<port>,remote:driver=harogic'`. Its Remote settings and `--connect` use SDRoxide's own WebSocket protocol (normally port 4950), which cannot connect to SoapySDRServer. This is an upstream compatibility path; SDRoxide/SAN-90 reception has not been verified here.

Upstream client paths include [SDR++](https://github.com/AlexandreRouma/SDRPlusPlus#sources) (`soapy_source` is optional/deprecated and may require a custom build), [OnAir desktop](https://github.com/adswill/OnAir#network-radios) (SoapyRemote), [SDR--](https://sdrmm.com/docs/hardware) (only builds with the Soapy path), and [OpenWebRX](https://github.com/jketterl/openwebrx/wiki/Supported-Hardware) (SoapyRemote). KiwiSDR has no general Soapy input. OnAir 3.2.0 source startup and IQ delivery at 15.625 MSps were exercised on SAN-90. With the updated driver, a five-second OnAir CLI FM run at 7.8125 MSps used CS16 UDP transport and CF32 client output, reported carrier lock, stereo and zero dropped samples, and exited successfully. Another five-second CLI run exercised 62.5 MSps CS8 UDP input with zero reported drops and a successful exit; its receiver resampled to 4 MSps and did not achieve FM carrier lock. This establishes the full-rate input path for that run, without proving GUI FFT processing at 62.5 MSps, full-rate decode quality, television lock or long-duration continuity. Other application paths remain upstream compatibility notes. See [driver setup and measurements](../../tools/README.md#soapy-server-driver-setup) for discovery, transport and performance limits.

---

## 4. Python Example

```python
import asyncio, json, struct
import aiohttp

async def main():
    async with aiohttp.ClientSession() as s:
        # 1) REST state
        async with s.get('http://localhost:8080/api/state') as r:
            st = await r.json()
            print('connected:', st['connected'], '| mode:', st['mode'])

        # 2) WebSocket trace frames
        async with s.ws_connect('ws://localhost:8080/ws') as ws:
            await ws.send_str(json.dumps({'cmd': 'SET_FREQ', 'center': 1e9, 'span': 100e6}))
            async for msg in ws:
                if msg.type == aiohttp.WSMsgType.BINARY:
                    head = struct.unpack('<4sIIf', msg.data[:16])
                    magic, ver, pts, sweep_ms = head[0], head[1], head[2], head[3]
                    if magic == b'FREQ':
                        freq = struct.unpack_from('<%dd' % pts, msg.data, 16)
                    elif magic == b'POWR':
                        import array
                        pwr = array.array('f')
                        pwr.frombytes(msg.data[16:])
                        print('POWR', ver, 'points', pts, 'peak', max(pwr))
                elif msg.type == aiohttp.WSMsgType.TEXT:
                    obj = json.loads(msg.data)
                    print('JSON:', obj.get('cmd'))

asyncio.run(main())
```

---

## 5. Notes

- **Single handle**: only one process may open the device at a time (SAStudio4 and the Web service are mutually exclusive)
- **Serialized device calls**: the native SDK shim owns a retained session thread and serializes access.
- **Binary SDK**: `libhtraapi.so` is proprietary, obtain it from HAROGIC (the `htra_api.py` wrapper is bundled)
- **SystemClockSource=External**: dangerous, do not use (hangs the device)


## Trigger reference

`SET_TRIGGER` has eleven parameters. Requests remain in `req.rta.trigger_<field>`;
`req.rta.trigger_actual` reports SDK-applied source, edge, level, safetime, delay,
pretime, acqtime, retrigger, retriggerperiod, out and outpolarity, plus runtime metadata.
Applied settings come from ProfileOut, and `poi` from SDK FrameInfo. Missing proof is
unknown/null; requests are never substituted for applied settings.

RTA2 TRIG metadata includes `valid`, `supported`, `simulated`, `generation`, `frames`,
`capture_id`, `packet_index`, `packet_count`, `waiting`, `complete`,
`trigger_missed` and `last_sdk_status`. Generation advances on successful configuration;
frames counts successful SDK packet acquisitions, not STATUS polls. Capture identity is
null when the SDK has none. Packet index is one-based since BusStart, becoming null
after continuity loss; packet count is known only for FixedPoints. Waiting is null
without SDK proof: -10 can mean trigger waiting or a communication error.

For finite Level/FixedPoints captures without retrigger, completion requires uninterrupted
packet continuity and the final configured packet or SDK status -301/-303. Even -301/-303
cannot establish a complete assembled capture after data loss. Timer, adaptive and
retrigger boundaries remain unknown without proof. -302/-303 sets `trigger_missed`;
-303 retains the completed capture and holds its final data while reporting the missed
trigger. A valid complete packet is processed before freezing; older generations are
ignored. No delay or first frame establishes completion. Finite Level capture retains
the maximum across its traces and packets. Simulated RTA reports unsupported, invalid
metadata and cannot produce a hardware HIT. Clock-relocked statuses -36..-39 retain
valid RTA data and last_sdk_status; Auto treats them as warning/pending.

After proven completion, the server stops SDK reads and retains the exact completed
RTA2 for that generation, republishing it at 5 Hz for late subscribers. Replays do not
advance frames, packet_index or Auto observations. Successful reconfiguration,
rearm/Free Run, rollback restart, invalidation and mode exit clear the retained frame.
This is a server-held capture; the finite SDK acquisition need not stop producing packets.

In standard SWP, Capture uses a browser threshold crossing between consecutive
display traces; only level, edge and capture helpers apply. RTA supports device timing;
SDR/harmonic/PNM disable these controls. External/timer API states are displayed
read-only. Free Run/Escape releases a held capture; successful mode/Preset transitions
reset local hold. RTA entry selects bus. Auto Scale does not release an armed trigger.

## Auto Scale and local level controls

Explicit Auto runs for at most 20 seconds / 24 steps. Each step requires fresh data
from the same mode/configuration. SDK IF overflow (-12) raises configured actual Ref
by 10 dB up to `caps.ref_max`, without lowering an already higher Ref or changing gain
selectors. Waiting/no data alone never raises Ref. A clean spectrum drives placement,
then fresh clean data must confirm it. Recovery does not lower hardware Ref back into
the proven overload region. Ordinary SDR fitting changes only display Ref; explicit
overflow recovery may reconfigure IQS through normal admission. Entry fitting uses
`display_only:true` and never changes hardware.

`auto_ref.result` is `idle`, `adjusting`, `applied`, `ok`, `no_data`, `overflow`,
`limit`, `stalled`, `failed`, `cancelled` or `timeout`. `target` is non-null only on
successful completion. No usable acquisition ends as no_data; an adjusted but
unconfirmed session ends as timeout. Manual Ref/AMP, frequency, mode and Preset edits
cancel the session and clear its old target. There is no background overflow recovery.

Manual Ref edits use an independent local viewport (-200..+100 dBm plus external Offset)
in SWP/RTA/SDR. SET_REF and AUTO_SCALE.current_ref subtract Offset and clamp to the existing
-160..+40 dBm wire bounds. A matching raw request report accepts the viewport without replacing
it: display +100 with Offset zero requests hardware +40. Further viewport moves with the same
clamped request do not reconfigure hardware or restart its pending timeout. Rejection or timeout
restores the accepted display. SDR stored `sdr.ref` confirms the request separately from effective
`ref`. Auto placement adds Offset once. Offset edits shift the absolute axis and trace together
without hardware writes; relative dB is unchanged. Waterfall Fixed
range requires finite low < high; rejected edits revert with a notice. SWP/RTA mapping/range changes clear quantized history even while paused; SDR retains raw amplitudes/RF axes and recolours history. Its Auto Floor source (Full Capture / Current View) is independent of Fixed dBm limits and the spectrum scale. SDR history retains up to 65536 native columns and 512 rows within 64 MiB for rows plus shared axes; budget pressure evicts whole oldest observations. See [architecture](ARCHITECTURE.md) and [validation](../validation/sdr-display-2026-11-04/README.md). Local Scale/Unit/Offset/Waterfall
remain usable in std/rta/sdr; exclusive harmonic/PNM disables Level controls.


### Receiver mode controls and application receipts

Logical `processors[0]` and `SET_SDR_CHANNEL` share validation for `am_sync`, `pitch`, `deemph_us`, `stereo`, `audio_rate`, nullable `sstv_mode`, and all `radiofax_*` persisted acquisition/IOC/LPM/modulation/polarity/clock-recovery/phase/ppm fields. `am_sync_generation` is server-owned. Main-panel Mode settings and the workbench use the same fields; duplication, presets and confirmed mode memory retain them. Instant actions are excluded from persisted processors.

A command response confirms acceptance. `receivers[].control_receipts` reports `queued`, `applied`, `inactive`, `cancelled`, `expired` or `failed`, bound to UID, service, decimal revision/request ID and processing generation. DSP application reports the first affected source block: channel ID, epoch, input index, fraction, actual input rate and delay; precision is `block`. `inactive` is a saved inactive configuration with a control boundary, not processed media. Superseded configurations that never reached data are explicitly cancelled. Model preparation failures cannot produce an applied receipt or substitute another algorithm.

For `type:"action"`, submit the original `receiver_uid`, `service_id`, `expected_revision`, `request_id`, unique `action_id`, current snapshot `action_token`, and `action:"radiofax_new_page"` or `"radiofax_reset_correction"`. Actions require active Radiofax. Tokens use service monotonic time and expire after 5000 ms, including time waiting for data. Retries retain identity; a changed payload conflicts. The action history holds 128 IDs; eviction advances a token rejection floor so an evicted action cannot replay. Old services and expired windows require a fresh explicit user action.

Each receiver accepts up to 64 outstanding controls, with at most 64 inexpensive actions processed before a data step. The shared preparation queue is also bounded to 64, with explicit busy results. Receipt history is bounded to 128 per branch. No control path drains all receiver owners; full queues do not discard IQ or affect another receiver's recording. Compatibility channel status includes logical control metadata for binding main-panel requests at submission.


`san90_next_batch_v1` is an optional SDK adapter ABI used only for full-rate capture. It fetches at most four physical packets in one permanent-owner dispatch, stopping before the next scheduled spectrum packet. The caller owns all four buffers; each packet retains its individual timestamp, calibration, warning and SDK timings. A failed call returns its valid prefix followed by the original error. Reconfiguration discards prefetched old-generation packets at the existing source boundary. Narrow input rates and older adapters retain single-packet calls. The USB queue is unchanged; this reduces synchronous thread rendezvous rather than hiding latency in a larger USB queue. Capture `fetch` wall time covers the batch on its first packet and cached delivery on subsequent packets; per-packet SDK timing remains separate.

Reception continuity is configured per receiver with `continuity: "continuous" | "strict"` (default `continuous`). It governs live source/processing recovery; recordings always end at a gap and retain their independently readable valid prefix. A processing-only overload leaves healthy IQ and IQ recordings running. Shared source gaps interrupt every affected lane. Device/internal faults are not automatically retried.

`receivers[].reception` reports `recovering`, `preparing`, `receiving`, or `stopped`, bounded interruption history, attempts (limit 3), and manual recovery eligibility. Each interruption records scope, failed identity, source positions, reason, and recovery time. Input positions and known missing sample counts are decimal strings; unknown lengths are null, especially across capture epochs. Submission is not recovery completion: only new valid input/processing confirms reception. Use the existing configure command with unchanged confirmed config and `recover: true` for an eligible manual resume.
