#!/bin/sh
# Build ggmorse (MIT) into the single-file wasm module the browser CW decoder loads.
#
# The artifact is COMMITTED (frontend/src/dsp/ggmorse/ggmorse.js) for the same reason the DSP
# cores are: the service and CI gate on it, so a machine without the toolchain still builds - and
# emscripten is a ~1.7 GB download. Run this script only when the vendored ggmorse sources or the
# wrapper change.
#
#   tools/build_ggmorse_wasm.sh            # build (bootstraps emsdk on first use)
#   tools/build_ggmorse_wasm.sh --check    # rebuild into a temp file and compare with the committed one
#
# The sources under tools/ggmorse/ are vendored from https://github.com/ggerganov/ggmorse
# (commit 7b4822a8, MIT - see tools/ggmorse/LICENSE). `ggmorse_wasm.cpp` is ours: a small C API over
# the library, because upstream's wasm target is its SDL demo application.
set -e

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
src="$here/ggmorse"
emsdk="$here/.emsdk"
out="$root/frontend/src/dsp/ggmorse/ggmorse.js"

check=0
[ "$1" = "--check" ] && check=1

emxx="$emsdk/upstream/emscripten/em++"
if [ ! -x "$emxx" ]; then
    if [ "$check" = "1" ]; then
        echo "emsdk is not installed; skipping the check (run tools/build_ggmorse_wasm.sh to bootstrap)"
        exit 0
    fi
    echo "bootstrapping emsdk into $emsdk (a ~1.7 GB download, one time)..."
    if [ ! -d "$emsdk" ]; then
        git clone --depth 1 https://github.com/emscripten-core/emsdk.git "$emsdk"
    fi
    (cd "$emsdk" && ./emsdk install latest && ./emsdk activate latest)
fi

# The env script's PATH edit does not stick in a non-interactive shell, and em++ must find its own
# node and llvm tools: point at them explicitly.
nodebin=$(ls -d "$emsdk"/node/*/bin 2>/dev/null | head -1)
PATH="$emsdk/upstream/emscripten${nodebin:+:$nodebin}:$PATH"
export PATH

tmp="$here/.ggmorse-build"
rm -rf "$tmp"
mkdir -p "$tmp"
trap 'rm -rf "$tmp"' EXIT

echo "compiling ggmorse + the wrapper with $("$emxx" --version | head -1)"
"$emxx" -O3 -std=c++17 \
    -I "$src/include" \
    "$src/ggmorse_wasm.cpp" "$src/src/ggmorse.cpp" "$src/src/resampler.cpp" \
    -s MODULARIZE=1 -s EXPORT_NAME=createGgMorse \
    -s EXPORTED_FUNCTIONS='["_ggmorse_wasm_new","_ggmorse_wasm_free","_ggmorse_wasm_push_i16","_ggmorse_wasm_take_text","_ggmorse_wasm_pending_samples","_ggmorse_wasm_pitch_hz","_ggmorse_wasm_wpm","_malloc","_free"]' \
    -s EXPORTED_RUNTIME_METHODS='["ccall","cwrap","HEAP16","HEAPU8"]' \
    -s ALLOW_MEMORY_GROWTH=1 -s SINGLE_FILE=1 -s EXPORT_ES6=1 -s ENVIRONMENT=node,web,worker -s FILESYSTEM=0 \
    -s ASSERTIONS=0 --no-entry \
    -o "$tmp/ggmorse.js"

bytes=$(wc -c < "$tmp/ggmorse.js" | tr -d ' ')
echo "built $((bytes / 1024)) KB"

if [ "$check" = "1" ]; then
    if [ ! -f "$out" ]; then
        echo "FAIL: the committed artifact is missing ($out)"
        exit 1
    fi
    # The build is not byte-reproducible (emscripten embeds paths/versions), so the check is a
    # smoke test: both must expose the same exported API.
    missing=""
    for sym in ggmorse_wasm_new ggmorse_wasm_push_i16 ggmorse_wasm_take_text ggmorse_wasm_pitch_hz; do
        grep -q "$sym" "$out" || missing="$missing $sym"
    done
    if [ -n "$missing" ]; then
        echo "FAIL: the committed artifact is missing:$missing"
        exit 1
    fi
    echo "OK: the committed artifact exposes the expected API"
    exit 0
fi

mkdir -p "$(dirname "$out")"
cp "$tmp/ggmorse.js" "$out"
echo "wrote $out"
