#!/bin/bash
# Build the DSP core for wasm32 and publish it as the committed browser artifact.
#
#   wasm/build.sh           build + copy to frontend/public/dsp.wasm + record the manifest
#   wasm/build.sh --check    rebuild and fail if the committed artifact differs (release gate)
#
# The artifact is committed so that CI and a machine without Rust can still build the frontend
# (`./build.sh` never calls cargo). The manifest records the hash plus the toolchain that
# produced it, and tools/checks/check_wasm_artifact.py verifies both without Rust.
set -euo pipefail
cd "$(dirname "$0")"

CHECK=0
[ "${1:-}" = "--check" ] && CHECK=1

TARGET=wasm32-unknown-unknown
LIB=websa_dsp
OUT="target/$TARGET/release/$LIB.wasm"
DST=../frontend/public/dsp.wasm
MANIFEST=dsp.artifact.json

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found: install Rust and 'rustup target add $TARGET'" >&2
  exit 1
fi
if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
  echo "target $TARGET is not installed: run 'rustup target add $TARGET'" >&2
  exit 1
fi

echo "== cargo test (kernels, native) =="
cargo test --quiet

echo "== cargo build --release --target $TARGET =="
cargo build --release --target "$TARGET"

if [ ! -f "$OUT" ]; then
  echo "build produced no $OUT" >&2
  exit 1
fi

sha() { sha256sum "$1" | cut -d' ' -f1; }

if [ "$CHECK" = "1" ]; then
  if [ ! -f "$DST" ]; then
    echo "committed artifact $DST is missing; run wasm/build.sh" >&2
    exit 1
  fi
  if ! cmp -s "$OUT" "$DST"; then
    echo "committed $DST differs from a fresh build:" >&2
    echo "  fresh     $(sha "$OUT")  $(stat -c%s "$OUT") bytes" >&2
    echo "  committed $(sha "$DST")  $(stat -c%s "$DST") bytes" >&2
    echo "run wasm/build.sh to republish it" >&2
    exit 1
  fi
  echo "wasm artifact OK: $DST matches a fresh build ($(sha "$DST"))"
  exit 0
fi

cp "$OUT" "$DST"
python3 - "$OUT" "$MANIFEST" "$TARGET" <<'PY'
import hashlib, json, pathlib, subprocess, sys

out, manifest, target = sys.argv[1], sys.argv[2], sys.argv[3]
data = pathlib.Path(out).read_bytes()
# The export list is read from the binary, not hardcoded: a new ABI function that the manifest
# never mentions would otherwise pass the CI check (which verifies the manifest is a subset).
sys.path.insert(0, str(pathlib.Path('..') / 'tools'))
from check_wasm_artifact import wasm_exports  # noqa: E402
document = {
    'note': 'Written by wasm/build.sh. The .wasm is committed so the frontend build needs no '
            'Rust; tools/checks/check_wasm_artifact.py verifies this manifest in CI.',
    'file': 'frontend/public/dsp.wasm',
    'sha256': hashlib.sha256(data).hexdigest(),
    'bytes': len(data),
    'target': target,
    'profile': 'release',
    'rustc': subprocess.run(['rustc', '--version'], capture_output=True, text=True).stdout.strip(),
    'cargo': subprocess.run(['cargo', '--version'], capture_output=True, text=True).stdout.strip(),
    # The exports the loader calls, read from the binary itself.
    'exports': sorted(wasm_exports(data)),
}
pathlib.Path(manifest).write_text(json.dumps(document, indent=1, sort_keys=True) + '\n')
print(f"published {out} -> {pathlib.Path(out).parent}/../.. "
      f"({len(data)} bytes, {document['sha256'][:16]}...)")
PY

ls -l "$DST"
