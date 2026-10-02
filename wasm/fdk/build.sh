#!/bin/bash
# Rebuild libfdk.a (wasm32-unknown-unknown) from the Fraunhofer FDK AAC sources.
#
# Usage: wasm/fdk/build.sh /path/to/fdk-aac
#
# The FDK AAC decoder is C++ and needs a tiny runtime shim (malloc/free come from the
# Rust host; memcpy/memset/memmove/memcmp from Rust's compiler-builtins). The shim
# provides operator new/delete and a few string helpers. The result is committed as
# wasm/fdk/libfdk.a so the crate builds without a C++ toolchain.
set -euo pipefail

SRC="${1:?usage: wasm/fdk/build.sh /path/to/fdk-aac}"
HERE="$(cd "$(dirname "$0")" && pwd)"

CXX="clang++ --target=wasm32-unknown-unknown -ffreestanding -fno-exceptions -fno-rtti -fno-threadsafe-statics -O2 -std=c++11 -DNDEBUG"
INCS="-I$SRC/libAACdec/include -I$SRC/libFDK/include -I$SRC/libMpegTPDec/include -I$SRC/libSBRdec/include -I$SRC/libSACdec/include -I$SRC/libSYS/include -I$SRC/libPCMutils/include -I$SRC/libDRCdec/include -I$SRC/libArithCoding/include"

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

# Decoder source files (AAC-LC/HE-AAC/HE-AACv2 receive path).
FILES=(
  libAACdec/src/aacdec_drc.cpp libAACdec/src/aacdec_hcr_bit.cpp libAACdec/src/aacdec_hcr.cpp
  libAACdec/src/aacdec_hcrs.cpp libAACdec/src/aacdecoder.cpp libAACdec/src/aacdecoder_lib.cpp
  libAACdec/src/aacdec_pns.cpp libAACdec/src/aacdec_tns.cpp libAACdec/src/aac_ram.cpp
  libAACdec/src/aac_rom.cpp libAACdec/src/block.cpp libAACdec/src/channel.cpp
  libAACdec/src/channelinfo.cpp libAACdec/src/conceal.cpp libAACdec/src/FDK_delay.cpp
  libAACdec/src/ldfiltbank.cpp libAACdec/src/pulsedata.cpp libAACdec/src/rvlcbit.cpp
  libAACdec/src/rvlcconceal.cpp libAACdec/src/rvlc.cpp libAACdec/src/stereo.cpp
  libAACdec/src/usacdec_ace_d4t64.cpp libAACdec/src/usacdec_acelp.cpp
  libAACdec/src/usacdec_ace_ltp.cpp libAACdec/src/usacdec_fac.cpp
  libAACdec/src/usacdec_lpc.cpp libAACdec/src/usacdec_lpd.cpp libAACdec/src/usacdec_rom.cpp
  libFDK/src/autocorr2nd.cpp libFDK/src/dct.cpp libFDK/src/FDK_bitbuffer.cpp
  libFDK/src/FDK_core.cpp libFDK/src/FDK_crc.cpp libFDK/src/FDK_decorrelate.cpp
  libFDK/src/FDK_hybrid.cpp libFDK/src/FDK_lpc.cpp libFDK/src/FDK_matrixCalloc.cpp
  libFDK/src/FDK_qmf_domain.cpp libFDK/src/FDK_tools_rom.cpp libFDK/src/FDK_trigFcts.cpp
  libFDK/src/fft.cpp libFDK/src/fft_rad2.cpp libFDK/src/fixpoint_math.cpp
  libFDK/src/huff_nodes.cpp libFDK/src/mdct.cpp libFDK/src/nlc_dec.cpp libFDK/src/qmf.cpp
  libFDK/src/scale.cpp
  libMpegTPDec/src/tpdec_adif.cpp libMpegTPDec/src/tpdec_adts.cpp libMpegTPDec/src/tpdec_asc.cpp
  libMpegTPDec/src/tpdec_drm.cpp libMpegTPDec/src/tpdec_latm.cpp libMpegTPDec/src/tpdec_lib.cpp
  libSBRdec/src/env_calc.cpp libSBRdec/src/env_dec.cpp libSBRdec/src/env_extr.cpp
  libSBRdec/src/hbe.cpp libSBRdec/src/HFgen_preFlat.cpp libSBRdec/src/huff_dec.cpp
  libSBRdec/src/lpp_tran.cpp libSBRdec/src/psbitdec.cpp libSBRdec/src/psdec.cpp
  libSBRdec/src/psdec_drm.cpp libSBRdec/src/psdecrom_drm.cpp libSBRdec/src/pvc_dec.cpp
  libSBRdec/src/sbr_deb.cpp libSBRdec/src/sbr_dec.cpp libSBRdec/src/sbrdec_drc.cpp
  libSBRdec/src/sbrdec_freq_sca.cpp libSBRdec/src/sbrdecoder.cpp libSBRdec/src/sbr_ram.cpp
  libSBRdec/src/sbr_rom.cpp
  libSACdec/src/sac_bitdec.cpp libSACdec/src/sac_calcM1andM2.cpp libSACdec/src/sac_dec_conceal.cpp
  libSACdec/src/sac_dec.cpp libSACdec/src/sac_dec_lib.cpp libSACdec/src/sac_process.cpp
  libSACdec/src/sac_qmf.cpp libSACdec/src/sac_reshapeBBEnv.cpp libSACdec/src/sac_rom.cpp
  libSACdec/src/sac_smoothing.cpp libSACdec/src/sac_stp.cpp libSACdec/src/sac_tsd.cpp
  libPCMutils/src/limiter.cpp libPCMutils/src/pcmdmx_lib.cpp libPCMutils/src/pcm_utils.cpp
  libSYS/src/syslib_channelMapDescr.cpp
  libDRCdec/src/drcDec_gainDecoder.cpp libDRCdec/src/drcDec_reader.cpp libDRCdec/src/drcDec_rom.cpp
  libDRCdec/src/drcDec_selectionProcess.cpp libDRCdec/src/drcDec_tools.cpp
  libDRCdec/src/drcGainDec_init.cpp libDRCdec/src/drcGainDec_preprocess.cpp
  libDRCdec/src/drcGainDec_process.cpp libDRCdec/src/FDK_drcDecLib.cpp
  libArithCoding/src/ac_arith_coder.cpp
)

for f in "${FILES[@]}"; do
  o="$OUT/$(basename "$f" .cpp).o"
  $CXX $INCS -c "$SRC/$f" -o "$o"
done

# Runtime shim: operator new/delete, string helpers and the FDK_* memory/string
# wrappers (genericStds.cpp is skipped because it includes hosted libc headers).
$CXX $INCS -c "$HERE/shim.cpp" -o "$OUT/fdk_shim.o"

llvm-ar rcs "$HERE/libfdk.a" "$OUT"/*.o
echo "wrote $HERE/libfdk.a ($(ls -la "$HERE/libfdk.a" | awk '{print $5}') bytes)"
