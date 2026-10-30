// SPDX-License-Identifier: GPL-3.0-or-later
// Standalone file generator linked to an external, pinned OnAir checkout.
// OnAir's REUSE.toml and THIRD_PARTY.md describe the encoder provenance/licenses.
#include "dect2/ais/ais_gen.h"
#include "dect2/packet/packet_gen.h"
#include "dect2/packet/packet_rx.h"
#include "hfdig_ftx_int.h"

#include <algorithm>
#include <cerrno>
#include <cmath>
#include <complex>
#include <cstdint>
#include <cstdlib>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

namespace {
using dect2::cf32;
namespace ftx = dect2::ftx;

void require(bool condition, const char* message) {
    if (!condition) throw std::runtime_error(message);
}

double number(const char* text, const char* error) {
    char* end = nullptr;
    errno = 0;
    const double value = std::strtod(text, &end);
    require(end != text && *end == '\0' && errno != ERANGE && std::isfinite(value), error);
    return value;
}

void printable(const std::string& text, size_t maximum) {
    require(!text.empty() && text.size() <= maximum, "MESSAGE is empty or too long");
    for (unsigned char c : text)
        require(c >= 32 && c <= 126, "MESSAGE must contain printable ASCII only");
}

std::string normalized(const std::string& text) {
    printable(text, 256);
    std::istringstream input(text);
    std::string result, word;
    while (input >> word) {
        if (!result.empty()) result += ' ';
        for (char c : word) result += c >= 'a' && c <= 'z' ? char(c - 'a' + 'A') : c;
    }
    require(!result.empty(), "MESSAGE must not be blank");
    return result;
}

void validateWspr(const std::string& text) {
    // The upstream packer uses atoi for power and its type-3 grid check is permissive.
    std::istringstream input(text);
    std::vector<std::string> fields;
    std::string word;
    while (input >> word) fields.push_back(word);
    require(fields.size() == 2 || fields.size() == 3, "WSPR requires CALL [GRID] POWER");
    const auto& power = fields.back();
    require(power.size() <= 2 && std::all_of(power.begin(), power.end(), [](char c) { return c >= '0' && c <= '9'; }),
            "WSPR power must be a decimal value from 0 to 60 dBm");
    if (fields.front().front() == '<') {
        const auto& call = fields.front();
        require(fields.size() == 3 && call.size() >= 5 && call.size() <= 13 && call.back() == '>', "Invalid WSPR hashed callsign");
        require(std::all_of(call.begin() + 1, call.end() - 1, [](char c) {
            return (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '/';
        }), "Invalid WSPR hashed callsign");
        const auto& grid = fields[1];
        require(grid.size() == 6 && grid[0] >= 'A' && grid[0] <= 'R' && grid[1] >= 'A' && grid[1] <= 'R'
                && grid[2] >= '0' && grid[2] <= '9' && grid[3] >= '0' && grid[3] <= '9'
                && grid[4] >= 'A' && grid[4] <= 'X' && grid[5] >= 'A' && grid[5] <= 'X', "Invalid WSPR six-character locator");
    }
}

uint32_t mmsiNumber(const std::string& text) {
    require(text.size() == 9, "AIS MMSI must contain exactly nine decimal digits");
    uint32_t value = 0;
    for (char c : text) {
        require(c >= '0' && c <= '9', "AIS MMSI must contain exactly nine decimal digits");
        value = value * 10 + uint32_t(c - '0');
    }
    require(dect2::ais::mmsiPlausible(value), "AIS MMSI is outside the assigned maritime identification ranges");
    return value;
}

std::string jsonString(const std::string& text) {
    std::string result = "\"";
    for (char c : text) {
        if (c == '"' || c == '\\') result += '\\';
        result += c;
    }
    return result + '"';
}

std::vector<cf32> render(const std::string& mode, const std::string& message, double baseHz,
                         double rate, size_t count, std::string& expected) {
    const double period = double(count) / rate;
    if (mode == "ft4" || mode == "wspr") {
        const int kind = mode == "ft4" ? ftx::kFt4 : ftx::kWspr;
        const auto& spec = ftx::spec(kind);
        require(period >= spec.t0 + spec.nn * spec.symSec, "PERIOD would truncate the digital frame");
        require(baseHz > -rate / 2 && baseHz + (spec.tones - 1) * spec.spacing < rate / 2,
                "BASE_HZ places FSK tones outside the source Nyquist band");
        expected = normalized(message);
        std::vector<int> tones;
        if (kind == ftx::kWspr) {
            validateWspr(expected);
            require(ftx::wsprSymbols(expected, tones), "MESSAGE is not encodable as WSPR");
        } else {
            uint8_t bits[77];
            require(ftx::pack77(expected, bits), "MESSAGE is not encodable as FT4");
            tones = ftx::ftxTones(kind, bits);
        }
        std::vector<float> re(count, 0), im(count, 0);
        ftx::addWaveIQ(kind, tones, baseHz, rate, spec.t0 * rate, false, re, im);
        std::vector<cf32> iq(count);
        for (size_t i = 0; i < count; ++i) iq[i] = cf32(re[i], im[i]);
        return iq;
    }
    if (mode == "aprs") {
        printable(message, 256);
        require(std::abs(baseHz) + 12500 < rate / 2, "BASE_HZ places APRS outside the source Nyquist band");
        dect2::ax25::Frame frame;
        frame.from.call = "N0CALL";
        frame.from.ssid = 1;
        frame.to.call = "APRS";
        frame.to.h = true; // AX.25 UI command: destination C=1, source C=0.
        frame.info = message;
        const auto bits = dect2::ax25::hdlcBits(dect2::ax25::buildFrame(frame), 24, 4);
        // makePacketSynthFrame loops its .6 s lead, frame and .25 s tail. Render
        // only the first complete frame (plus the final interpolation sample).
        const size_t active = size_t(std::ceil((0.6 + double(bits.size()) / 1200) * rate)) + 1;
        require(active <= count, "PERIOD would truncate the APRS frame");
        dect2::SynthConfig config;
        config.snrDb = 100;
        config.cfoHz = baseHz + dect2::packetTuning().tuneOffsetHz;
        auto synth = dect2::makePacketSynthFrame(config, rate, frame, 1200);
        require(bool(synth), "OnAir rejected the APRS source rate");
        std::vector<cf32> iq(count, cf32(0, 0));
        synth->generate(iq.data(), active);
        expected = "N0CALL-1>APRS:" + message;
        return iq;
    }
    require(std::abs(baseHz) + 12500 < rate / 2, "BASE_HZ places AIS outside the source Nyquist band");
    const uint32_t mmsi = mmsiNumber(message);
    dect2::ais::Bits payload(168, 0);
    // ITU-R M.1371 type 1 field layout, also used by OnAir's ais_testutil.h.
    dect2::ais::putU(payload, 0, 6, 1);
    dect2::ais::putU(payload, 8, 30, mmsi);
    dect2::ais::putU(payload, 38, 4, 5); // moored
    dect2::ais::putS(payload, 42, 8, -128); // turn rate unavailable
    dect2::ais::putU(payload, 60, 1, 1); // position accuracy better than 10 m
    dect2::ais::putS(payload, 61, 28, int32_t(std::lround(-122.4194 * 600000)));
    dect2::ais::putS(payload, 89, 27, int32_t(std::lround(37.7749 * 600000)));
    dect2::ais::putU(payload, 116, 12, 3600); // course unavailable
    dect2::ais::putU(payload, 128, 9, 511); // heading unavailable
    std::vector<dect2::AisBurstSpec> bursts(2);
    for (size_t i = 0; i < bursts.size(); ++i) {
        bursts[i].payload = payload;
        dect2::ais::putU(bursts[i].payload, 137, 6, uint32_t(i));
        bursts[i].channel = 'A';
        bursts[i].startSec = 0.2 + double(i);
        const auto line = dect2::ais::burstLineBits(bursts[i].payload);
        require(period >= bursts[i].startSec + double(line.size() + 1) / 9600, "PERIOD would truncate an AIS burst");
    }
    dect2::AisRenderConfig config;
    config.rate = rate;
    // renderAisBursts truncates its sample count; half a sample avoids roundoff.
    config.durationSec = (double(count) + 0.5) / rate;
    config.snrDb = -200;
    config.tuneOffsetHz = -25000 - baseHz;
    expected = "AIS type 1 MMSI " + message + " lat 37.7749 lon -122.4194";
    return dect2::renderAisBursts(bursts, config);
}
} // namespace

int main(int argc, char** argv) {
    try {
        require(argc == 6, "Usage: pluto_digital_source OUTPUT MODE MESSAGE BASE_HZ PERIOD (MODE: ft4, wspr, aprs, ais)");
        const std::string mode = argv[2];
        require(mode == "ft4" || mode == "wspr" || mode == "aprs" || mode == "ais", "MODE must be ft4, wspr, aprs or ais");
        const double rate = mode == "aprs" ? 240000 : mode == "ais" ? 96000 : 8000;
        const double baseHz = number(argv[4], "BASE_HZ must be a finite number");
        const double period = number(argv[5], "PERIOD must be a finite number");
        require(period > 0 && period * rate <= double(std::numeric_limits<int>::max()), "PERIOD is outside the supported sample range");
        const size_t count = size_t(std::llround(period * rate));
        require(count > 0, "PERIOD is shorter than one sample");
        std::string expected;
        const auto iq = render(mode, argv[3], baseHz, rate, count, expected);
        static_assert(sizeof(float) == 4 && std::numeric_limits<float>::is_iec559, "CF32 requires IEEE float32");
        static_assert(sizeof(cf32) == 2 * sizeof(float), "CF32 requires contiguous I/Q floats");
        std::ofstream output;
        output.exceptions(std::ios::failbit | std::ios::badbit);
        output.open(argv[1], std::ios::binary | std::ios::trunc);
        output.write(reinterpret_cast<const char*>(iq.data()), std::streamsize(iq.size() * sizeof(cf32)));
        output.close();
        std::cout << std::setprecision(17) << "{\"rate\":" << rate << ",\"period\":" << double(iq.size()) / rate
                  << ",\"expected\":" << jsonString(expected) << "}\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << "pluto_digital_source: " << error.what() << '\n';
        return 1;
    }
}
