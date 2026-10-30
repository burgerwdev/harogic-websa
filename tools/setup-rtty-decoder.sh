#!/usr/bin/env bash
# The GPL companion is a separate executable; never linked into the MIT host.
set -euo pipefail
revision=6b97328b1939f569b161c5066fbea8053a76c130
prefix="${WEBSA_DECODERS_PREFIX:-${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders}"
source_dir=""
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
usage() {
  printf '%s\n' 'Usage: bash tools/setup-rtty-decoder.sh [--prefix DIR] [--source ONAIR_CHECKOUT]' \
    'Builds only the standalone GPL RTTY decoder using a C++17 compiler and pinned OnAir source.' \
    'Without --source, git fetches the pinned revision into PREFIX/sources/onair-rtty.' \
    'No GUI, audio server, SDR driver, full OnAir build, root, or radio access is required.'
}
while (($#)); do
  case "$1" in
    --prefix) prefix="${2:?--prefix requires a directory}"; shift 2 ;;
    --source) source_dir="${2:?--source requires a checkout}"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done
[[ -n "$prefix" ]] || { printf '%s\n' 'Empty prefix is invalid' >&2; exit 2; }
compiler="${CXX:-c++}"
command -v "$compiler" >/dev/null
command -v git >/dev/null
mkdir -p "$prefix/bin" "$prefix/sources" "$prefix/share/licenses/websa-rtty"
if [[ -z "$source_dir" ]]; then
  source_dir="$prefix/sources/onair-rtty"
  if [[ ! -d "$source_dir/.git" ]]; then git init "$source_dir"; fi
  if ! git -C "$source_dir" cat-file -e "$revision^{commit}" 2>/dev/null; then
    git -C "$source_dir" fetch --depth 1 https://github.com/adswill/OnAir.git "$revision"
  fi
  git -C "$source_dir" -c advice.detachedHead=false checkout --detach "$revision"
fi
[[ "$(git -C "$source_dir" rev-parse HEAD)" == "$revision" ]] || {
  printf 'OnAir source must be pinned to %s\n' "$revision" >&2; exit 1;
}
# Refuse local changes to the compiled source and its headers: the revision is provenance.
git -C "$source_dir" diff --exit-code HEAD -- core/src/hfdig/hfdig_rtty.cpp core/include
build_dir="$(mktemp -d "$prefix/sources/rtty-build.XXXXXX")"
trap 'rm -rf -- "$build_dir"' EXIT
"$compiler" -std=c++17 -O2 -pthread -I "$source_dir/core/include" \
  "-DWEBSA_ONAIR_REV=\"$revision\"" \
  "$script_dir/companions/rtty/main.cpp" "$source_dir/core/src/hfdig/hfdig_rtty.cpp" \
  -o "$build_dir/websa-rtty"
install -m755 "$build_dir/websa-rtty" "$prefix/bin/websa-rtty"
install -m644 "$source_dir/LICENSE" "$prefix/share/licenses/websa-rtty/LICENSE"
install -m644 "$script_dir/companions/rtty/main.cpp" "$prefix/share/licenses/websa-rtty/companion.cpp"
printf '%s\n' "OnAir source: https://github.com/adswill/OnAir/tree/$revision" \
  'OnAir HfdigRtty and this standalone companion: GPL-3.0-or-later.' \
  'Keep corresponding source and license notices when redistributing the executable.' \
  > "$prefix/share/licenses/websa-rtty/NOTICE"
printf 'export WEBSA_RTTY_PATH=%q\n' "$prefix/bin/websa-rtty" > "$prefix/rtty-env.sh"
printf 'Installed %s\nSource %s before starting websad.\n' "$prefix/bin/websa-rtty" "$prefix/rtty-env.sh"
