#!/usr/bin/env bash
set -euo pipefail
script_path="$(realpath -- "${BASH_SOURCE[0]}")"
script_dir="$(dirname -- "$script_path")"
companion_dir="$script_dir/companions/dtmb"
[[ -d "$companion_dir" ]] || companion_dir="$script_dir"
prefix="${WEBSA_DECODERS_PREFIX:-${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders}"
while (($#)); do
  case "$1" in
    --prefix) prefix="${2:?}"; shift 2 ;;
    --help|-h) printf '%s\n' 'Usage: tools/setup-dtmb-player.sh [--prefix DIR]' 'Builds FFmpeg 7.1.2 with AVS+/DRA decoding and H.264/AAC fragmented MP4 output. Requires C compiler, make, pkg-config, x264, curl, git, perl and xz. Builds a temporary NASM on x86 if needed.'; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done
[[ -n "$prefix" ]]
pkg-config --exists x264
mkdir -p "$prefix/bin" "$prefix/sources" "$prefix/share/licenses/websa-dtmb-ffmpeg"
prefix="$(realpath -- "$prefix")"
build_dir="$(mktemp -d "$prefix/sources/dtmb-player.XXXXXX")"
trap 'rm -rf -- "$build_dir"' EXIT
case "$(uname -m)" in
  x86_64|i?86)
    if ! command -v nasm >/dev/null; then
      curl -fL --retry 3 https://www.nasm.us/pub/nasm/releasebuilds/2.16.03/nasm-2.16.03.tar.xz -o "$build_dir/nasm.tar.xz"
      (cd "$build_dir" && printf '%s\n' '1412a1c760bbd05db026b6c0d1657affd6631cd0a63cddb6f73cc6d4aa616148  nasm.tar.xz' | sha256sum -c -)
      tar -xf "$build_dir/nasm.tar.xz" -C "$build_dir"
      (cd "$build_dir/nasm-2.16.03" && ./configure && make -j 2 nasm)
      export PATH="$build_dir/nasm-2.16.03:$PATH"
    fi ;;
esac
for filename in ffmpeg.tar.xz avs.patch; do
  if [[ -f "$script_dir/$filename" ]]; then
    cp "$script_dir/$filename" "$build_dir/"
  elif [[ -f "$prefix/share/licenses/websa-dtmb-ffmpeg/$filename" ]]; then
    cp "$prefix/share/licenses/websa-dtmb-ffmpeg/$filename" "$build_dir/"
  fi
done
[[ -f "$build_dir/ffmpeg.tar.xz" ]] || curl -fL --retry 3 https://ffmpeg.org/releases/ffmpeg-7.1.2.tar.xz -o "$build_dir/ffmpeg.tar.xz"
[[ -f "$build_dir/avs.patch" ]] || curl -fL --retry 3 https://raw.githubusercontent.com/maliwen2015/ffmpeg_cavs_dra/eb94ceb887b47b6ecfbda9da43273dd65246df1e/ffmpeg-7.1.2_cavs_dra.patch -o "$build_dir/avs.patch"
cd "$build_dir"
printf '%s\n' '089bc60fb59d6aecc5d994ff530fd0dcb3ee39aa55867849a2bbc4e555f9c304  ffmpeg.tar.xz' '1d1cac53cc2d19af39de854db186985fb653e820b0d071844a1e529550c08e40  avs.patch' | sha256sum -c -
tar -xf ffmpeg.tar.xz
cd ffmpeg-7.1.2
# Recount the upstream patch's incorrect added-file hunk lengths.
git apply --recount -p2 ../avs.patch
perl -pi -e 's/\r\n/\n/g' libavcodec/libcavs.c libavcodec/libcavsdec.c
git apply "$companion_dir/player-threads.patch"
hardware=()
optimization=()
if "${CC:-cc}" -march=native -x c -c /dev/null -o ../native.o 2>/dev/null; then
  optimization+=(--extra-cflags=-march=native)
fi
if pkg-config --exists libva libva-drm; then
  hardware+=(--enable-vaapi --enable-encoder=h264_vaapi --enable-filter=hwupload,scale_vaapi,deinterlace_vaapi)
fi
./configure --disable-everything --disable-doc --disable-debug --disable-avdevice --disable-network \
  --disable-autodetect --enable-gpl --enable-libx264 \
  "${hardware[@]}" "${optimization[@]}" --enable-decoder=cavs,dra,mp3float,mp2float,aac,aac_latm,ac3,eac3,h264,hevc,mpeg2video,rawvideo \
  --enable-parser=cavsvideo,mpegaudio,aac,aac_latm,ac3,h264,hevc,mpegvideo \
  --enable-demuxer=mpegts,rawvideo --enable-encoder=libx264,aac --enable-muxer=mp4,mpegts \
  --enable-protocol=file,pipe --enable-filter=aresample,scale,format,fps,yadif \
  --extra-version=websa-dtmb-avsplus1
# FFmpeg disables GCC vectorization globally; the added C-only AVS+ decoder benefits from it.
if "${CC:-cc}" -ftree-vectorize -x c -c /dev/null -o ../vector.o 2>/dev/null; then
  printf '\nlibavcodec/libcavs.o: CFLAGS += -ftree-vectorize\n' >> Makefile
fi
make -j 2 ffmpeg
install -m755 ffmpeg "$prefix/bin/websa-dtmb-ffmpeg"
license_dir="$prefix/share/licenses/websa-dtmb-ffmpeg"
install -m644 COPYING.GPLv2 COPYING.GPLv3 COPYING.LGPLv2.1 "$license_dir/"
install -m644 ../ffmpeg.tar.xz ../avs.patch "$license_dir/"
cp "$companion_dir/player-threads.patch" ../player-threads.patch
install -m644 ../player-threads.patch "$license_dir/"
cp "$script_path" ../build.sh
install -m644 ../build.sh "$license_dir/build.sh"
printf '%s\n' 'FFmpeg 7.1.2: https://ffmpeg.org/releases/ffmpeg-7.1.2.tar.xz' 'AVS+/DRA patch: https://github.com/maliwen2015/ffmpeg_cavs_dra/tree/eb94ceb887b47b6ecfbda9da43273dd65246df1e' 'Built with GPL x264; preserve corresponding source and notices when distributing.' > "$license_dir/NOTICE"
printf 'Installed %s\n' "$prefix/bin/websa-dtmb-ffmpeg"
