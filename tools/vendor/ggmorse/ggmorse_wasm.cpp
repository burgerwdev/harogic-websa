// A C API around ggmorse's C++ decoder, for the browser.
//
// Why a wrapper at all: ggmorse's published wasm target is its SDL demo *application*, and its C++
// streaming contract is awkward to drive from JavaScript - `decode()` pulls audio through a callback
// that must be served in exact frame-sized chunks (a short read is logged as a capture failure), and
// the decoded characters wait in a queue that `takeRxData()` empties. This exposes what the audio
// worker actually needs as plain C symbols:
//
//   new(sampleRate, pitchHz, toleranceHz)  one decoder, tuned to the operator's Pitch
//   push_i16(samples, count)               append audio, decode whatever became available
//   take_text(buffer, capacity)            the characters decoded since the last take
//
// Audio in is int16 PCM at `sampleRate`; ggmorse works at 4 kHz and decimates internally when the
// ratio is integral, which the pipeline's 48 kHz is (GGMorse::kBaseSampleRate).
//
// ggmorse is MIT licensed (tools/vendor/ggmorse/LICENSE); this wrapper is part of the MIT project too.
#include "ggmorse/ggmorse.h"

#include <cstring>
#include <string>
#include <vector>

namespace {

struct Handle {
    GGMorse gm;
    /// Audio pushed but not yet pulled by the decoder (compacted after every decode pass).
    std::vector<int16_t> audio;
    /// Decoded characters waiting for the caller.
    std::string out;

    Handle(int sampleRateInp, float pitchHz, float toleranceHz)
        : gm(parametersFor(sampleRateInp)) {
        auto decode = GGMorse::getDefaultParametersDecode();
        if (pitchHz > 0.0f) {
            // This code sets the range only. `frequencyRangeMin/Max` give the band of the decoder
            // around the operator's Pitch. `frequency_hz` stays at 0. Thus the tone search inside
            // the band is automatic. The old code set `frequency_hz` to the Pitch. That value
            // pinned the Goertzel filter to this frequency. A test showed that a signal 150 Hz from
            // the Pitch decoded nothing. A real signal is always off the Pitch, because the two
            // clocks are different.
            //
            // `toleranceHz` gives the full frequency tolerance of the mode. The caller now uses the
            // CW audio band as the default, and not `pitch +/- 250`. The floor keeps the high-pass
            // filter out of the low-frequency noise. The ceiling is the frequency where ggmorse
            // stops all resolution. GGMorse decimates the audio to 4 kHz, thus its band ends at
            // approximately 1.5 kHz to 1.7 kHz. A test showed that a sidetone at 1800 Hz does not
            // decode, for any range.
            const float tol = toleranceHz > 0.0f ? toleranceHz : 250.0f;
            decode.frequency_hz = 0.0f;
            decode.frequencyRangeMin_hz = pitchHz - tol > 0.0f ? pitchHz - tol : 100.0f;
            decode.frequencyRangeMax_hz = pitchHz + tol;
        }
        gm.setParametersDecode(decode);
    }

    static GGMorse::Parameters parametersFor(int sampleRateInp) {
        auto p = GGMorse::getDefaultParameters();
        p.sampleRateInp = sampleRateInp > 0 ? float(sampleRateInp) : 48'000.0f;
        p.sampleFormatInp = GGMORSE_SAMPLE_FORMAT_I16;
        p.samplesPerFrame = GGMorse::kDefaultSamplesPerFrame;
        return p;
    }

    /// Serve the decoder exactly what it asked for, or nothing - a short read is an error to it.
    uint32_t pull(void* data, uint32_t nBytes) {
        const size_t need = nBytes / sizeof(int16_t);
        if (audio.size() < need) return 0;
        memcpy(data, audio.data(), nBytes);
        audio.erase(audio.begin(), audio.begin() + static_cast<long>(need));
        return nBytes;
    }
};

}  // namespace

extern "C" {

/// One decoder for `sampleRate` Hz input, aimed at `pitchHz` (+/- `toleranceHz`).
void* ggmorse_wasm_new(int sampleRate, float pitchHz, float toleranceHz) {
    return new Handle(sampleRate, pitchHz, toleranceHz);
}

void ggmorse_wasm_free(void* handle) {
    delete static_cast<Handle*>(handle);
}

/// Append `count` int16 samples and run the decoder over what is now available.
void ggmorse_wasm_push_i16(void* handle, const int16_t* samples, int count) {
    auto* h = static_cast<Handle*>(handle);
    if (h == nullptr || samples == nullptr || count <= 0) return;
    h->audio.insert(h->audio.end(), samples, samples + count);
    h->gm.decode([h](void* data, uint32_t nBytes) -> uint32_t {
        return h->pull(data, nBytes);
    });
    GGMorse::TxRx rx;
    if (h->gm.takeRxData(rx) > 0) {
        h->out.append(reinterpret_cast<const char*>(rx.data()), rx.size());
    }
}

/// Copy the decoded characters since the last call into `buffer` (capacity is bytes).
int ggmorse_wasm_take_text(void* handle, char* buffer, int capacity) {
    auto* h = static_cast<Handle*>(handle);
    if (h == nullptr || buffer == nullptr || capacity <= 0) return 0;
    const int n = int(h->out.size() < size_t(capacity) ? h->out.size() : size_t(capacity));
    if (n > 0) {
        memcpy(buffer, h->out.data(), size_t(n));
        h->out.erase(0, size_t(n));
    }
    return n;
}

/// Audio samples buffered towards the next frame (diagnostics).
int ggmorse_wasm_pending_samples(void* handle) {
    auto* h = static_cast<Handle*>(handle);
    return h == nullptr ? 0 : int(h->audio.size());
}

/// The pitch the decoder settled on, in Hz (0 until it detects one).
float ggmorse_wasm_pitch_hz(void* handle) {
    auto* h = static_cast<Handle*>(handle);
    return h == nullptr ? 0.0f : h->gm.getStatistics().estimatedPitch_Hz;
}

/// The speed the decoder settled on, in words per minute (0 until it detects one).
float ggmorse_wasm_wpm(void* handle) {
    auto* h = static_cast<Handle*>(handle);
    return h == nullptr ? 0.0f : h->gm.getStatistics().estimatedSpeed_wpm;
}

}  // extern "C"
