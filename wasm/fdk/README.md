# FDK AAC decoder (prebuilt for wasm32)

`libfdk.a` is the Fraunhofer FDK AAC decoder, compiled to a `wasm32-unknown-unknown`
static archive and linked into `websa-dsp` only for the wasm32 target (`wasm/build.rs`
emits the link flags; the native `cargo test` build does not link it).

## Provenance and licence

- Source: <https://github.com/mstorsjo/fdk-aac> (Fraunhofer FDK AAC Codec Library for
  Android), licence: the Fraunhofer FDK AAC licence (a permissive redistribution
  licence; **no patent licence is granted** — see `NOTICE`/`MODULE_LICENSE_FRAUNHOFER`
  in the source). The full licence text is reproduced in that source tree.
- This archive covers the **decoder** receive path only (AAC-LC / HE-AAC / HE-AAC v2).
  It does **not** decode xHE-AAC (USAC); that is a separate codec (libxaac).

## Building

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

`wasm/src/fdk.rs` (gated to `target_arch = "wasm32"`) provides the malloc family on
top of Rust's global allocator and the `aacDecoder_*` FFI surface as an
`AacDecoder` (`new()` → `decode(access_unit) → Vec<i16>`).
