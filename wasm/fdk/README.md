# FDK AAC decoder (native and wasm32)

`libfdk.a` is the Fraunhofer FDK AAC decoder compiled to a `wasm32-unknown-unknown`
static archive. Native builds link the hosted `fdk-aac` library using `pkg-config`;
the wasm archive is never used as a native library.

## Provenance and licence

- Source: <https://github.com/mstorsjo/fdk-aac> (Fraunhofer FDK AAC Codec Library for
  Android), licence: the Fraunhofer FDK AAC licence (a permissive redistribution
  licence; **no patent licence is granted** — see `NOTICE`/`MODULE_LICENSE_FRAUNHOFER`
  in the source). The full licence text is reproduced in that source tree.
- The live receiver uses FDK's `TT_DRM` decoder for AAC-LC, HE-AAC, HE-AAC v2 and
  xHE-AAC/USAC. FDK 2.0.3 supports the DRM type-9 xHE static configuration and USAC
  access units; libxaac remains a separate wasm diagnostic decoder.

## Building

Native builds require `pkg-config` and FDK AAC **2.0.3 or newer**, with the DRM and
USAC decoder enabled and 16-bit `INT_PCM`. On Arch Linux, install `pkgconf` and
`libfdk-aac`. For another installation prefix, set `PKG_CONFIG_PATH` to its
`lib/pkgconfig` directory. `PKG_CONFIG`, `PKG_CONFIG_LIBDIR`, and
`PKG_CONFIG_SYSROOT_DIR` are also honored by the build script. There is no runtime
WASM dependency or additional Rust build dependency.

```sh
pkg-config --atleast-version=2.0.3 fdk-aac
pkg-config --libs fdk-aac
cargo test --manifest-path wasm/Cargo.toml -j1 --lib native_audio_fixtures_decode_with_codec_format -- --nocapture
```

The native fixture regression checks non-silent AAC, DRM+, xHE-AAC and HE-AAC v2
PCM, and the actual codec rate and channel count. It uses committed IQ fixtures;
DRM+ is synthetic rather than an off-air VHF recording.

`libfdk.a` is committed so the crate builds without a C++ toolchain. To regenerate:

```sh
git clone https://github.com/mstorsjo/fdk-aac.git
wasm/fdk/build.sh /path/to/fdk-aac
```

The build compiles the decoder sources with
`clang++ --target=wasm32-unknown-unknown -ffreestanding -fno-exceptions …` and adds a
tiny runtime shim: `malloc`/`free`/`calloc`/`realloc` are provided by Rust
(`wasm/src/fdk.rs`), `memcpy`/`memset`/`memmove`/`memcmp` by Rust's compiler-builtins,
and the shim supplies `operator new/delete` plus `strlen`/`strcpy`/`strcmp`.

## Rust side

`wasm/src/fdk.rs` provides the shared `AacDecoder` (`new()` → `configure(type9)` →
`decode(access_unit) → Vec<i16>`) and the last decoded stream format. Its allocator
shims are restricted to wasm32 so native decoding uses the hosted C runtime.
The native receiver drains `DecodedAudioChunk` segments in order. Each segment
retains its actual i16 interleaved PCM rate, channels, codec delay and IQ provenance.
A format change retains the previous segment. `audio_rate_hz()` and
`audio_channels()` describe the compatibility drain's current segment. The wasm
receiver retains its mono diagnostic output. DRM+ AAC's 48 kHz rate code is
translated at the FDK configuration boundary to its DRM transport code.

`audio_codec_delay_samples()` reads FDK's `CStreamInfo.outputDelay` in decoded PCM
samples. `audio_source_position()` returns the earliest contributing baseband IQ
index as `(index, 0, 1)` for buffered PCM. This provenance follows the actual FFT
windows, channel-estimator history, cell permutation and deinterleaver memory;
xHE frames preserve the minimum origin across every super frame they span.
`audio_total_delay_samples()` reports the consumed-input boundary minus that causal
origin, plus the actual codec delay converted to baseband samples. This is protocol
provenance and does not imply knowledge of the transmitter's PCM presentation phase.
Draining PCM removes its provenance at access-unit boundaries without interpolating
a fabricated IQ position between audio samples.

Direct codec content and malformed-input regressions:

```sh
cargo test --manifest-path wasm/Cargo.toml -j1 --lib fdk::tests -- --nocapture
```
