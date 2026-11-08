# DRM development reference

[Documentation](../INDEX.md)

## Bench workflow

Use a repeatable local signal to capture DRM baseband and compare decoders. Fixture provenance, manifests, formats and regeneration instructions belong in the [DRM fixture README](../../../tests/fixtures/drm/README.md).

### Configuration

The [station configuration](../../../tools/bench/drm_bench/station_iq.toml) currently generates mode B, SO3 (10 kHz), 64-QAM MSC, 16-QAM SDC, long interleaving, protection A/B 0/1, and a mono **xHE-AAC** 1 kHz tone labelled `SAN90 DRM BENCH`. IQ output is int16 stereo (I left), at -15 dBFS; tone level is -12 dBFS. The committed live capture predates this configuration and carries HE-AAC (12 kHz core + SBR, 24 kHz PCM), not xHE-AAC.

Example bench configuration; use actual device readback for input level and capture geometry:

- Transmitter LO: 400 MHz; DRM offset: +100 kHz to avoid LO leakage.
- Analyzer: SDR mode, center and listen frequency 400.1 MHz, decimate 256 (about 195 kHz IQ capture bandwidth), reference level -40 dBm, DRM demodulation.
- The example uses a 15-second IQ file; keep the cyclic transmit buffer within available transmitter memory.

### Generate, transmit and capture

Run from this repository with an external DecDRM `decdrm` executable on `PATH`:

```sh
decdrm tx tools/bench/drm_bench/station_iq.toml --output /tmp/drm_iq_15s.wav --duration 15
python3 tools/pluto/pluto_drm_tx.py --iq /tmp/drm_iq_15s.wav --lo 400e6 --gain -20 --seconds 600
```

Start the service and configure the SDR session before capture. Confirm an approximately 10 kHz signal at 400.1 MHz in the panadapter. Capture requires Python `aiohttp` and NumPy:

```sh
python3 tools/bench/drm_capture.py --seconds 10 --out /tmp/drm_live.f32 --json /tmp/drm_live.json
```

The tool subscribes to `/ws?iq=1&noaudio=1`; it does not configure or open the device. It saves interleaved little-endian float32 I/Q from IQBF wire v2 frames, plus rate, level and delivery metadata. It drains one second of queued frames before measuring. Captured frames must retain the first frame’s identity, center frequency, actual rate and delay. Invalid sizes, nonfinite samples, duplicate/missing sequences, stream resets and backwards known input clocks are rejected. The tool checks `--max-bytes` (512 MiB by default) before appending and retains only whole frames that fit; this bounds captured data, not total process RSS. Use the reported rate for offline decoding; the committed live fixture publishes 48828.125 Hz rather than the DRM30 core's 48 kHz. Capture levels and historical RF measurements are not calibration constants for a new session.

### Offline comparison

The committed capture and synthetic fixtures are described in the [fixture README](../../../tests/fixtures/drm/README.md). The native receiver fixture command is:

```sh
cargo test --release -j 1 --manifest-path dsp/core/Cargo.toml --test drm_receiver live_capture -- --test-threads=1
```

This command reads the committed fixture, not `/tmp/drm_live.f32`. Native codec builds require `pkg-config` and FDK-AAC >= 2.0.3 with DRM/USAC support; see [AAC fixtures](#aac-fixtures).

For an independent comparison of a new capture, supply an external Dream console binary with `--status-socket` support and the capture's actual published rate:

```sh
python3 tools/analysis/drm_oracle_check.py /tmp/drm_live.f32 48828.125 --dream-bin /path/to/dream
```

The oracle tool resamples to 48 kHz and normalizes before writing Dream's int16 IQ WAV. Dream and DecDRM are external prerequisites; their binaries and external build instructions are not supplied here.

### Audio interpretation

DRM audio must be deframed using its signalled codec configuration. With the text flag set, the final four bytes of each audio logical frame are text and must be removed before splitting access units; otherwise the reverse-read SBR payload is displaced. The historical HE-AAC capture uses five 208-byte access units per 400 ms frame and produces a 1 kHz tone at 24 kHz PCM.

The direct AAC-LC fixture has a 24 kHz core without SBR and outputs 24 kHz. The synthetic end-to-end AAC IQ fixture signals SBR over that core and therefore reports 48 kHz output. Use the decoder's returned sample rate and channel count, rather than assuming either rate from the fixture name. Streaming decoding also depends on persistent MSC deinterleaving and carrier correction across input blocks.

## AAC fixtures

[The fixture re-serializer](../../../tools/fixtures/drm_aac_reserialize.py) converts raw MPEG-4 GA AAC-LC single-channel, `ONLY_LONG` access units to DRM syntax. It implements VCB11 section data, HCR spectral reordering and CRC-8; it is not a general DRM audio encoder. The generator accepts only 24 kHz mono and skips other window sequences. The serializer rejects unsupported elements, prediction, pulse, TNS and gain control; this fixture path does not generate SBR. FDK-AAC provides a DRM decoder (`TT_DRM`), but no DRM encoder.

### Frame syntax

For the supported no-TNS/no-SBR fixture, the emitted access unit is:

```text
aac_crc_bits(8)
ics_info tns_data_present ltp_data_present global_gain
section_data scale_factor_data reordered_spectral_data_length(14)
longest_codeword_length(6) spectral_data(HCR)
```

- The GA `id_syn_ele` and element tag are omitted. Long-window `ics_info` contains reserved(1), window sequence(2), window shape(1), and `max_sfb`(6), without GA's `predictor_data_present` bit.
- `global_gain` follows the TNS/LTP flags. General DRM syntax can carry TNS data; the fixture serializer does not implement that payload.
- **VCB11 means virtual codebook 11, not 11-bit indices.** This long-window path writes 5-bit codebook indices. Codebook 11 and virtual codebooks 16–31 imply one band; other codebooks carry 5-bit section lengths with escape value 31. Virtual codebooks bound the largest absolute coefficient.
- CRC covers the emitted side information from `ics_info` through the HCR length fields, excluding spectral data. The implementation uses polynomial `0x1D`, initial register `0xFF`, MSB-first processing and a final ones' complement; the CRC byte precedes the payload for `TT_DRM`. General CRC boundaries follow the decoder's `drmRead_CrcStartReg`/`CrcEndReg` regions, including applicable TNS data.
- Real HE-AAC DRM frames carry SBR at the frame end, read in reverse bit order, with its own CRC. The no-SBR fixture omits it. Text bytes must be removed before audio deframing; see [bench workflow](#bench-workflow).

### HCR ordering

HCR is a segmented state machine, not a plain codebook sort:

- Priority is `11, 31..16` descending, then `9/10, 7/8, 5/6, 3/4, 1/2`.
- Maximum codeword lengths include sign and escape bits. Segment width is `min(aMaxCwLen[cb], longest_codeword_length)`.
- Priority codewords start the segments. Remaining codewords fill leftover space in sets, alternating direction per set. The length fields describe the reordered spectral payload and longest codeword.

### Fixture generation and codec builds

The [fixture README](../../../tests/fixtures/drm/README.md) owns the fixture metadata, verifier build command and recorded comparison. The direct fixture contains nine 36-byte DRM units paired with nine 320-byte GA units: a 1 kHz sine, AAC-LC, 24 kHz mono, 64 kbit/s, 960 samples per frame. Decoder priming accounts for the first two silent frames.

Generation needs external FDK GA output built with the 960-sample encoder patch described by DecDRM's `crates/decdrm-fdk-sys/build.rs` (`aacenc_lib.cpp`, `aacenc.cpp`, `psy_configuration.cpp`). Given raw access units as `*.bin` files:

```sh
python3 tools/fixtures/gen_drm_aac_fixture.py /path/to/ga_units /tmp/aac_sine_24k
```

This writes `.drm`, `.ga` and `.json`; non-`ONLY_LONG` frames are skipped. Metadata includes `verification_required`: separately run the real FDK verifier to compare `TT_DRM` and `TT_MP4_RAW` PCM and tone. Successful generation does not establish successful decoding. The source encoder and its patch are external prerequisites, not provided by this command.

The receiver's [build script](../../../dsp/core/build.rs) requires `pkg-config` and native FDK-AAC >= 2.0.3 with DRM/USAC support. Wasm builds link the committed FDK and libxaac archives; the standalone libxaac diagnostic is wasm-only. Native FDK handles AAC, HE-AAC and xHE-AAC/USAC. PCM consumers must use the decoder's returned sample rate and channel count: the direct no-SBR fixture outputs 24 kHz, while the synthetic IQ fixture signals SBR and outputs 48 kHz.

### References and licensing

- ISO/IEC 14496-3 §4.5.4.2.4 (HCR), §4.6.2 (Huffman tables); ETSI ES 201 980 §5.3.1.2 (DRM AAC framing).
- External FDK-AAC sources: `libAACdec/src/aacdec_hcr.cpp`, `aacdec_hufftab.h`, `aac_rom.cpp`; `libMpegTPDec/src/tpdec_drm.cpp` defines transport CRC regions. FDK-AAC has its own license.
- [DecDRM](https://github.com/CasualArclamp/DecDRM) is GPL-2.0-or-later. Its `FdkDrmEncoder`, `aac/drm.rs` and `aac/hcr.rs` are external algorithm references, not source to copy into the independent receiver. Generated fixture data and generator source have distinct provenance; the [fixture README](../../../tests/fixtures/drm/README.md#provenance-and-licence) owns that record.

The standards, external encoder patch and reference decoder sources are not vendored by this document. Codec regressions use fixtures matched to the implementation; real RF acceptance still requires separate evidence of the device, signal source, capture geometry and continuity.
