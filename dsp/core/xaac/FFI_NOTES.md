# libxaac decoder FFI notes

`dsp/core/src/xaac.rs` wraps the prebuilt archive for optional WASM diagnostics.
Native production xHE-AAC uses its separate FDK decoder integration. libxaac uses
a command-based API (`ixheaacd_dec_api(obj, cmd, idx, value)`); the wrapper owns
each allocation and its exact layout through initialization, feeding and release.

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
| `IA_API_CMD_GET_MEM_INFO_TYPE` | 0x0013 |
| `IA_API_CMD_SET_MEM_PTR` | 0x0016 |

Init types: `PRE_CONFIG_PARAMS=0x0100`, `POST_CONFIG_PARAMS=0x0200`,
`INIT_PROCESS=0x0300`, `INIT_DONE_QUERY=0x0400`; execute types
`DO_EXECUTE=0x0100`, `DONE_QUERY=0x0200`.

Config params: `IA_XHEAAC_DEC_CONFIG_PARAM_PCM_WDSZ=0x0000`,
`IA_XHEAAC_DEC_CONFIG_PARAM_MP4FLAG=0x000C`.

`IA_ENHAACPDEC_NUM_MEMTABS = 4`; `ia_mem_info_struct` is 7×`u32` = 28 bytes, so
`GET_MEMTABS_SIZE` returns `(28+4)*4 = 128`.

## Decode flow

1. `GET_API_SIZE(NULL)` → checked size → owned allocation.
2. `INIT(PRE_CONFIG_PARAMS)`.
3. `SET_CONFIG_PARAM(MP4FLAG, &1)` (and `PCM_WDSZ, &16`).
4. `GET_MEMTABS_SIZE` → checked allocation → `SET_MEMTABS_PTR`.
5. `INIT(POST_CONFIG_PARAMS)` fills the registered memory-table descriptor.
6. For each of 4 memtabs: query size, alignment and type, allocate with a valid
   power-of-two alignment, then `SET_MEM_PTR(i, ptr)`. Require one input and one
   output table; retain all allocations for cleanup on any failed SDK command.
7. Feed one complete ASC within input capacity, `SET_INPUT_BYTES(asc_len)`,
   `INIT(INIT_PROCESS)`, then require `INIT_DONE_QUERY` to report completion.
8. Per complete access unit: check input capacity before copying, set its size,
   execute, and query output size. Accept only an even PCM16 byte count within
   output capacity before reading the output table.
9. `INPUT_OVER` at the end.

The upstream `xaacdec` testbench decodes the mp4 bitstream with its
`-dec_info_init` ASC and per-frame `-ia_mp4_stsz_size` entries from the metadata
file. A USAC test vector can be generated natively by `xaacenc
-ifile:tone.wav -ofile:out.bin -br:96000 -usac:1 -adts:0` (the encoder is
restricted to 64/96 kbps USAC) and decoded by `xaacdec -ifile:out.bin
-imeta:out.txt -ofile:pcm -mp4:1`.

## Verification scope

`src/xaac_tests.rs` injects SDK responses and allocation failures without linking
the native archive. It checks partial-initialization cleanup, exact deallocation
layouts, required table identities, input/output bounds and command errors.
Those tests do not validate the portable C decoder or a rebuilt WASM artifact.
Run the committed USAC fixture against that artifact to check actual decoding.

`build.sh` consumes a caller-provided source checkout and does not fetch a pinned
revision or apply source patches. The committed archive therefore cannot be
reproduced merely by cloning the current upstream branch. Record the source
revision, any local patches and resulting archive hash when rebuilding it.
The WASM `longjmp` shim traps; malformed-bitstream recovery in upstream C is not
isolated by the Rust allocation and result-size checks.
