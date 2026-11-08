Native receiver noise reduction, WebSA 1.12.2

All PCM framing, DSP, rate conversion, cache handling, gain control and inference
calls are implemented in Rust. There is no Python processing subprocess, Python
runtime dependency or imported upstream Python implementation. Published ONNX
weights execute on the native ONNX Runtime 1.23.2 CPU library, through ort
2.0.0-rc.13 with API 23. One sequential, single-thread CPU session per model per
receiver worker is prepared/warmed before IQ admission. Explicit neural caches,
analysis/synthesis, resampling and guard states belong to each channel/L/R path.

Published model provenance (unmodified ONNX):

gtcrn_dns3_epoch96.onnx
  https://github.com/Xiaobin-Rong/gtcrn
  commit 502ebfab64da7c4a9af78dcb9c6ceef1ebb01c73
  stream/onnx_models/gtcrn_simple.onnx, matching streaming DNS3 epoch96
  sha256 b4718df6228e7bdf1a8a435cf98f838636eb2fd331acabf86ba87c5192ebcb87
  MIT; LICENSE.gtcrn
  16kHz, FFT512/hop256, periodic sqrt-Hann, 257 complex bins.
  Inputs: mix [1,257,1,2], conv_cache [2,1,16,16,33],
  tra_cache [2,3,1,1,16], inter_cache [2,1,33,16].

ulunas_dns3_stream.onnx
  https://github.com/Xiaobin-Rong/ul-unas
  commit 00f7c700da43d38347f30a6ccebd86fcbc798e07
  ulunas_onnx/onnx_models/ulunas_stream_simple.onnx
  sha256 f2e804d54d6a88f4f82f44d86c9f1cf646db2509bfca935cfbfc5fcd8cbfac3b
  MIT; LICENSE.ul-unas
  16kHz, FFT512/hop256, periodic Hann, 257 complex bins.
  Inputs: mix [1,257,1,2], conv_cache [1,5358],
  tfa_cache [1,402], inter_cache [1,1056].

fastenhancer_t_48k.onnx
  https://github.com/aask1357/fastenhancer
  Source/config commit 85d98a689a85b956dec384f258b075859ca50263
  Release onnx-48khz-v1, asset fastenhancer_t_spec.onnx
  https://github.com/aask1357/fastenhancer/releases/tag/onnx-48khz-v1
  sha256 ebc673b4346ecbb89d2dce13718dbac46db81c57b056e3cb0c79eb767405956a
  MIT; LICENSE.fastenhancer
  Official FastEnhancer-T 48kHz configuration, FFT1024/hop512, periodic Hann.
  Inputs: spec_in [1,513,1,2], cache_in_0/cache_in_1 [1,24,20].

ONNX Runtime CPU binary:
  https://github.com/microsoft/onnxruntime/releases/tag/v1.23.2
  Official Linux x64 archive sha256:
  1fa4dcaef22f6f7d5cd81b28c2800414350c10116f5fdd46a2160082551c5f9b
  libonnxruntime.so.1.23.2 sha256:
  13ab8084954fa4a47c777880180b90810d6020f021441395712b48a75b74c68b
  MIT, LICENSE.onnxruntime and ThirdPartyNotices.onnxruntime.txt.
  tools/fetch-nr-runtime.sh downloads/verifies it. The library is packaged beside
  websad. WEBSA_ORT_DYLIB can specify another location. Missing/init failure is
  reported as a selected-model preparation failure; inference failures stop the
  processing branch. Neither condition silently substitutes another algorithm.

Runtime choice:
  Existing Tract 0.21.4 executed GTCRN and UL-UNAS, but GTCRN's initial native
  probe was too close to its 16ms frame budget. FastEnhancer's published graph
  failed Tract analysis at /enc_pre/enc_pre.0/Pad (optional constant input).
  ONNX Runtime runs all three original graphs, without conversion/graph rewriting.

Causal PCM:
  Analysis uses zero initial overlap; recurrent caches persist between callbacks.
  Real FFT/IFFT and window-square-normalized overlap-add run in Rust. No centered
  reflect padding and no future frame is used. Completed hops only are emitted.
  One FFT hop of initial signal delay is retained in PCM and reported in metadata.
  The negative-time first synthesis hop is defined as zeros; it is never divided
  by a partial Hann window, avoiding startup overshoot. Real onset samples start
  in the next fully overlapped hop, with no change to the sample clock.
  GTCRN/UL-UNAS use a separate, removable 3:1 converter: 241-tap Blackman sinc,
  cutoff 7200Hz, decimation phase2/interpolation phase0. Combined converter delay
  is exactly 238 samples at 48k. Native model PCM delay totals:
    gtcrn / ul-unas: 3*256+238 = 1006 samples = 20.958333ms
    fastenhancer: 512 samples = 10.666667ms
  Packet accumulation adds bounded availability wait (at most an unfinished hop),
  not an additional signal shift. Flush offline with zeros then trim the reported
  delay to preserve the original sample count and tail. Stopping/switching a live
  stream drops its buffered tail under the existing receiver discontinuity policy.

Listener sample rates:
  Neural reducers accept 8-192kHz listening PCM, including 12/24kHz. At rates
  other than 48kHz, a channel-owned pair of Blackman-windowed sinc FIR converters
  surrounds the existing 48kHz model stream. Exact integer phase accumulators
  retain cadence across callbacks; the filters reject interpolation images and
  downsampling aliases. The cutoff is 0.45 times the lower sample rate. Each FIR
  has a radius of 32*ceil(input_rate/output_rate) input samples. A fractional delay
  in the return FIR rounds the complete signal delay up to a listener sample.
  Metadata includes both converters and model latency. At 12/24kHz respectively:
    dfn:          424 / 784 samples (35.333 / 32.667ms)
    gtcrn/ul-unas: 316 / 567 samples (26.333 / 23.625ms)
    fastenhancer: 192 / 320 samples (16.000 / 13.333ms)
  Converted paths run the model at full attenuation and mix the delayed original
  PCM at the listener rate. Thus 0dB retains the original waveform, including its
  high frequencies, with the same delay as other attenuation settings. Live
  attenuation changes do not reset conversion, recurrent state or sample clocks.
  The direct 48kHz paths retain their existing model-specific behavior below.

dfn (UI DeepFilterNet3):
  Restored pre-guard production behavior in 1.12.1 after listening feedback of
  room/echo coloration. Original licensed/embedded DFN3 weights and DfTract::process
  run directly in 480-sample hops; there is no guard, statistics or second WOLA.
  Original attenuation limit is applied by the DFN runtime's fixed spectrum mix.
  Normal DFN signal delay is 1440 samples (30ms); the former extra 24ms is gone.
  At 0dB the original runtime bypasses enhancement and its signal delay is zero
  (hop packet accumulation still applies). Original low-energy behavior is retained.
  Guard sources and prior reports remain in the external research archive.

Attenuation semantics:
  DFN uses its original fixed residual mix, a=10^(-atten_db/20), between aligned
  model and noisy spectra. At 0dB it returns original audio. At 100dB it runs the
  original full-enhancement policy. The three added models keep their existing
  delayed 48kHz dry path, including full-band audio for the 16kHz models:
    a=10^(-atten_db/20), Y=(1-a)*official_model_output+a*aligned_original.
  Their 0dB is delayed original; 100dB is effectively the official model output.
  NR precedes volume. Strength is Wiener-only. No hidden limiter is introduced.

Mode and UI policy:
  Display labels are DeepFilterNet3, GTCRN, UL_UNAS, FastEnhancer; persisted API
  ids remain dfn, gtcrn, ul-unas, fastenhancer respectively.
  GTCRN/UL-UNAS necessarily limit the wet component to ~7.2kHz. This affects
  wideband/music material at high attenuation. FastEnhancer/DFN remain at 48k.
  WFM has independent L/R model, converter and overlap state at equal cadence.
  DRM/DRM+ retain NR preference but bypass listener NR.
  Digital decoder RAW audio remains upstream and unaffected by listening NR.

Wiener:
  This is a spectral Wiener gain with an estimated flat in-band noise PSD, not
  IMCRA or a neural speech model. The exact lower-quartile order statistic uses
  reusable scratch and is corrected by -ln(0.75) for exponential noise-bin power.
  Noise estimation follows the demodulator's usable audio band rather than its
  rejected spectrum. Narrow CW keeps a conservative full-spectrum estimate to
  protect continuous carriers. Strong tones cannot set a histogram resolution
  and inflate the noise estimate. STFT overlap, delay and -6..-20dB gain-floor
  strength semantics are retained; the frame loop no longer allocates gains.

No claim of word-recognition or input-SNR improvement is implied by these
contracts, runtime timings, finite outputs or the original listening feedback.
