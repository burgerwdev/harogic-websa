// SPDX-License-Identifier: GPL-3.0-or-later
#include "dect2/dtmb/dtmb_fft.h"
#include "dect2/dsp/dsp_compat.h"
#include <fftw3.h>
#include <cstring>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <unordered_map>

namespace {
std::mutex planner;
struct Plan {
    fftwf_complex* data;
    fftwf_plan forward, inverse;
    explicit Plan(int size) : data(fftwf_alloc_complex(size)), forward(nullptr), inverse(nullptr) {
        if (!data) throw std::bad_alloc();
        std::lock_guard lock(planner);
        forward = fftwf_plan_dft_1d(size, data, data, FFTW_FORWARD, FFTW_ESTIMATE);
        inverse = fftwf_plan_dft_1d(size, data, data, FFTW_BACKWARD, FFTW_ESTIMATE);
        if (!forward || !inverse) {
            if (forward) fftwf_destroy_plan(forward);
            if (inverse) fftwf_destroy_plan(inverse);
            fftwf_free(data);
            throw std::runtime_error("DTMB FFT plan failed");
        }
    }
    ~Plan() {
        std::lock_guard lock(planner);
        fftwf_destroy_plan(forward);
        fftwf_destroy_plan(inverse);
        fftwf_free(data);
    }
};
Plan& plan(int size) {
    thread_local std::unordered_map<int, std::unique_ptr<Plan>> plans;
    auto& value = plans[size];
    if (!value) value = std::make_unique<Plan>(size);
    return *value;
}
}

namespace dect2::dtmb {
MixedFft::MixedFft(int size) : n_(size) {
    int rest = size;
    if (rest < 1) throw std::invalid_argument("DTMB FFT size");
    for (int factor : {2, 3, 5, 7}) while (rest % factor == 0) rest /= factor;
    if (rest != 1) throw std::invalid_argument("DTMB FFT factors");
}
void MixedFft::run(cf32* samples, bool inverse) {
    auto& p = plan(n_);
    static_assert(sizeof(cf32) == sizeof(fftwf_complex));
    std::memcpy(p.data, samples, sizeof(cf32)*n_);
    fftwf_execute(inverse ? p.inverse : p.forward);
    std::memcpy(samples, p.data, sizeof(cf32)*n_);
}
}

namespace dect2 {
void fftSplit(float* real, float* imag, int log2n, bool inverse) {
    if (log2n < 1) return;
    if (log2n > 23) throw std::invalid_argument("DTMB FFT size");
    const int size = 1 << log2n;
    auto& p = plan(size);
    for (int i = 0; i < size; ++i) { p.data[i][0] = real[i]; p.data[i][1] = imag[i]; }
    fftwf_execute(inverse ? p.inverse : p.forward);
    for (int i = 0; i < size; ++i) { real[i] = p.data[i][0]; imag[i] = p.data[i][1]; }
}
}
