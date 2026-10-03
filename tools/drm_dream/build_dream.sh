#!/usr/bin/env bash
# Build the headless wwek/dream console receiver used as the background DRM
# decoder for this worktree.
#
# Why this fork and this build mode?
#   * The packaged `dream-nox` (AUR) builds wwek/dream with CONFIG+=qtconsole,
#     which does NOT define USE_CONSOLEIO, so `--status-socket` is parsed but the
#     CStatusBroadcast code is never compiled in.
#   * The upstream `dream` 2.3 package (Drm-tools/dream) is a Qt6 GUI build with
#     no status socket at all.
#   * `CONFIG+=console` (no Qt) is the documented headless mode; it includes
#     src/main.cpp -> CStatusBroadcast, giving the newline-delimited JSON status
#     stream we need for station label / mode / bitrate.
#
# Output: tools/drm_dream/build/dream (git-ignored).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="${DRM_DREAM_VERSION:-v2.2.4}"
REPO="${DRM_DREAM_REPO:-https://github.com/wwek/dream}"
SRC="${DRM_DREAM_SRC:-$HERE/.src}"
OUT="${1:-$HERE/build/dream}"

if [ ! -d "$SRC/.git" ]; then
    mkdir -p "$SRC"
    git clone --depth 1 --branch "$VERSION" "$REPO" "$SRC"
else
    git -C "$SRC" fetch --depth 1 origin "refs/tags/$VERSION:refs/tags/$VERSION" || true
    git -C "$SRC" checkout -q "$VERSION"
fi

cd "$SRC"
qmake CONFIG+=console CONFIG+=fdk-aac dream.pro
make -j"$(nproc)"

mkdir -p "$(dirname "$OUT")"
cp "$SRC/dream" "$OUT"
echo "built $OUT ($(git -C "$SRC" describe --tags))"
"$OUT" --version 2>&1 | head -3 || true
