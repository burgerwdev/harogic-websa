#include <chrono>
#include <cmath>
#include <cstdio>
#include <htra_api.h>
#include <vector>
int main() {
  void *dsp = nullptr;
  struct CloseDsp {
    void **handle;
    ~CloseDsp() {
      if (*handle)
        DSP_Close(handle);
    }
  } close_dsp{&dsp};
  int st = DSP_Open(&dsp);
  if (st || !dsp)
    return 1;
  for (auto n : {16240u, 32768u, 65536u, 32768u, 16240u}) {
    DSP_FFT_TypeDef in{}, out{};
    DSP_FFT_DeInit(&in);
    in.FFTSize = n;
    in.SamplePts = n;
    in.WindowType = FlatTop;
    in.Intercept = .8;
    in.DetectionRatio = 1;
    in.Calibration = false;
    in.TraceDetector = TraceDetector_PosPeak;
    uint32_t pts = 0;
    double rbw = 0;
    st = DSP_FFT_Configuration(&dsp, &in, &out, &pts, &rbw);
    printf("request=%u status=%d actual_fft=%u actual_samples=%u output=%u "
           "rbw_ratio=%.12g\n",
           n, st, out.FFTSize, out.SamplePts, pts, rbw);
    if (st || out.FFTSize != n || out.SamplePts != n || pts < 2 || pts > n ||
        !std::isfinite(rbw) || rbw <= 0) {
      std::fprintf(stderr, "invalid FFT configuration\n");
      return 2;
    }
    std::vector<int16_t> iq(2 * n);
    for (unsigned i = 0; i < n; i++) {
      double phase = 2 * M_PI * 1234 * i / n;
      iq[2 * i] = lround(10000 * cos(phase));
      iq[2 * i + 1] = lround(10000 * sin(phase));
    }
    IQStream_TypeDef stream{};
    stream.AlternIQStream = iq.data();
    stream.IQS_ScaleToV = 1e-6;
    stream.IQS_Profile.DataFormat = Complex16bit;
    stream.IQS_Profile.CenterFreq_Hz = 100e6;
    stream.IQS_Profile.TriggerLength = n;
    stream.IQS_StreamInfo.PacketSamples = n;
    stream.IQS_StreamInfo.StreamSamples = n;
    stream.IQS_StreamInfo.PacketDataSize = 4 * n;
    stream.IQS_StreamInfo.IQSampleRate = 62500000.;
    stream.IQS_StreamInfo.Bandwidth = 50e6;
    std::vector<double> freq(pts);
    std::vector<float> p(pts);
    double elapsed_ms = 0;
    for (int k = 0; k < 100; k++) {
      auto t = std::chrono::steady_clock::now();
      st = DSP_FFT_IQSToSpectrum(&dsp, &stream, freq.data(), p.data());
      elapsed_ms += std::chrono::duration<double, std::milli>(
                        std::chrono::steady_clock::now() - t).count();
      if (st) {
        std::fprintf(stderr, "FFT processing failed: %d\n", st);
        return 3;
      }
      for (unsigned i = 0; i < pts; i++) {
        if (!std::isfinite(freq[i]) || !std::isfinite(p[i]) ||
            (i && freq[i] <= freq[i - 1])) {
          std::fprintf(stderr, "invalid FFT output at point %u\n", i);
          return 3;
        }
      }
    }
    double ms = elapsed_ms / 100;
    unsigned peak = 0;
    for (unsigned i = 1; i < pts; i++)
      if (p[i] > p[peak])
        peak = i;
    printf("first=%.12f last=%.12f peak_index=%u\n", freq[0], freq[pts - 1],
           peak);
    printf("process=%d mean_ms=%.6f peak_dbm=%.6f peak_hz=%.6f df=%.6f\n", st,
           ms, p[peak], freq[peak], freq[1] - freq[0]);
  }
}
