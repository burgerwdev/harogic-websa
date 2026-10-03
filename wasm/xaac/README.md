# libxaac (xHE-AAC / MPEG-D USAC) decoder (prebuilt for wasm32)

`libxaac.a` is the Ittiam libxaac decoder, compiled to a `wasm32-unknown-unknown`
static archive and linked into `websa-dsp` only for the wasm32 target.

## Provenance and licence

- Source: <https://github.com/ittiam-systems/libxaac>, licence **Apache-2.0**.
- This archive covers the **decoder** only (`decoder/` + `common/`). The encoder is
  used natively as a test-vector generator, not linked into the crate.

## Building

`libxaac.a` is committed so the crate builds without a C toolchain. To regenerate:

```sh
git clone https://github.com/ittiam-systems/libxaac.git
wasm/xaac/build.sh /path/to/libxaac
```

The build provides minimal stubs for the hosted libc headers libxaac includes
(`<string.h>`, `<math.h>`, `<stdlib.h>`, `<setjmp.h>`, …): `malloc`/`free` come from
Rust, `memcpy`/`memset` from compiler-builtins, the math functions from Rust's
vendored libm, and `setjmp`/`longjmp` are stubbed to "no error" / trap (libxaac uses
them only for bitstream-error recovery).
