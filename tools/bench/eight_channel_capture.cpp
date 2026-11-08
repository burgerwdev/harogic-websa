// Deterministic raw ADC fixture for the production capture/PFB/fine/receiver probe.
// This adapter never opens a radio. It is not RF or vendor SDK performance evidence.
// c++ -O3 -std=c++17 -shared -fPIC -pthread tools/bench/eight_channel_capture.cpp -o /tmp/websa-eight-adc.so
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <thread>
#include <vector>

struct PacketInfo { uint64_t stamp; uint32_t samples; float scale_to_v; };
struct Timing { uint64_t entry_interval_ns, fetch_ns, memcpy_ns, fft_ns; uint32_t measured; int32_t fetch_status, fft_status; };
struct Capture {
    int decimate;
    double center;
    uint64_t position = 0;
    std::chrono::steady_clock::time_point began;
    std::vector<int16_t> raw;
};
#ifndef SDR_BENCH_CHANNELS
#define SDR_BENCH_CHANNELS 8
#endif
static_assert(SDR_BENCH_CHANNELS == 8 || SDR_BENCH_CHANNELS == 16);
constexpr double rate = 62500000.0;
constexpr double tau = 6.2831853071795864769;
constexpr uint64_t period = 3125000; // 50 ms, exact 20 Hz carrier/tone lattice.

extern "C" int san90_open_v8(void** out, double center, const void*, int decimate,
                               double* bandwidth, double* configured) {
    if (!out || !bandwidth || !configured || decimate < 1 || decimate > 64 || (decimate & (decimate-1))) return -100;
    auto* capture = new Capture{decimate, center, 0, {}, {}};
    capture->raw.resize(2*period);
    const double offsets[] = {-20e6,-12e6,-4e6,0.0,20000.0,40000.0,8e6,20e6,
        -18e6,-14e6,-10e6,-6e6,2e6,6e6,12e6,18e6};
    for (uint64_t i=0; i<period; ++i) {
        const double t=static_cast<double>(i)/rate;
        double re=0.0, im=0.0;
        for (int slot=0; slot<SDR_BENCH_CHANNELS; ++slot) {
            const int mode=slot%8;
            const double tone=400.0+200.0*mode;
            double phase=tau*offsets[slot]*t;
            double amplitude=1200.0;
            if (mode<2) amplitude*=1.0+0.5*std::cos(tau*tone*t);
            else if (mode<4) phase+=1500.0/tone*std::sin(tau*tone*t);
            else phase+=tau*tone*t*(mode<6 ? 1.0 : -1.0);
            re+=amplitude*std::cos(phase); im+=amplitude*std::sin(phase);
        }
        capture->raw[2*i]=static_cast<int16_t>(std::lround(re));
        capture->raw[2*i+1]=static_cast<int16_t>(std::lround(im));
    }
    *bandwidth=50e6/decimate; *configured=center; *out=capture;
    return 0;
}
extern "C" int san90_iqs_arm_v1(void* handle) {
    if (!handle) return -100;
    auto* c=static_cast<Capture*>(handle); c->began=std::chrono::steady_clock::now(); c->position=0;
    return 0;
}
extern "C" int san90_iqs_reconfig_v2(void* handle, double center, const void*, int decimate,
                                     double* bandwidth, double* configured) {
    if (!handle || !bandwidth || !configured || decimate<1 || decimate>64 || (decimate & (decimate-1))) return -100;
    auto* c=static_cast<Capture*>(handle); c->center=center; c->decimate=decimate;
    *bandwidth=50e6/decimate; *configured=center;
    return san90_iqs_arm_v1(handle);
}
extern "C" int san90_next_v3(void* handle, int16_t* dest, uint32_t capacity, PacketInfo* info,
                             double* freq, float* power, uint32_t spectrum_capacity, uint32_t* points,
                             Timing* timing, int* warning) {
    if (!handle || !dest || capacity<16240 || !info || !timing || !warning) return -100;
    auto* c=static_cast<Capture*>(handle);
    const auto due=c->began+std::chrono::nanoseconds((c->position+16240*c->decimate)*16);
    std::this_thread::sleep_until(due);
    for (uint64_t i=0; i<16240; ++i) {
        const auto index=(c->position+i*c->decimate)%period;
        dest[2*i]=c->raw[2*index]; dest[2*i+1]=c->raw[2*index+1];
    }
    *info={2*c->position,16240,1.0f}; c->position+=16240*c->decimate;
    *timing={}; *warning=0;
    if (freq && power && points) {
        if (spectrum_capacity<128) return -107;
        for (uint32_t i=0; i<128; ++i) {
            freq[i]=c->center+50e6/c->decimate*(static_cast<double>(i)/127.0-0.5);
            power[i]=-70.0f;
        }
        *points=128;
    }
    return 0;
}
extern "C" int san90_close(void* handle) { delete static_cast<Capture*>(handle); return 0; }
