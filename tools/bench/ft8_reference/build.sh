#!/bin/sh
# Build the reference FT8 decoder (ft8_lib) and run it on a raw 16-bit PCM file.
#
# The point of having it here: it is the implementation an Android FT8 app (and WSJT-X) uses, so it is
# the gold standard to compare our decoder against on the *same* input. Our own decoder finds a real
# signal's sync but never converges, and guessing at the difference cost a lot of time - the reference
# answers it directly.
#
#   ffmpeg -y -i capture.wav -ac 1 -ar 48000 -f s16le /tmp/audio.raw
#   tools/bench/ft8_reference/build.sh /tmp/audio.raw 4 2      # time_osr, freq_osr
set -e
here=$(cd "$(dirname "$0")" && pwd)
src="$here/ft8_lib"
if [ ! -d "$src" ]; then
    mkdir -p "$src"
    for f in common/common.h common/monitor.c common/monitor.h common/wave.c common/wave.h \
             common/audio.h ft8/constants.c ft8/constants.h ft8/crc.c ft8/crc.h ft8/debug.h \
             ft8/decode.c ft8/decode.h ft8/ldpc.c ft8/ldpc.h ft8/message.c ft8/message.h \
             ft8/text.c ft8/text.h fft/kiss_fft.c fft/kiss_fft.h fft/kiss_fftr.c fft/kiss_fftr.h \
             fft/_kiss_fft_guts.h; do
        mkdir -p "$src/$(dirname "$f")"
        curl -sSfL -o "$src/$f" "https://raw.githubusercontent.com/kgoba/ft8_lib/master/$f"
    done
fi
cc -O2 -o "$here/decode_ref" "$here/main.c" "$src/common/monitor.c" "$src/common/wave.c" \
   "$src/ft8/constants.c" "$src/ft8/crc.c" "$src/ft8/decode.c" "$src/ft8/ldpc.c" \
   "$src/ft8/message.c" "$src/ft8/text.c" "$src/fft/kiss_fft.c" "$src/fft/kiss_fftr.c" \
   -I"$src" -I"$src/ft8" -I"$src/fft" -I"$src/common" -lm 2>/dev/null
if [ $# -gt 0 ]; then
    "$here/decode_ref" "$@"
fi
