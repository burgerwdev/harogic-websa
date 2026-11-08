#!/bin/sh
# Build the pinned independent FT8 decoder and optionally read a mono PCM WAV.
# Sources and licenses are cached outside the repository; each build is cleaned.
# Usage: tools/bench/ft8_reference/build.sh capture.wav 4 4 48000
set -eu
here=$(cd "$(dirname "$0")" && pwd)
revision=9fec6ca39886edbf96f4f5e71edc76da5074e871
digest=22b5f2f8c8b328e44d34f63a6eeec888bda7e7c62b08e96ba6624503ff582bd8
cache="${WEBSA_FT8_REFERENCE_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/harogic-websa/ft8-reference}/$revision"
mkdir -p "$cache"
work=$(mktemp -d "$cache/.build.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
archive="$cache/source.tar.gz"
if [ ! -f "$archive" ]; then
    curl --fail --location --silent --show-error --proto '=https' --proto-redir '=https' \
        "https://codeload.github.com/kgoba/ft8_lib/tar.gz/$revision" -o "$work/source.tar.gz"
    printf '%s  %s\n' "$digest" "$work/source.tar.gz" | sha256sum --check --status
    mv "$work/source.tar.gz" "$archive"
fi
printf '%s  %s\n' "$digest" "$archive" | sha256sum --check --status
src="$work/source"
mkdir "$src"
tar -xzf "$archive" --strip-components=1 -C "$src"
# Preserve the complete upstream archive and an accessible license alongside it.
cp "$src/LICENSE" "$cache/LICENSE"
printf 'Source: https://github.com/kgoba/ft8_lib/tree/%s\nArchive SHA256: %s\n' \
    "$revision" "$digest" > "$cache/SOURCE.txt"
"${CC:-cc}" -O2 -DHAVE_STPCPY -o "$work/decode_ref" "$here/main.c" "$src/common/monitor.c" "$src/common/wave.c" \
   "$src/ft8/constants.c" "$src/ft8/crc.c" "$src/ft8/decode.c" "$src/ft8/ldpc.c" \
   "$src/ft8/message.c" "$src/ft8/text.c" "$src/fft/kiss_fft.c" "$src/fft/kiss_fftr.c" \
   -I"$src" -I"$src/ft8" -I"$src/fft" -I"$src/common" -lm
if [ "$#" -gt 0 ]; then
    "$work/decode_ref" "$@"
fi
