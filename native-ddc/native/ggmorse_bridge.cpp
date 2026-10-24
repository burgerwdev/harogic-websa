// C ABI for the vendored MIT ggmorse core; no browser runtime.
#include "ggmorse/ggmorse.h"

#include <algorithm>
#include <array>
#include <cstring>
#include <memory>
#include <vector>

namespace {
struct Decoder {
    GGMorse gm;
    // Only one 100 ms input block plus an incomplete decoder frame is retained.
    std::vector<int16_t> audio;
    size_t head = 0;
    GGMorse::TxRx rx;
    std::array<char, 4096> text{};
    size_t text_size = 0;

    static GGMorse::Parameters parameters(int rate) {
        auto p = GGMorse::getDefaultParameters();
        p.sampleRateInp = float(rate);
        p.sampleFormatInp = GGMORSE_SAMPLE_FORMAT_I16;
        p.samplesPerFrame = GGMorse::kDefaultSamplesPerFrame;
        return p;
    }

    Decoder(int rate, float pitch, float tolerance) : gm(parameters(rate)) {
        auto p = GGMorse::getDefaultParametersDecode();
        p.frequency_hz = 0.0f;
        p.frequencyRangeMin_hz = pitch - tolerance > 0 ? pitch - tolerance : 100.0f;
        p.frequencyRangeMax_hz = pitch + tolerance;
        gm.setParametersDecode(p);
        audio.reserve(size_t(rate / 5 + 4096));
        rx.reserve(512);
    }

    uint32_t pull(void* dst, uint32_t bytes) {
        const auto count = bytes / sizeof(int16_t);
        if (audio.size() - head < count) return 0;
        std::memcpy(dst, audio.data() + head, bytes);
        head += count;
        return bytes;
    }

    void push(const int16_t* samples, size_t count) {
        audio.insert(audio.end(), samples, samples + count);
        gm.decode([this](void* dst, uint32_t bytes) { return pull(dst, bytes); });
        // Compact once per block rather than copying the tail on each decoder pull.
        if (head) {
            audio.erase(audio.begin(), audio.begin() + head);
            head = 0;
        }
        rx.clear();
        if (gm.takeRxData(rx) > 0) {
            const auto take = std::min(rx.size(), text.size() - text_size);
            std::memcpy(text.data() + text_size, rx.data(), take);
            text_size += take;
        }
    }
};
}

extern "C" {
void* native_ggmorse_new(int rate, float pitch, float tolerance) noexcept {
    try { return new Decoder(rate, pitch, tolerance); }
    catch (...) { return nullptr; }
}
void native_ggmorse_free(void* handle) noexcept { delete static_cast<Decoder*>(handle); }
int native_ggmorse_push(void* handle, const int16_t* samples, size_t count) noexcept {
    try {
        static_cast<Decoder*>(handle)->push(samples, count);
        return 0;
    } catch (...) { return -1; }
}
size_t native_ggmorse_take(void* handle, char* out, size_t capacity) noexcept {
    auto& h = *static_cast<Decoder*>(handle);
    const auto count = std::min(capacity, h.text_size);
    std::memcpy(out, h.text.data(), count);
    h.text_size -= count;
    std::memmove(h.text.data(), h.text.data() + count, h.text_size);
    return count;
}
float native_ggmorse_pitch(void* handle) noexcept {
    return static_cast<Decoder*>(handle)->gm.getStatistics().estimatedPitch_Hz;
}
float native_ggmorse_wpm(void* handle) noexcept {
    return static_cast<Decoder*>(handle)->gm.getStatistics().estimatedSpeed_wpm;
}
}
