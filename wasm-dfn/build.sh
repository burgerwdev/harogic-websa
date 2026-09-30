#!/bin/bash
# Build the DeepFilterNet3 noise-reduction runtime (upstream `libDF`, tract ONNX runtime) to WASM
# and publish it as the committed browser artifact.
#
#   wasm-dfn/build.sh             build + copy to frontend/modern/public/dfn/{df_bg.wasm,df.js}
#   wasm-dfn/build.sh --check     rebuild and fail if the committed artifact differs (release gate)
#
# This is the author's own streaming runtime (tract's PulsedModel + SimpleState handle the conv
# lookahead and the GRU state correctly, one `df_process_frame` per 480-sample hop). The model is
# NOT embedded: it is fetched at runtime and passed to `df_create(model_bytes)` (see `src/wasm.rs`).
# The `wasm` feature was edited to drop `default-model` so the DFN3 weights stay out of the binary.
set -euo pipefail
cd "$(dirname "$0")"

CHECK=0
[ "${1:-}" = "--check" ] && CHECK=1

DST=../frontend/modern/public/dfn
OUT=pkg/df_bg.wasm

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "wasm-pack not found: install with 'cargo install wasm-pack' (or the prebuilt binary)" >&2
  exit 1
fi
if ! rustup target list --installed 2>/dev/null | grep -qx "wasm32-unknown-unknown"; then
  echo "target wasm32-unknown-unknown not installed: 'rustup target add wasm32-unknown-unknown'" >&2
  exit 1
fi

echo "== wasm-pack build (release, wasm feature, no embedded model) =="
wasm-pack build . --target web --features wasm

if [ ! -f "$OUT" ]; then
  echo "build produced no $OUT" >&2
  exit 1
fi

if [ "$CHECK" = "1" ]; then
  if ! cmp -s "$OUT" "$DST/df_bg.wasm"; then
    echo "committed $DST/df_bg.wasm differs from a fresh build; run wasm-dfn/build.sh" >&2
    exit 1
  fi
  echo "dfn wasm artifact OK: $DST/df_bg.wasm matches a fresh build"
  exit 0
fi

cp "$OUT" pkg/df.js "$DST/"
ls -l "$DST"
echo "published -> $DST"
