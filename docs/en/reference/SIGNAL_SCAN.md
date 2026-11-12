# In-band scan and identification

The framed `i` icon beside the channel workbench opens **Scan / Identify** without starting a scan.
**S** toggles the window in SDR mode, including inside its popout, outside text fields and other dialogs.
Start/Pause controls the shared continuous scan. Hiding the window leaves scanning running and
ends temporary listening; collapsing or popping it out changes only the view. Region identification
works independently: drag a frequency band on the spectrum or waterfall, release to submit, or
press Esc to cancel. Both views show the selected band. Frozen plots and the waterfall still analyze
current IQ, not historical IQ. **I** remains the IQ recording shortcut.

Scanning automatically identifies stable candidates within its budget; More settings can disable
automatic identification. Discovery uses an independent 65,536 / 131,072 / 262,144-point Blackman–Harris FFT,
chosen from the actual input rate to keep bin spacing at or below 250 Hz, and four 50%-overlapping
Welch linear PSD segments, normally five observations per second. At 62.5 MS/s this gives 238.42 Hz
spacing without changing shared capture geometry or allocating another receiver. The low-sidelobe window
keeps strong carrier leakage from joining neighbouring stations; PSD normalization still preserves power.
Discontinuities or receiver load
skip whole blocks, reduce cadence or pause analysis. This does not provide continuous short-burst
capture across the band. Fixed frequency windows integrate linear power and require at least eight
FFT bins; reduce actual capture bandwidth if necessary. Display zoom adds no resolution.
Above-background margin is neither RF SNR nor uncalibrated dBm. Activity ratios count actual observations.

The noise reference uses ordered statistics from cells within ±750 kHz of each RF frequency,
excluding a ±12 kHz guard. The 20th percentile is corrected for the mean of four noise periodograms
(about ×1.7416), and anchor values are interpolated in log power. An observation uses its current local reference;
an earlier low floor cannot remain frozen after noise rises. Nearby reference cells may extend
outside the selected search region, so changing the selection alone does not change the reference
for a given frequency. This follows frequency-dependent noise without using a dense band average.

Neighbouring components use a fixed merge gap rather than a gap that grows with the merged width;
this prevents a run of HF stations from becoming one unprobeable band. A candidate exceeding one analysis passband
stays unavailable with `analysis_passband_limit`; the UI asks for a narrower signal selection and blocks
preview/add configurations that cannot fit. This is a per-candidate analysis limit, not a rejection of the
whole selected discovery region. Optional wider FM pilot capture falls back to the original analysis
width when only that width fits. PFB passband, rate, cost and formal receiver admission remain unchanged.
When no peak is detected and the entire requested region cannot fit one probe, discovery returns
`no_signal_detected` with no region candidate or probe. An explicit narrow IQ measurement remains
available below the discovery threshold and is marked `requested_window` / `not_detected`.

Identification first taps compatible existing baseband IQ before demodulation, otherwise borrowing
an unused non-Current slot. Wider tapped IQ passes through the existing planned DDC for region filtering
and decimation before feature analysis. Observation checkpoints are 160, 400 and 800 ms. Strong AM
and broadcast FM evidence must agree across two independent halves before early completion;
other candidates continue collecting the same contiguous IQ. Pure carriers retain the full observation
to avoid prematurely suggesting CW during an AM speech pause. Gaps reset checkpoints without joining
samples across the gap or extending the original two-second lease. Each IQ buffer remains bounded
by 8 MiB. Features are computed on demand; verified FM pilots skip digital modulation features.
Measured thread CPU time is charged to a 1/11-core average budget with at most 20 ms of saved CPU
burst credit. Cancellation and new selections do not erase CPU debt. Receiver pressure still slows
or pauses scanning first. Candidates reserve no formal slots; stopped saved receivers retain theirs.
There is at most one temporary probe. Temporary DDC uses the same geometry/cost admission.
Formal creation waits for bounded synchronous probe retirement and never reuses an unreleased slot.
At sixteen receivers, discovery and compatible IQ reuse remain possible; other identification waits for resources.

Candidates whose detection margin cannot qualify for early completion go directly to the full
800 ms observation, avoiding short-window computations that cannot finish the task early. They
remain eligible for modulation verdicts from the full observation.

Scanning yields when queued plus in-flight recording data reaches one quarter of its existing
byte/item budget or its oldest unfinished input reaches 250 ms. Recording's hard byte/item/age
limits remain unchanged. Scan admission never waits for a recording lock or disk I/O, and resumes
after the existing pressure cooldown once the backlog drains.

Instantaneous occupancy need not cover the next FM excursion. For candidates at least 40 kHz wide,
the same single probe may retain 180 kHz IQ when selection/capture geometry permits. The original
window is analyzed first. Only unknown/FM-like results may adopt a wider result, requiring a pilot,
low envelope variation and a center inside the original measured window. Existing AM/FSK/PSK
verdicts remain; compatible narrow IQ can still be reused, without reserving another slot for the
wide check. Sampling and compute budgets still apply. Center refinement also shrinks subsequent
probes to stay within the original analysis bounds.

Detection smooths linear spectra, expands toward background and merges nearby regions to keep AM
sidebands and FM components together. Measured carrier width differs from the analysis passband:
narrow peaks allow up to 12 kHz for audio sidebands, strictly within the selected region. Adding AM
defaults to at least 12 kHz. AM uses frequency-offset-tolerant carrier-line and envelope features;
FM uses envelope/discriminator features and searches for a pilot across 18.85–19.15 kHz in three
independent windows. AM carrier and mean WFM discriminator offset refine the suggested center and
advance its generation while preserving measured frequency bounds.

Initial labels describe carrier, AM/FM-like, 2FSK/4FSK-like, PSK-like or unknown shapes. They do not
name protocols such as DMR or P25 automatically. A 19 kHz FM pilot is a dedicated feature; valid RDS
groups, DRM FAC or FT8 messages from already running decoders can supply independent protocol evidence.
Identification does not start providers in bulk. `label` describes modulation shape; `protocol` requires
structured evidence. `receiver_available` describes suggested receiving-mode availability, while
`decoder_available` describes availability for an established protocol. Changed recommendations advance
candidate generation; ordinary level updates do not. Confidence is not measured accuracy. Uncertain
results can be added as IQ or heard after explicitly choosing an audio mode. Add/reuse still checks
receiver admission, identity and resources.

## Find, listen and keep

**Discover signals** finds unknown frequencies. Each row offers **Listen** and **Add**. Listening reuses
a compatible saved receiver or admits a temporary receiver, reusing that slot when switching results.
Stop or hide releases it. **Mute other channels while listening** is enabled by default and changes only
local speaker routing; reception, decoding and recording continue. Disable it to retain the existing mix.
Ending the preview restores routing, Solo and follow settings. A receiver edited by the user or taken
over for recording is not automatically deleted. Add retains the preview receiver without rebuilding its
IQ stream and enables audio according to the existing mix rules.

**Listen / Add mode** affects only the selected result's reception, not its classification. Auto uses
the recommendation. IQ-only has no audio; use the workbench to record/export/analyze its baseband.
Unknown signals can be heard using an explicit AM/USB or other audio mode, or added as IQ.

Search accepts MHz, kHz and type text. Type/status filters change only the list. Sort using frequency,
bandwidth, type or margin column headings. Pointer/keyboard interaction locks row order so changing
power cannot move buttons. Clear removes results without deleting saved channels; running scans can
detect them again. More settings can clear only inactive results. Late analysis cannot restore cleared rows.

**Fixed frequency measurement** integrates known windows in parallel and measures observed activity,
for example several frequencies spaced by 100 kHz. It creates no receivers. Use discovery or a region
for ordinary station finding. Candidates require valid nonnegative RF frequencies; IQ windows crossing
zero do not display negative-frequency images.

## HTTP

- `GET /api/sdr/scan`: shared session, source identity, range, up to 128 summary rows, probe occupancy
  and observation/CPU/skip statistics. Limits: 256 candidates and 32 queued identifications. Summaries
  omit detailed evidence. `bandwidth_hz` and `low_hz/high_hz` describe the measured/requested window;
  `analysis_bandwidth_hz` is the actual analysis passband. AM/WFM `center_hz` can be refined independently
  of the measured window midpoint.
- `GET /api/sdr/scan/signals/{id}`: on-demand features, protocol evidence, actual IQ source and sample
  times. Times are host reception times, not calibrated RF UTC.
- `GET /api/sdr/scan/events`: coalesced SSE change notifications, limited to eight subscriptions per
  engine. A slow observer retains the latest version, without queued IQ or full snapshots. Visible
  windows refresh immediately on notification, fall back to 500 ms polling while disconnected and
  close the subscription when hidden.
- `POST /api/sdr/scan`: `start`, `pause`, `stop`, `configure`, `identify`, `identify_region`,
  `cancel_region`, `ignore`, `clear`.

All commands require current `service_id`, unique `request_id` and decimal-string `capture_epoch`.
Except for `identify_region`/`cancel_region`, supply the GET response's `scan_id` and `expected_revision`.
`identify`/`ignore` also require `signal_id` and string `signal_generation`. Success includes `request_id`
and the latest snapshot. An identical request ID/body retries the original receipt; different content
is rejected. Conflicts return 409; error fields are `code`, `error`, `request_id`.

Example region request, using identities from the current GET:

```json
{"type":"identify_region","service_id":"…","request_id":"unique-id","capture_epoch":"…","low_hz":9830000,"high_hz":9840000,"origin":"waterfall"}
```

`origin` is `spectrum`, `waterfall` or `manual`; the source is always `live_iq`. Requests collect new
contiguous observation blocks and split into at most eight regions. A new region replaces the temporary
group without changing the continuous scan range or starting scanning. Out-of-capture bounds are rejected
without silent clipping or tuning. Service, capture geometry, real capture epoch or reference-level
changes invalidate old results. Periodic SDK amplitude calibration only reloads the current observation
block and rescales the background; it does not cancel identification. Cancellation/new selection prevents
late responses from replacing current results.

`clear` accepts `scope: "all" | "inactive"`, defaulting to all. It retains scan running state, range,
frequency windows and formal receivers. Identity/revision and deduplication checks still apply; old
results can be cleared while capture is disconnected.

`configure` changes only supplied fields: `range: {low_hz,high_hz} | null`, `channels: [{low_hz,high_hz}]`
(at most 256), `threshold_db` (5–20), `auto_identify`. No channel table means region discovery. Formal
receiver creation can include `scan: {capture_epoch,signal_id,signal_generation}`; admission and commit
both recheck the recommendation.

Fake mode replays independent basebands without shared RF IQ and supplies no simulated scan results.
Separate synthetic tests cover scanning algorithms. RF accuracy, duration and high-load capacity require
qualification on the actual device, rate and receiver configuration.

Completed rows expose `timing`: `queue_ms`, `sampling_ms`, `feature_queue_ms`, `processing_ms`,
`total_ms`, `observation_seconds`, `checkpoints` and `early_exit`. Durations use the host monotonic
clock; sampling time includes analysis/budget waits between checkpoints. Total time starts at
identification enqueue and excludes discovery. `checkpoints` is the last observation stage (1–3),
not the actual job count. Statistics also expose
feature jobs, completions, early completions, extended observations and current CPU-budget wait.
Early completion does not raise a modulation verdict's confidence or confirm a protocol.

## Validation

`cargo test --release -j 1 --manifest-path backend/Cargo.toml --bin websad -- --test-threads=1`
and frontend Vitest cover algorithms, commands, resources and selection state. `tools/e2e/signal_scan.mjs`
and `tools/bench-signal-scan.py` require `WEBSA_SCAN_LIVE=1` and a running hardware service for the browser
workflow and 1/8/16-receiver measurements. Specify artifacts outside the checkout. The latter requires
a single-receiver baseline, tags its own receivers and removes its recordings/receivers on exit.

The performance tool's `--mixed-audio` uses AM/NFM/USB/LSB receivers and subscribes to every IQ and
audio stream to check sequences, stream identities and exact sample continuity. A WFM Current also
subscribes to decoding events. The default still
adds IQ-only receivers. Both modes check Current IQ/audio recordings, full-capacity waiting,
existing IQ reuse and formal creation preempting the temporary probe. Recording checks traverse all
volumes and index pages, reconciling block counts, sample totals and positions across blocks.

`tools/verify-broadcast-scan.py` is an optional local regression for 9–10 MHz AM and 100–107 MHz broadcast FM.
It requires `WEBSA_SCAN_LIVE=1`, a single Current baseline and external `--artifacts`. It temporarily
retunes/restores capture, checks multiple detections and modulation/pilot evidence, and repeats three
independent region requests per band. Per-candidate first completion and first/last result latency
are saved with 50 ms polling granularity, measured from after the command response. Missing broadcasts must fail; this is not a generic CI test or
RF accuracy measurement. Browser gates must wait for region `complete`; cancelled, stale or other
terminal states cannot count as successful identification.
