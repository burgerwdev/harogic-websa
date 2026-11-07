# Multichannel draft-v1

Native capture adds `parameters_state` (`live`, `last_known`, `unknown`, `simulated`) and `state:idle`. Bandwidth/input rate are nullable before opening a source. Unknown center is requested-only; epoch `"0"` is a sentinel. Independent fixture lanes report a simulated common display window/rate; per-lane clocks are authoritative. Known hardware epoch identifies the DSP source, with input rate taken before PFB/DDC.

Portable JSON Schema for the P2 snapshot/receiver/command/event/task boundary and
the native P3/P4 receiver/processed-output API and P5 backend IQ tasks. This is an intentionally versioned, evolving draft.
The corresponding TypeScript definitions live in
`frontend/src/core/multichannel.ts`; semantics and migration points are documented
in [the receiver contract](../../../docs/en/reference/RECEIVERS.md) and its Chinese counterpart.

`fixtureReceiverAdapter.ts` creates the development snapshot and owns simulated
state. No fixture here replaces original captured IQ or the production contract.

Production IQ recordings use WBIQ0002 with independently checked blocks, bounded
index pages and restart recovery. WBIQ0001 remains an offline experiment. Neither
container is SigMF. Native recording states and precise decimal `state_revision`
are service-owned; browser fixtures do not save files.

Native `iq_archive_read` / `iq_range_max_samples` capabilities enable checked,
finished-task range reads. SigMF downloads decompress CF32 into a standard
meta/data archive; the internal WBIQ containers keep their original format.
`tools/e2e/native_multichannel_archive.py` validates actual snapshots against
this schema and exported samples with the independent official SigMF reader.

New published-audio tasks write WAV/FLAC/Opus media directly. Legacy WBAU0001
fixtures retain the bounded CRC/Zstd journal, original AUD2 Float32 samples and
packet-owned backend configuration for backward-compatibility checks. The schema publishes
`audio_archive_export`, `published_audio` / `f32_le` and audio segment metadata.
`tools/e2e/native_multichannel_audio.py` independently checks actual network bytes,
CRC/system libzstd and Float32 WAV exports with SciPy (development-only dependency).
These samples include shared backend processing; they are not protocol-input audio.

`audio_archive_read`, decimal `audio_range_max_samples` and integer
`audio_range_max_seconds` enable bounded original-PCM playback. An `audio_record`
may carry a `selection` with exact decimal `record_skip_frames`, `frames` and
`range_sample_start`; the original header and source clock fields remain unchanged.
`tools/e2e/native_multichannel_audio_playback.py` checks the selected WAV bytes,
two-volume stereo/mono clocks, original task identity and real media lifecycle.
