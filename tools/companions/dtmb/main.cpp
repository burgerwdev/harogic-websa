// SPDX-License-Identifier: GPL-3.0-or-later
#include "dect2/dtmb/dtmb_rx.h"
#include <algorithm>
#include <array>
#include <bit>
#include <cerrno>
#include <cmath>
#include <cstring>
#include <iostream>
#include <stdexcept>
#include <string>
#include <thread>
#include <arpa/inet.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <unistd.h>

namespace {
struct Output {
    int fd = -1;
    std::array<uint8_t, 188*64> batch{};
    size_t filled = 0;
    ~Output() { if (fd >= 0) close(fd); }
    void write(const void* data, size_t size) {
        auto p = static_cast<const char*>(data);
        while (size) {
            const auto n = ::write(fd, p, size);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) throw std::runtime_error("transport output stopped");
            p += n; size -= static_cast<size_t>(n);
        }
    }
    void flush() { if (filled) { write(batch.data(), filled); filled = 0; } }
    void packets(const uint8_t* data, size_t size) {
        while (size) {
            const size_t n = std::min(size, batch.size()-filled);
            std::memcpy(batch.data()+filled, data, n);
            filled += n; data += n; size -= n;
            if (filled == batch.size()) flush();
        }
    }
};
double number(const std::string& text) {
    size_t end = 0;
    double value = std::stod(text, &end);
    if (end != text.size() || !std::isfinite(value)) throw std::runtime_error("invalid number");
    return value;
}
double finite(double value) { return std::isfinite(value) ? value : 0; }
void status(dect2::DtmbReceiver& receiver, uint64_t samples, bool eof) {
    dect2::DtmbTelemetry t;
    receiver.telemetry(t, 0);
    constexpr int headers[] = {420, 595, 945};
    constexpr const char* mappings[] = {"4QAM-NR", "4QAM", "16QAM", "32QAM", "64QAM"};
    std::cout << "{\"state\":\"" << (t.tsLock && t.dataValid ? "receiving" : t.siOk ? "correcting" : t.state ? "synchronizing" : "searching")
        << "\",\"header\":" << (t.header >= 0 && t.header < 3 ? headers[t.header] : 0)
        << ",\"phaseRotates\":" << (t.phaseRotates ? "true" : "false")
        << ",\"carriers\":" << t.carriers << ",\"siLocked\":" << (t.siOk ? "true" : "false")
        << ",\"siIndex\":" << t.siIndex << ",\"mapping\":\"" << (t.mapping >= 0 && t.mapping < 5 ? mappings[t.mapping] : "")
        << "\",\"codeRate\":" << (t.rate >= 0 && t.rate < 3 ? 0.4 + 0.2*t.rate : 0)
        << ",\"interleaver\":" << t.interleaver << ",\"cnDb\":" << finite(t.snrPnDb)
        << ",\"merDb\":" << finite(t.merDb) << ",\"cfoHz\":" << finite(t.cfoHz)
        << ",\"clockPpm\":" << finite(t.clockPpm) << ",\"levelDbfs\":" << finite(t.levelDbfs)
        << ",\"echoSpanUs\":" << finite(t.echoSpanUs) << ",\"frames\":" << t.frames
        << ",\"blocksOk\":" << t.blocksOk << ",\"blocksBad\":" << t.blocksBad
        << ",\"packets\":" << t.packets << ",\"dropped\":" << t.cwDropped
        << ",\"skipped\":" << t.cwSkipped << ",\"inputSamples\":" << samples
        << ",\"eof\":" << (eof ? "true" : "false") << "}\n" << std::flush;
    if (!std::cout) throw std::runtime_error("status output stopped");
}
}

int main(int argc, char** argv) {
    try {
        if (argc == 2 && std::string(argv[1]) == "--version") {
            std::cout << "websa-dtmb protocol=1 iq=cf32le transport=tcp OnAir=" << WEBSA_ONAIR_REV << '\n';
            return 0;
        }
        double rate = 15625000;
        int port = 0, threads = std::clamp<int>(std::thread::hardware_concurrency()/4, 1, 4);
        std::string token, output, format = "cf32le";
        for (int i = 1; i < argc; i += 2) {
            if (i + 1 == argc) throw std::runtime_error("missing option value");
            const std::string key = argv[i], value = argv[i+1];
            if (key == "--rate") rate = number(value);
            else if (key == "--port") { const double n = number(value); if (n < 1 || n > 65535 || n != std::floor(n)) throw std::runtime_error("invalid port"); port = static_cast<int>(n); }
            else if (key == "--threads") { const double n = number(value); if (n < 1 || n > 8 || n != std::floor(n)) throw std::runtime_error("invalid thread count"); threads = static_cast<int>(n); }
            else if (key == "--token") token = value;
            else if (key == "--output") output = value;
            else if (key == "--format") format = value;
            else throw std::runtime_error("unknown option: " + key);
        }
        if (rate < 8000000 || rate > 125000000 || (format != "cf32le" && format != "ci16le")) throw std::runtime_error("unsupported IQ geometry");
        Output sink;
        if (port && output.empty() && token.size() == 32 && token.find_first_not_of("0123456789abcdef") == std::string::npos) {
            sink.fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
            sockaddr_in addr{}; addr.sin_family = AF_INET; addr.sin_port = htons(port); addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
            timeval timeout{2, 0};
            if (sink.fd < 0 || setsockopt(sink.fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof timeout) || connect(sink.fd, reinterpret_cast<sockaddr*>(&addr), sizeof addr)) throw std::runtime_error("transport connection failed");
            sink.write(token.data(), token.size());
        } else if (!port && !output.empty()) {
            sink.fd = open(output.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
            if (sink.fd < 0) throw std::runtime_error("cannot create transport file");
        } else throw std::runtime_error("use --port/--token or --output");
        dect2::DtmbReceiver receiver;
        // One millisecond of wideband IQ should not require repeated pipe wakeups.
        (void)fcntl(STDIN_FILENO, F_SETPIPE_SZ, 1024*1024);
        receiver.setDecoderThreads(threads);
        receiver.setComputeMode(0);
        receiver.setLogCallback([](const std::string& line) { std::cerr << line << '\n'; });
        receiver.setPacketCallback([&](const uint8_t* p, size_t n, double) { sink.packets(p, n*188); });
        receiver.configure(rate, 8);
        if (!receiver.ready()) throw std::runtime_error("DTMB sample rate rejected");
        std::array<unsigned char, 131072> bytes{};
        std::array<dect2::cf32, 32768> iq{};
        const size_t stride = format == "ci16le" ? 4 : 8;
        size_t pending = 0;
        uint64_t samples = 0, nextReport = 0;
        for (;;) {
            const auto n = read(STDIN_FILENO, bytes.data() + pending, bytes.size() - pending);
            if (n < 0 && errno == EINTR) continue;
            if (n < 0) throw std::runtime_error("IQ input failed");
            if (!n && !pending) break;
            const size_t available = pending + n, count = available / stride;
            // Pipe reads can be only a few IQ pairs; keep DSP blocks independent of transport fragmentation.
            if (n && available < bytes.size()) { pending = available; continue; }
            if (stride == 8 && std::endian::native == std::endian::little) {
                static_assert(sizeof(dect2::cf32) == 8);
                std::memcpy(iq.data(), bytes.data(), count*8);
                bool invalid = false;
                for (size_t i = 0; i < count; ++i) {
                    invalid |= ((std::bit_cast<uint32_t>(iq[i].real()) & 0x7f800000u) == 0x7f800000u)
                        | ((std::bit_cast<uint32_t>(iq[i].imag()) & 0x7f800000u) == 0x7f800000u);
                }
                if (invalid) throw std::runtime_error("nonfinite IQ");
            } else for (size_t i = 0; i < count; ++i) {
                float pair[2];
                for (size_t j = 0; j < 2; ++j) {
                    const auto* p = bytes.data() + i*stride + j*(stride/2);
                    if (stride == 4) pair[j] = static_cast<int16_t>(uint16_t(p[0]) | uint16_t(p[1])<<8) / 32768.0f;
                    else { uint32_t bits = uint32_t(p[0]) | uint32_t(p[1])<<8 | uint32_t(p[2])<<16 | uint32_t(p[3])<<24; std::memcpy(&pair[j], &bits, 4); }
                    if (!std::isfinite(pair[j])) throw std::runtime_error("nonfinite IQ");
                }
                iq[i] = {pair[0], pair[1]};
            }
            receiver.feed(iq.data(), count);
            samples += count;
            if (samples >= nextReport) { status(receiver, samples, false); nextReport = samples + static_cast<uint64_t>(rate/2); }
            pending = available % stride;
            std::memmove(bytes.data(), bytes.data() + count*stride, pending);
            if (!n) break;
        }
        if (pending) throw std::runtime_error("truncated IQ pair");
        receiver.flush();
        sink.flush();
        status(receiver, samples, true);
        return 0;
    } catch (const std::exception& e) { std::cerr << "websa-dtmb: " << e.what() << '\n'; return 1; }
}
