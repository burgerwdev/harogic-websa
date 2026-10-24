# DRM AAC re-serialization (MPEG-4 → DRM syntax)

[The fixture re-serializer](../../tools/fixtures/drm_aac_reserialize.py) converts raw MPEG-4 GA AAC-LC single-channel, `ONLY_LONG` access units to DRM syntax. It implements VCB11 section data, HCR spectral reordering and CRC-8; it is not a general DRM audio encoder. The generator skips other window sequences, and this fixture path does not generate SBR. FDK-AAC provides a DRM decoder (`TT_DRM`), but no DRM encoder.

## Frame syntax

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
- Real HE-AAC DRM frames carry SBR at the frame end, read in reverse bit order, with its own CRC. The no-SBR fixture omits it. Text bytes must be removed before audio deframing; see [bench audio notes](DRM_BENCH.md).

## HCR ordering

HCR is a segmented state machine, not a plain codebook sort:

- Priority is `11, 31..16` descending, then `9/10, 7/8, 5/6, 3/4, 1/2`.
- Maximum codeword lengths include sign and escape bits. Segment width is `min(aMaxCwLen[cb], longest_codeword_length)`.
- Priority codewords start the segments. Remaining codewords fill leftover space in sets, alternating direction per set. The length fields describe the reordered spectral payload and longest codeword.

## Fixture generation and codec builds

The [fixture README](../../tests/fixtures/drm/README.md) owns the fixture metadata, verifier build command and recorded comparison. The direct fixture contains nine 36-byte DRM units paired with nine 320-byte GA units: a 1 kHz sine, AAC-LC, 24 kHz mono, 64 kbit/s, 960 samples per frame. Decoder priming accounts for the first two silent frames.

Generation needs external FDK GA output built with the 960-sample encoder patch described by DecDRM's `crates/decdrm-fdk-sys/build.rs` (`aacenc_lib.cpp`, `aacenc.cpp`, `psy_configuration.cpp`). Given raw access units as `*.bin` files:

```sh
python3 tools/fixtures/gen_drm_aac_fixture.py /path/to/ga_units /tmp/aac_sine_24k
```

This writes `.drm`, `.ga` and `.json`; non-`ONLY_LONG` frames are skipped. The source encoder and its patch are external prerequisites, not provided by this command.

The receiver's [build script](../../wasm/build.rs) requires `pkg-config` and native FDK-AAC >= 2.0.3 with DRM/USAC support. Wasm builds link the committed FDK and libxaac archives; the standalone libxaac diagnostic is wasm-only. Native FDK handles AAC, HE-AAC and xHE-AAC/USAC. PCM consumers must use the decoder's returned sample rate and channel count: the direct no-SBR fixture outputs 24 kHz, while the synthetic IQ fixture signals SBR and outputs 48 kHz.

## References and licensing

- ISO/IEC 14496-3 §4.5.4.2.4 (HCR), §4.6.2 (Huffman tables); ETSI ES 201 980 §5.3.1.2 (DRM AAC framing).
- External FDK-AAC sources: `libAACdec/src/aacdec_hcr.cpp`, `aacdec_hufftab.h`, `aac_rom.cpp`; `libMpegTPDec/src/tpdec_drm.cpp` defines transport CRC regions. FDK-AAC has its own license.
- [DecDRM](https://github.com/CasualArclamp/DecDRM) is GPL-2.0-or-later. Its `FdkDrmEncoder`, `aac/drm.rs` and `aac/hcr.rs` are external algorithm references, not source to copy into the independent receiver. Generated fixture data and generator source have distinct provenance; the [fixture README](../../tests/fixtures/drm/README.md#provenance-and-licence) owns that record.

The standards, external encoder patch and reference decoder sources are not vendored by this document. These notes describe the implementation and existing fixture record, not a new codec or hardware verification run.
