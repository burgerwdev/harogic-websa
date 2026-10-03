# libxaac decoder FFI notes

The `libxaac.a` archive is linked, but the Rust FFI for live decoding is not yet
written. The decoder uses a command-based API (`ixheaacd_dec_api(obj, cmd, idx,
value)`); these are the extracted constants and the flow, so the FFI can be written
without re-deriving them.

## Command values (`ixheaacd_apicmd_standards.h`)

| Command | Value |
|---|---|
| `IA_API_CMD_GET_API_SIZE` | 0x0002 |
| `IA_API_CMD_INIT` | 0x0003 |
| `IA_API_CMD_SET_CONFIG_PARAM` | 0x0004 |
| `IA_API_CMD_GET_MEMTABS_SIZE` | 0x0006 |
| `IA_API_CMD_SET_MEMTABS_PTR` | 0x0007 |
| `IA_API_CMD_EXECUTE` | 0x0009 |
| `IA_API_CMD_GET_CURIDX_INPUT_BUF` | 0x000B |
| `IA_API_CMD_SET_INPUT_BYTES` | 0x000C |
| `IA_API_CMD_GET_OUTPUT_BYTES` | 0x000D |
| `IA_API_CMD_INPUT_OVER` | 0x000E |
| `IA_API_CMD_GET_MEM_INFO_SIZE` | 0x0011 |
| `IA_API_CMD_GET_MEM_INFO_ALIGNMENT` | 0x0012 |
| `IA_API_CMD_SET_MEM_PTR` | 0x0016 |

Init types: `PRE_CONFIG_PARAMS=0x0100`, `POST_CONFIG_PARAMS=0x0200`,
`INIT_PROCESS=0x0300`, `INIT_DONE_QUERY=0x0400`; execute types
`DO_EXECUTE=0x0100`, `DONE_QUERY=0x0200`.

Config params: `IA_XHEAAC_DEC_CONFIG_PARAM_PCM_WDSZ=0x0000`,
`IA_XHEAAC_DEC_CONFIG_PARAM_MP4FLAG=0x000C`.

`IA_ENHAACPDEC_NUM_MEMTABS = 4`; `ia_mem_info_struct` is 7×`u32` = 28 bytes, so
`GET_MEMTABS_SIZE` returns `(28+4)*4 = 128`.

## Decode flow

1. `GET_API_SIZE(NULL)` → size → `malloc`.
2. `INIT(PRE_CONFIG_PARAMS)`.
3. `SET_CONFIG_PARAM(MP4FLAG, &1)` (and `PCM_WDSZ, &16`).
4. `INIT(POST_CONFIG_PARAMS)`.
5. `GET_MEMTABS_SIZE` → 128 → `malloc` → `SET_MEMTABS_PTR`.
6. For each of 4 memtabs: `GET_MEM_INFO_SIZE(i)`/`GET_MEM_INFO_ALIGNMENT(i)` →
   `malloc` → `SET_MEM_PTR(i, ptr)`.
7. Feed the ASC: copy it into the input memtab, `SET_INPUT_BYTES(asc_len)`,
   `INIT(INIT_PROCESS)`, `INIT(INIT_DONE_QUERY, &done)` until done.
8. Per access unit: copy into the input memtab, `SET_INPUT_BYTES(au_len)`,
   `EXECUTE(DO_EXECUTE)`, `GET_OUTPUT_BYTES` → PCM byte count, read PCM from the
   output memtab.
9. `INPUT_OVER` at the end.

The native `xaacdec` testbench is the reference: it decodes the mp4 bitstream +
its `-dec_info_init` ASC and per-frame `-ia_mp4_stsz_size` entries from the
metadata file. A USAC test vector is generated natively by `xaacenc
-ifile:tone.wav -ofile:out.bin -br:96000 -usac:1 -adts:0` (the encoder is
restricted to 64/96 kbps USAC) and decoded by `xaacdec -ifile:out.bin
-imeta:out.txt -ofile:pcm -mp4:1`, which was verified to produce non-silent PCM.

## Status

`wasm/src/xaac.rs` implements the flow above and links; the `websa_dsp_xaac_decode`
smoke export builds but traps (`unreachable`) on the second access unit. The
generic path is now **fixed and natively verified** (see the blocker section), so the
remaining trap is a Rust-side panic in the FFI layer, not the decoder.

## Blocker: the generic (portable C) decode path was broken upstream — now fixed

`libxaac`'s SIMD paths (x86/x86_64/armv7/armv8) are maintained; the portable
`decoder/generic/` path (the only one usable for wasm32) was stale. Three fixes to
the libxaac source make the generic path decode correctly (verified with a native
repro that decodes all 45 USAC frames to non-silent PCM, energy 232M):

1. Six function-pointer signature mismatches in
   `decoder/generic/ixheaacd_function_selector_generic.c` (wrong parameter types vs
   the current headers; fixed in the clone).
2. Null-pointer arithmetic UB in `decoder/ixheaacd_sbrdecoder.c:141`
   (`&lpc_filt_states_real[1][old_lsb]` on a NULL pointer) — guarded with
   `if (new_lsb > old_lsb)`.
3. Left-shift-of-negative UB in `decoder/generic/ixheaacd_qmf_dec_generic.c`
   (`<< RADIXSHIFT` on a negative value) — replaced with `* 2`.

The fixes live in the external `~/git/libxaac` clone; this archive is rebuilt with
`wasm/xaac/build.sh ~/git/libxaac`. After these, the native generic build decodes
cleanly, but the wasm32 build still traps with `unreachable` inside the Rust FFI on
the second access unit (a Rust panic in `websa_dsp_xaac_decode` / `XaacDecoder::feed`,
not a C-decoder fault).
