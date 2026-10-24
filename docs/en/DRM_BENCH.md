# DRM bench loop (PlutoSDR → SAN-90)

Use a repeatable local signal to capture DRM baseband and compare decoders. Fixture provenance, manifests, formats and regeneration instructions belong in the [DRM fixture README](../../tests/fixtures/drm/README.md). This guide does not record fresh hardware verification.

## Configuration

The [station configuration](../../tools/bench/drm_bench/station_iq.toml) currently generates mode B, SO3 (10 kHz), 64-QAM MSC, 16-QAM SDC, long interleaving, protection A/B 0/1, and a mono **xHE-AAC** 1 kHz tone labelled `SAN90 DRM BENCH`. IQ output is int16 stereo (I left), at -15 dBFS; tone level is -12 dBFS. The committed live capture predates this configuration and carries HE-AAC (12 kHz core + SBR, 24 kHz PCM), not xHE-AAC.

The recorded arrangement used a PlutoSDR and a SAN-90 external antenna 2 m apart:

- Transmitter LO: 400 MHz; DRM offset: +100 kHz to avoid LO leakage.
- Analyzer: SDR mode, center and listen frequency 400.1 MHz, decimate 256 (about 195 kHz IQ capture bandwidth), reference level -40 dBm, DRM demodulation.
- Use a 15 s IQ file. A 60 s file required a roughly 250 MB cyclic buffer and failed on the recorded Pluto setup; 15 s required about 63 MB.

## Generate, transmit and capture

Run from this repository with an external DecDRM `decdrm` executable on `PATH`:

```sh
decdrm tx tools/bench/drm_bench/station_iq.toml --output drm_iq_15s.wav --duration 15
python3 tools/pluto/pluto_drm_tx.py --iq drm_iq_15s.wav --lo 400e6 --gain -20 --seconds 600
```

Start the service and configure the SDR session before capture. Confirm an approximately 10 kHz signal at 400.1 MHz in the panadapter. Capture requires Python `aiohttp` and NumPy:

```sh
python3 tools/bench/drm_capture.py --seconds 10 --out /tmp/drm_live.f32 --json /tmp/drm_live.json
```

The tool subscribes to `/ws?iq=1&noaudio=1`; it does not configure or open the device. It saves interleaved little-endian float32 I/Q from IQBF wire v2 frames, plus rate, level and delivery metadata. It drains one second of queued frames before measuring. Use the reported rate for offline decoding; the committed live fixture publishes 48828.125 Hz rather than the DRM30 core's 48 kHz. Capture levels and historical RF measurements are not calibration constants for a new session.

## Offline comparison

The committed capture and synthetic fixtures are described in the [fixture README](../../tests/fixtures/drm/README.md). The native receiver fixture command is:

```sh
cargo test --manifest-path wasm/Cargo.toml --release --test drm_live_fixture -- --nocapture
```

This command reads the committed fixture, not `/tmp/drm_live.f32`. Native codec builds require `pkg-config` and FDK-AAC >= 2.0.3 with DRM/USAC support; see [AAC format and build notes](DRM_AAC_RESERIALIZATION.md).

For an independent comparison of a new capture, supply an external Dream console binary with `--status-socket` support and the capture's actual published rate:

```sh
python3 tools/analysis/drm_oracle_check.py /tmp/drm_live.f32 48828.125 --dream-bin /path/to/dream
```

The oracle tool resamples to 48 kHz and normalizes before writing Dream's int16 IQ WAV. Dream and DecDRM are external prerequisites; their binaries and external build instructions are not supplied here.

## Audio interpretation

DRM audio must be deframed using its signalled codec configuration. With the text flag set, the final four bytes of each audio logical frame are text and must be removed before splitting access units; otherwise the reverse-read SBR payload is displaced. The historical HE-AAC capture uses five 208-byte access units per 400 ms frame and produces a 1 kHz tone at 24 kHz PCM.

The direct AAC-LC fixture has a 24 kHz core without SBR and outputs 24 kHz. The synthetic end-to-end AAC IQ fixture signals SBR over that core and therefore reports 48 kHz output. Use the decoder's returned sample rate and channel count, rather than assuming either rate from the fixture name. Streaming decoding also depends on persistent MSC deinterleaving and carrier correction across input blocks.
