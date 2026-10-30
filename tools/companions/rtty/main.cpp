// SPDX-License-Identifier: GPL-3.0-or-later
// Standalone companion, built separately from the MIT websa host.
// Links OnAir's GPL HfdigRtty decoder; no decoder implementation is copied here.
#include "dect2/hfdig/hfdig_rtty.h"
#include <array>
#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <stdexcept>
#include <string>
#include <unistd.h>

static void quoted(const std::string& text) {
    std::cout << '"';
    for (unsigned char c : text) {
        if (c == '"' || c == '\\') std::cout << '\\' << c;
        else if (c < 32) {
            static const char hex[] = "0123456789abcdef";
            std::cout << "\\u00" << hex[c >> 4] << hex[c & 15];
        } else std::cout << c;
    }
    std::cout << '"';
}
static double number(const char* arg) {
    char* end = nullptr;
    const double value = std::strtod(arg, &end);
    if (end == arg || *end || !std::isfinite(value)) throw std::runtime_error("invalid numeric option");
    return value;
}
template<size_t N> static int index(double value, const double (&table)[N]) {
    for (size_t i = 0; i < N; ++i) if (value == table[i]) return static_cast<int>(i);
    throw std::runtime_error("unsupported baud or shift");
}
int main(int argc, char** argv) {
    try {
        if (argc == 2 && std::string(argv[1]) == "--version") {
            std::cout << "websa-rtty protocol=1 audio=f32le rate=8000 OnAir=" << WEBSA_ONAIR_REV << '\n';
            return 0;
        }
        int baud = 0, shift = 0;
        bool reverse = false, usos = false;
        double low = 0;
        for (int i = 1; i < argc; i += 2) {
            if (i + 1 == argc) throw std::runtime_error("option requires a value");
            const std::string key = argv[i];
            const double value = number(argv[i+1]);
            if (key == "--baud") baud = index(value, dect2::kRttyBaudTable);
            else if (key == "--shift") shift = index(value, dect2::kRttyShiftTable);
            else if (key == "--low-tone") low = value;
            else if (key == "--reverse" || key == "--usos") {
                if (value != 0 && value != 1) throw std::runtime_error("boolean option must be 0 or 1");
                (key == "--reverse" ? reverse : usos) = value != 0;
            } else throw std::runtime_error("unknown option: " + key);
        }
        if (low != 0 && (low < 300 || low + dect2::kRttyShiftTable[shift] > 3300))
            throw std::runtime_error("fixed pair must fit in 300..3300 Hz; 0 means auto");
        dect2::HfdigRtty decoder;
        decoder.setBaudIndex(baud);
        decoder.setShiftIndex(shift);
        decoder.setReverse(reverse);
        decoder.setUnshiftOnSpace(usos);
        decoder.setFixedPair(low);
        uint64_t previousChars = 0, nextStatus = 0;
        auto emit = [&](bool final) {
            dect2::HfdigRttyTelemetry t;
            decoder.telemetry(t);
            if (t.chars > previousChars) {
                const auto count = static_cast<size_t>(std::min<uint64_t>(t.chars - previousChars, t.text.size()));
                std::cout << "{\"kind\":\"text\",\"text\":";
                quoted(t.text.substr(t.text.size() - count));
                std::cout << ",\"characters\":" << t.chars << ",\"audioSamples\":" << t.audioSamples << "}\n";
                previousChars = t.chars;
            }
            if (final || t.audioSamples >= nextStatus) {
                std::cout << "{\"kind\":\"table\",\"state\":";
                quoted(t.state == 2 ? "receiving" : t.state == 1 ? "tone-pair-locked" : "searching");
                std::cout << ",\"audioSamples\":" << t.audioSamples << ",\"audioRateHz\":8000"
                    << ",\"characters\":" << t.chars << ",\"framingErrors\":" << t.framingErrors
                    << ",\"markHz\":" << t.markHz << ",\"spaceHz\":" << t.spaceHz
                    << ",\"baud\":" << t.baud << ",\"shiftHz\":" << t.shiftHz
                    << ",\"reverse\":" << (t.reverse ? "true" : "false")
                    << ",\"unshiftOnSpace\":" << (t.unshiftOnSpace ? "true" : "false")
                    << ",\"quality\":" << t.quality << ",\"text\":";
                quoted(t.text);
                std::cout << ",\"eof\":" << (final ? "true" : "false") << "}\n";
                nextStatus = t.audioSamples + 8000;
            }
            std::cout.flush();
            if (!std::cout) throw std::runtime_error("result output failed");
        };
        // POSIX read preserves live pipe latency; carry partial floats across writes.
        std::array<unsigned char, 4096> bytes{};
        std::array<float, 1024> audio{};
        size_t pending = 0;
        for (;;) {
            const ssize_t n = ::read(STDIN_FILENO, bytes.data() + pending, bytes.size() - pending);
            if (n < 0) { if (errno == EINTR) continue; throw std::runtime_error("audio input failed"); }
            if (n == 0) break;
            const size_t available = pending + static_cast<size_t>(n), count = available / 4;
            for (size_t i = 0; i < count; ++i) {
                const auto* b = bytes.data() + 4*i;
                const uint32_t bits = uint32_t(b[0]) | uint32_t(b[1]) << 8 | uint32_t(b[2]) << 16 | uint32_t(b[3]) << 24;
                std::memcpy(&audio[i], &bits, 4);
                if (!std::isfinite(audio[i])) throw std::runtime_error("nonfinite audio sample");
            }
            decoder.feedAudio(audio.data(), count);
            emit(false);
            pending = available % 4;
            std::memmove(bytes.data(), bytes.data() + count*4, pending);
        }
        if (pending) throw std::runtime_error("truncated f32le sample at EOF");
        emit(true);
        return 0;
    } catch (const std::exception& error) {
        std::cerr << "websa-rtty: " << error.what() << '\n';
        return 1;
    }
}
