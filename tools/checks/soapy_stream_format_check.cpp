// Runtime contract for the patched official SoapyHarogic driver.
// Links the patched HarogicDevice translation unit against an in-process HTRA
// stub. No physical probe is opened.
#include "HarogicDevice.hpp"

#include <SoapySDR/Constants.h>
#include <SoapySDR/Formats.hpp>
#include <SoapySDR/Errors.h>
#include <atomic>
#include <thread>

#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <memory>
#include <limits>
#include <new>
#include <stdexcept>
#include <string>
#include <vector>

namespace {

constexpr double kNativeRate = 122880000.0;
constexpr int kPacketSamples = 4;

// Faults are configured only while no RX worker exists.
double g_native_rate = kNativeRate;
double g_configured_rate = 0;
bool g_override_rate = false;
uint32_t g_configured_samples = kPacketSamples;
int64_t g_configured_bytes = -1;
int g_configured_format = -1;
std::atomic<int> g_starts(0), g_stops(0), g_fetches(0);
std::atomic<bool> g_triggered(false);
uint32_t g_packet_samples = kPacketSamples;
int64_t g_packet_bytes = -1;
int g_packet_format = -1;
bool g_null_packet = false;
std::atomic<bool> g_watch_allocations(false), g_excess_allocation(false);
constexpr size_t kAllocationBudget = 64 * 1024 * 1024;

int g_last_data_format = -1;
int g_opens = 0;
int g_closes = 0;
int g_configurations = 0;
bool g_fail_configuration = false;
bool g_fail_open = false;
int g_last_device_index = -1;
int g_device_lists = 0;
uint8_t g_network_count = 0;
int g_network_lists = 0;
std::array<uint8_t, 4> g_last_ip{};
std::atomic<int> g_packets_remaining(-1);
DevicePowerSupply_TypeDef g_last_power{};
std::atomic<bool> g_fail_fetch(false);
std::atomic<int> g_packets_before_failure(0);
std::atomic<bool> g_fatal_fetch_returned(false);
// Every stub fetch returns the same immutable packet. The RX worker may fetch
// ahead of readStream; expectations must not read a changing producer buffer.
int8_t g_iq[kPacketSamples * 2] = {10, -20, 11, -21, 12, -22, 13, -23};
int8_t g_iq8[kPacketSamples * 2] = {-128, 127, 0, -1, 1, 64, -64, 126};
int16_t g_iq16[kPacketSamples * 2] = {-32768, 32767, 0, -1, 1, 16384, -16384, 32766};
int32_t g_iq32[kPacketSamples * 2] = {INT32_MIN, INT32_MAX, 0, -1, 1, 1073741824, -1073741824, INT32_MAX - 1};
bool g_extrema = false;
int g_fetch_warning = 0;

void fail(const std::string &message) {
    std::cerr << "soapy_stream_format_check: " << message << '\n';
    std::exit(1);
}

void expect(bool condition, const std::string &message) {
    if (!condition) fail(message);
}

} // namespace

// A broken bound must fail without exhausting the test host. Observe the
// attempted allocation as well as the API error, so bad_alloc is not a pass.
void *operator new(std::size_t size) {
    if (g_watch_allocations && size > kAllocationBudget) {
        g_excess_allocation = true;
        throw std::bad_alloc();
    }
    if (void *p = std::malloc(size ? size : 1)) return p;
    throw std::bad_alloc();
}
[[gnu::noinline]] void operator delete(void *p) noexcept { std::free(p); }
[[gnu::noinline]] void operator delete(void *p, std::size_t) noexcept { std::free(p); }

extern "C" {

int Device_List(const BootProfile_TypeDef *, uint8_t *count, uint8_t indices[MAX_DEVICE],
                DeviceInfo_TypeDef info[MAX_DEVICE]) {
    if (!count || !indices || !info) return -1;
    ++g_device_lists;
    *count = 2;
    indices[0] = 2;
    indices[1] = 7;
    std::memset(info, 0, 2 * sizeof(*info));
    info[0].DeviceUID = 0xA11CE;
    info[1].DeviceUID = 0xB0B;
    info[0].Model = info[1].Model = 90;
    info[0].HardwareVersion = info[1].HardwareVersion = 1;
    return 0;
}

int Device_Open(void **handle, int index, const BootProfile_TypeDef *boot, BootInfo_TypeDef *info) {
    if (!handle || !info || index < 0) return -1;
    if (!g_fail_open) ++g_opens;
    g_last_device_index = index;
    g_last_power = boot->DevicePowerSupply;
    std::copy_n(boot->ETH_IPAddress, g_last_ip.size(), g_last_ip.begin());
    static int slot = 1;
    *handle = &slot;
    if (g_fail_open) return -1;
    std::memset(info, 0, sizeof(*info));
    info->DeviceInfo.DeviceUID = index == 7 ? 0xB0B : 0xA11CE;
    info->DeviceInfo.Model = 90;
    info->DeviceInfo.HardwareVersion = 1;
    return 0;
}

int Device_Close(void **handle) {
    if (handle && *handle) ++g_closes;
    if (handle) *handle = nullptr;
    return 0;
}

int Device_GetNetworkDeviceList(uint8_t *count, NetworkDeviceInfo_TypeDef *info, uint8_t *, uint8_t *) {
    if (!count || !info) return -1;
    ++g_network_lists;
    *count = g_network_count;
    // Inject a bad returned count without corrupting the caller's 64-entry buffer.
    for (unsigned i = 0; i < std::min<unsigned>(g_network_count, 64); ++i) {
        info[i] = {};
        info[i].DeviceUID = i == 63 ? 0xA11CE : 0xB000 + i;
        info[i].IPAddress[0] = 192;
        info[i].IPAddress[1] = 168;
        info[i].IPAddress[2] = 1;
        info[i].IPAddress[3] = static_cast<uint8_t>(i + 1);
    }
    return 0;
}

int IQS_ProfileDeInit(void **, IQS_Profile_TypeDef *profile) {
    if (!profile) return -1;
    std::memset(profile, 0, sizeof(*profile));
    profile->NativeIQSampleRate_SPS = g_native_rate;
    return 0;
}

int IQS_Configuration(void **, const IQS_Profile_TypeDef *in, IQS_Profile_TypeDef *out, IQS_StreamInfo_TypeDef *info) {
    if (!in || !out || !info) return -1;
    ++g_configurations;
    if (g_fail_configuration) return -1;
    *out = *in;
    g_last_data_format = static_cast<int>(in->DataFormat);
    std::memset(info, 0, sizeof(*info));
    if (g_configured_format >= 0) out->DataFormat = static_cast<DataFormat_TypeDef>(g_configured_format);
    info->PacketSamples = g_configured_samples;
    const size_t width = in->DataFormat == Complex8bit ? 2 : in->DataFormat == Complex16bit ? 4 : 8;
    info->PacketDataSize = g_configured_bytes >= 0 ? g_configured_bytes : info->PacketSamples * width;
    info->IQSampleRate = g_override_rate ? g_configured_rate :
        g_native_rate / (in->DecimateFactor ? in->DecimateFactor : 1);
    return 0;
}

int IQS_BusTriggerStart(void **) { ++g_starts; g_triggered = true; return 0; }
int IQS_BusTriggerStop(void **) { ++g_stops; g_triggered = false; return 0; }

int IQS_GetIQStream_PM1(void **, IQStream_TypeDef *stream) {
    if (!stream) return -1;
    ++g_fetches;
    if (g_fail_fetch && g_packets_before_failure.fetch_sub(1) <= 0) {
        g_fatal_fetch_returned = true;
        return -1;
    }
    std::this_thread::sleep_for(std::chrono::microseconds(10));
    if (g_packets_remaining == 0) return APIRETVAL_WARNING_BusTimeOut;
    if (g_packets_remaining > 0) --g_packets_remaining;
    std::memset(stream, 0, sizeof(*stream));
    stream->IQS_Profile.DataFormat = static_cast<DataFormat_TypeDef>(
        g_packet_format >= 0 ? g_packet_format : g_last_data_format);
    stream->IQS_StreamInfo.PacketSamples = g_packet_samples;
    const size_t width = g_last_data_format == Complex8bit ? 2 : g_last_data_format == Complex16bit ? 4 : 8;
    stream->IQS_StreamInfo.PacketDataSize = g_packet_bytes >= 0 ? g_packet_bytes : g_packet_samples * width;
    stream->AlternIQStream = g_extrema ? (g_last_data_format == Complex8bit ? static_cast<void *>(g_iq8) :
        g_last_data_format == Complex16bit ? static_cast<void *>(g_iq16) : static_cast<void *>(g_iq32)) : g_iq;
    if (g_null_packet) stream->AlternIQStream = nullptr;
    return g_fetch_warning;
}

} // extern "C"

static void expect_native(SoapySDR::Device &device, const std::string &format, double scale) {
    double full_scale = -1.0;
    const std::string native = device.getNativeStreamFormat(SOAPY_SDR_RX, 0, full_scale);
    expect(native == format && full_scale == scale, "native format/fullScale mismatch for " + format);
}

static void expect_rejected(SoapySDR::Device &device, const std::string &format) {
    bool threw = false;
    try {
        device.setupStream(SOAPY_SDR_RX, format);
    } catch (const std::runtime_error &) {
        threw = true;
    }
    expect(threw, "setupStream accepted unsupported format " + format);
}
template <typename Sample>
static void check_output(SoapySDR::Device &device, const std::string &native, Sample *packet, double scale) {
    for (const auto &output : {native, std::string(SOAPY_SDR_CF32)}) {
        auto *stream = device.setupStream(SOAPY_SDR_RX, output, {0}, {{"native_format", native}});
        expect_native(device, output, output == SOAPY_SDR_CF32 ? 1.0 : scale);
        expect(device.activateStream(stream) == 0, "typed output activation failed");
        const size_t bytes = kPacketSamples * (output == SOAPY_SDR_CF32 ? sizeof(std::complex<float>) : 2 * sizeof(Sample));
        alignas(std::complex<float>) unsigned char data[kPacketSamples * sizeof(std::complex<float>) + 16];
        std::memset(data, 0xA5, sizeof(data));
        void *buffs[] = {data};
        int flags = 0;
        long long time = 0;
        expect(device.readStream(stream, buffs, kPacketSamples, flags, time, 1000000) == kPacketSamples,
               "typed output sample count mismatch");
        for (size_t i = bytes; i < sizeof(data); ++i) expect(data[i] == 0xA5, "typed output overwrote canary");
        if (output == native) expect(std::memcmp(data, packet, bytes) == 0, "integer output changed SDK bits");
        else {
            const auto *samples = reinterpret_cast<const std::complex<float> *>(data);
            for (size_t i = 0; i < kPacketSamples; ++i) {
                const std::complex<float> expected(static_cast<float>(packet[2*i]) / static_cast<float>(scale),
                                                    static_cast<float>(packet[2*i+1]) / static_cast<float>(scale));
                expect(samples[i] == expected, "CF32 integer extrema normalization mismatch");
            }
        }
        device.setSampleRate(SOAPY_SDR_RX, 0, device.getSampleRate(SOAPY_SDR_RX, 0) > 60000000 ?
                             kNativeRate / 8 : kNativeRate);
        expect(g_last_data_format == (sizeof(Sample) == 1 ? Complex8bit : Complex16bit), "live rate changed SDK width");
        device.closeStream(stream);
    }
}


static void check_ring() {
    RingBuffer<int> ring(8);
    const int input[] = {0, 1, 2, 3, 4, 5, 6, 7};
    int output[10];
    std::fill(std::begin(output), std::end(output), -99);
    expect(ring.write(input, 8), "ring rejected exact capacity");
    expect(!ring.write(input, 1), "ring accepted data beyond capacity");
    expect(ring.read(output, 5) == 5, "ring short initial read");
    expect(std::equal(output, output + 5, input), "ring initial ordering");
    expect(ring.write(input, 5), "ring rejected wrapped write");
    expect(ring.read(output, 10) == 8, "ring failed wrapped full read");
    const int expected[] = {5, 6, 7, 0, 1, 2, 3, 4};
    expect(std::equal(output, output + 8, expected), "ring wrapped ordering");
    expect(output[8] == -99 && output[9] == -99, "ring wrote past available samples");
    expect(ring.size() == 0 && ring.read(output, 1) == 0, "ring did not conserve samples");
    expect(ring.write(input, 8), "ring rejected wrapped exact capacity");
    expect(ring.read(output, 8) == 8 && std::equal(output, output + 8, input), "ring exact-capacity wrap");
}

static void check_discovery() {
    const auto all = SoapySDR::Device::enumerate({{"driver", "harogic"}, {"interface", "USB"}});
    expect(all.size() == 2, "USB enumeration did not return Device_List entries");
    expect(all[0].at("serial") == "A11CE" && all[0].at("device") == "2" &&
           all[1].at("serial") == "B0B" && all[1].at("device") == "7",
           "USB enumeration lost SDK serials or sparse device indices");
    const auto serial = SoapySDR::Device::enumerate({{"driver", "harogic"}, {"interface", "USB"}, {"serial", "B0B"}});
    expect(serial.size() == 1 && serial[0].at("device") == "7", "USB serial filter selected the wrong device");
    const auto index = SoapySDR::Device::enumerate({{"driver", "harogic"}, {"interface", "USB"}, {"device", "2"}});
    expect(index.size() == 1 && index[0].at("serial") == "A11CE", "USB index filter selected the wrong device");
    expect(SoapySDR::Device::enumerate({{"driver", "harogic"}, {"interface", "USB"},
                                      {"device", "2"}, {"serial", "B0B"}}).empty(),
           "USB enumeration ignored conflicting filters");
    expect(g_device_lists > 0 && g_opens == 0 && g_closes == 0,
           "USB enumeration opened hardware instead of using Device_List");
}

static void check_network_bounds(bool construction) {
    const auto discover = [] {
        return SoapySDR::Device::enumerate({{"driver", "harogic"}, {"interface", "ETH"}});
    };
    const auto valid = [&] {
        g_network_count = 64;
        const int opens = g_opens, closes = g_closes;
        if (construction) {
            {
                SoapyHarogic device(SoapySDR::Kwargs{{"interface", "ETH"}, {"serial", "A11CE"}});
                expect(g_last_ip == std::array<uint8_t, 4>{192, 168, 1, 64},
                       "ETH constructor did not resolve the last valid discovery entry");
                expect(g_opens == opens + 1 && g_closes == closes,
                       "ETH constructor did not retain exactly one handle");
            }
            expect(g_closes == closes + 1, "ETH constructor leaked its valid handle");
        } else {
            const auto devices = discover();
            expect(devices.size() == 64 && devices.front().at("serial") == "B000" &&
                   devices.back().at("serial") == "A11CE" && devices.back().at("ip") == "192.168.1.64",
                   "ETH enumeration lost the exact-capacity discovery list");
            expect(g_opens == opens && g_closes == closes, "ETH discovery opened a device");
        }
    };
    valid();
    for (uint8_t count : {65, 255}) {
        g_network_count = count;
        const int opens = g_opens, closes = g_closes;
        bool rejected = false;
        try {
            if (construction) {
                SoapyHarogic invalid(SoapySDR::Kwargs{{"interface", "ETH"}, {"serial", "A11CE"}});
            } else {
                // Soapy's public enumeration may translate a driver exception to no results.
                rejected = discover().empty();
            }
        } catch (const std::runtime_error &) { rejected = true; }
        expect(rejected, std::string(construction ? "ETH constructor" : "ETH enumeration") +
               " accepted SDK count " + std::to_string(count));
        expect(g_opens == opens && g_closes == closes,
               "malformed ETH discovery opened a device before rejecting its count");
        valid();
    }
    // A direct endpoint keeps working without relying on broadcast discovery.
    g_network_count = 255;
    const int lists = g_network_lists, opens = g_opens, closes = g_closes;
    {
        SoapyHarogic direct(SoapySDR::Kwargs{{"interface", "ETH"}, {"ip", "192.168.1.100"}});
        expect(g_network_lists == lists && g_opens == opens + 1,
               "direct ETH endpoint unexpectedly used discovery");
    }
    expect(g_closes == closes + 1, "direct ETH constructor leaked its handle");
    g_network_count = 0;
}

static void check_gain_bounds() {
    SoapyHarogic device(SoapySDR::Kwargs{{"device", "2"}});
    auto *stream = device.setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, {{"native_format", "CS8"}});
    const double nan = std::numeric_limits<double>::quiet_NaN();
    const double inf = std::numeric_limits<double>::infinity();
    for (bool active : {false, true}) {
        if (active) expect(device.activateStream(stream) == 0, "gain-bound stream activation failed");
        for (double value : {nan, inf, -inf, 1e300, -1e300,
                static_cast<double>(std::numeric_limits<int>::max()) + 1.0,
                static_cast<double>(std::numeric_limits<int>::min()) - 1.0}) {
            const int starts = g_starts, stops = g_stops, configurations = g_configurations;
            const double previous = device.getGain(SOAPY_SDR_RX, 0, "REF");
            bool rejected = false;
            try { device.setGain(SOAPY_SDR_RX, 0, "REF", value); }
            catch (const std::invalid_argument &) { rejected = true; }
            expect(rejected, "REF gain accepted nonfinite or out-of-int-range input");
            expect(device.getGain(SOAPY_SDR_RX, 0, "REF") == previous &&
                   g_starts == starts && g_stops == stops && g_configurations == configurations &&
                   g_triggered == active, "invalid REF gain mutated settings or stopped/reconfigured acquisition");
        }
        // Keep the prior truncation semantics for finite, representable values.
        for (double value : {-12.75, 15.75}) {
            const int configurations = g_configurations;
            device.setGain(SOAPY_SDR_RX, 0, "REF", value);
            expect(device.getGain(SOAPY_SDR_RX, 0, "REF") == std::trunc(value), "valid REF gain changed truncation");
            expect(g_configurations == configurations + (active ? 1 : 0),
                   "valid REF gain did not preserve live/inactive configuration behavior");
        }
        if (active) {
            std::complex<float> data[kPacketSamples];
            void *buffs[] = {data};
            int flags = 0; long long time = 0;
            expect(device.readStream(stream, buffs, kPacketSamples, flags, time, 1000000) == kPacketSamples,
                   "stream did not remain readable after rejecting invalid REF gain");
            expect(device.deactivateStream(stream) == 0 && !g_triggered, "gain-bound stream did not stop");
            expect(device.activateStream(stream) == 0, "gain-bound stream did not restart");
            expect(device.readStream(stream, buffs, kPacketSamples, flags, time, 1000000) == kPacketSamples,
                   "gain-bound stream did not produce samples after restart");
            device.deactivateStream(stream);
        }
    }
    for (double value : {static_cast<double>(std::numeric_limits<int>::min()),
                         static_cast<double>(std::numeric_limits<int>::max())}) {
        device.setGain(SOAPY_SDR_RX, 0, "REF", value);
        expect(device.getGain(SOAPY_SDR_RX, 0, "REF") == value, "representable REF gain endpoint rejected");
    }
    device.closeStream(stream);
}

static void check_sdk_bounds() {
    const double nan = std::numeric_limits<double>::quiet_NaN();
    const double inf = std::numeric_limits<double>::infinity();
    for (const double rate : {nan, inf, -inf, 0.0, -1.0, 1e300}) {
        g_native_rate = rate;
        const int opens = g_opens, closes = g_closes;
        bool rejected = false;
        try { SoapyHarogic invalid(SoapySDR::Kwargs{{"device", "2"}}); }
        catch (const std::runtime_error &) { rejected = true; }
        expect(rejected, "accepted invalid SDK native sample rate");
        expect(g_opens == opens + 1 && g_closes == closes + 1,
               "capability validation leaked the opened device");
    }
    g_native_rate = kNativeRate;
    SoapyHarogic device(SoapySDR::Kwargs{{"device", "2"}});
    const size_t mtu = device.getStreamMTU(nullptr);
    expect(mtu == 16384, "transport MTU is not stable and bounded");
    auto *stream = device.setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, {{"native_format", "CS32"}});
    const auto reject_configuration = [&] {
        const int starts = g_starts, fetches = g_fetches, stops = g_stops;
        g_excess_allocation = false;
        g_watch_allocations = true;
        const int result = device.activateStream(stream);
        g_watch_allocations = false;
        expect(result == SOAPY_SDR_STREAM_ERROR, "accepted invalid SDK stream geometry");
        expect(!g_excess_allocation, "SDK geometry reached oversized allocation before validation");
        expect(g_starts == starts && g_fetches == fetches && g_stops > stops && !g_triggered,
               "rejected geometry started acquisition or failed to stop it");
        expect(device.getStreamMTU(stream) == mtu, "failed activation changed the advertised MTU");
    };
    g_override_rate = true;
    for (double rate : {nan, inf, -inf, 0.0, -1.0, 1e-300, 1e300, 200000000.0}) {
        g_configured_rate = rate;
        reject_configuration();
    }
    g_override_rate = false;
    for (uint32_t samples : {0u, 1048577u, UINT32_MAX}) {
        g_configured_samples = samples;
        reject_configuration();
    }
    g_configured_samples = kPacketSamples;
    for (int64_t bytes : {0LL, 31LL, 33LL, static_cast<long long>(UINT32_MAX)}) {
        g_configured_bytes = bytes;
        reject_configuration();
    }
    g_configured_bytes = -1;
    g_configured_format = Complex8bit;
    reject_configuration();
    g_configured_format = -1;
    device.closeStream(stream);

    // An otherwise supported high-rate profile can still exceed the total byte
    // budget in CS32. This must be rejected before allocating a larger ring.
    g_native_rate = 200000000.0;
    {
        SoapyHarogic high_rate(SoapySDR::Kwargs{{"device", "2"}});
        auto *wide_stream = high_rate.setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, {{"native_format", "CS32"}});
        g_excess_allocation = false;
        g_watch_allocations = true;
        expect(high_rate.activateStream(wide_stream) == SOAPY_SDR_STREAM_ERROR,
               "accepted ring larger than stream byte budget");
        g_watch_allocations = false;
        expect(!g_excess_allocation, "byte budget was enforced only by allocator failure");
    }
    g_native_rate = kNativeRate;
    g_extrema = true;
    stream = device.setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, {{"native_format", "CS16"}});
    const auto read_packet = [&](int expected) {
        std::complex<float> data[kPacketSamples + 1];
        const std::complex<float> sentinel(123, -456);
        std::fill(std::begin(data), std::end(data), sentinel);
        void *buffs[] = {data};
        int flags = 0;
        long long time = 0;
        expect(device.readStream(stream, buffs, kPacketSamples, flags, time, 1000000) == expected,
               "packet validation returned the wrong stream state");
        const size_t untouched = expected < 0 ? 0 : kPacketSamples;
        for (size_t i = untouched; i < std::size(data); ++i)
            expect(data[i] == sentinel, "malformed packet or bounded read modified destination");
    };
    for (int fault = 0; fault < 9; ++fault) {
        g_packet_samples = fault == 0 ? 0 : fault == 1 ? 5 : fault == 2 ? UINT32_MAX : kPacketSamples;
        g_packet_bytes = fault == 3 ? 0 : fault == 4 ? 15 : fault == 5 ? 17 : -1;
        g_packet_format = fault == 6 ? Complex8bit : fault == 7 ? Real16bit : -1;
        g_null_packet = fault == 8;
        expect(device.activateStream(stream) == 0, "bad-packet activation failed");
        read_packet(SOAPY_SDR_STREAM_ERROR);
        const int stops = g_stops;
        expect(device.deactivateStream(stream) == 0 && g_stops > stops && !g_triggered,
               "bad-packet stop did not release acquisition");
        const int fetches = g_fetches;
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
        expect(g_fetches == fetches, "stopped SDK worker kept fetching");
        g_packet_samples = kPacketSamples; g_packet_bytes = -1; g_packet_format = -1; g_null_packet = false;
        expect(device.activateStream(stream) == 0, "valid stream did not recover after malformed packet");
        read_packet(kPacketSamples);
        device.deactivateStream(stream);
    }
    device.closeStream(stream);
    g_extrema = false;
}

int main(int argc, char **argv) {
    // Isolate malformed SDK inputs for sanitizer RED/GREEN runs as well as the full contract.
    if (argc == 2) {
        const std::string selected = argv[1];
        if (selected == "gain-bounds") check_gain_bounds();
        else if (selected == "network-discovery-bounds") check_network_bounds(false);
        else if (selected == "network-constructor-bounds") check_network_bounds(true);
        else fail("unknown focused contract " + selected);
        expect(g_opens == g_closes, "focused boundary check leaked a device handle");
        std::cout << "soapy_stream_format_check: " << selected << " ok\n";
        return 0;
    }
    expect(argc == 1, "unexpected contract arguments");
    check_ring();
    check_discovery();
    g_fail_open = true;
    bool open_threw = false;
    try { SoapyHarogic failed(SoapySDR::Kwargs{{"device", "2"}}); }
    catch (const std::runtime_error &) { open_threw = true; }
    g_fail_open = false;
    expect(open_threw && g_opens == 0 && g_closes == 0,
           "failed Device_Open did not reject its unusable nonnull handle");
    std::unique_ptr<SoapySDR::Device> device(new SoapyHarogic(SoapySDR::Kwargs{{"serial", "B0B"}}));
    expect(g_last_device_index == 7, "constructor serial selection ignored Device_List indices");
    expect(g_opens == 1 && g_closes == 0, "constructor did not retain its single device handle");
    expect(g_last_power == DevicePowerSupply_TypeDef::USBPortAndPowerPort, "constructor did not select streaming power");
    const int retained_opens = g_opens;
    const int retained_closes = g_closes;

    const auto formats = device->getStreamFormats(SOAPY_SDR_RX, 0);
    expect(formats == std::vector<std::string>({SOAPY_SDR_CF32, SOAPY_SDR_CS16, SOAPY_SDR_CS8}),
           "getStreamFormats does not advertise implemented output widths");
    expect_native(*device, SOAPY_SDR_CS8, 128.0);

    bool native_format_advertised = false;
    for (const auto &arg : device->getStreamArgsInfo(SOAPY_SDR_RX, 0)) {
        if (arg.key != "native_format") continue;
        native_format_advertised = true;
        expect(std::find(arg.options.begin(), arg.options.end(), "CS8") != arg.options.end(),
               "native_format does not offer the SDK CS8 format exercised below");
    }
    expect(native_format_advertised, "native_format stream argument missing");

    for (const char *rejected : {"CS32", "CU8"}) expect_rejected(*device, rejected);

    SoapySDR::Kwargs args;
    args["native_format"] = "CS8";
    auto *stream = device->setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, args);
    expect(stream != nullptr, "CF32 setupStream returned null");
    expect_native(*device, SOAPY_SDR_CF32, 1.0);

    // The setup MTU bounds the remote consumer's allocation.
    const size_t cached_mtu = device->getStreamMTU(stream);
    expect(cached_mtu > 0, "getStreamMTU returned zero before activation");

    // The remote sender reads immediately after setup, before activateStream.
    // It must see a timed wait, not a fatal error that makes OnAir stop its source.
    const std::complex<float> inactive_sentinel(12345.0f, -23456.0f);
    std::complex<float> inactive_destination[kPacketSamples];
    std::fill(std::begin(inactive_destination), std::end(inactive_destination), inactive_sentinel);
    void *inactive_buffs[] = {inactive_destination};
    int inactive_flags = 0;
    long long inactive_time_ns = 0;
    constexpr long inactive_timeout_us = 20000;
    const auto inactive_start = std::chrono::steady_clock::now();
    const int inactive_count = device->readStream(stream, inactive_buffs, kPacketSamples,
                                                 inactive_flags, inactive_time_ns, inactive_timeout_us);
    const auto inactive_elapsed = std::chrono::steady_clock::now() - inactive_start;
    expect(inactive_count == SOAPY_SDR_TIMEOUT, "inactive readStream did not return TIMEOUT");
    expect(inactive_elapsed >= std::chrono::microseconds(inactive_timeout_us),
           "inactive readStream returned before its timeout");
    expect(inactive_elapsed < std::chrono::seconds(2), "inactive readStream exceeded its timeout bound");
    for (const auto &sample : inactive_destination)
        expect(sample == inactive_sentinel, "inactive readStream modified its destination");

    expect(device->activateStream(stream) == 0, "activateStream failed");
    expect(g_last_data_format == Complex8bit, "native_format CS8 did not select SDK Complex8bit");
    const int unchanged_configurations = g_configurations;
    device->setFrequency(SOAPY_SDR_RX, 0, "RF", device->getFrequency(SOAPY_SDR_RX, 0, "RF"));
    device->setGain(SOAPY_SDR_RX, 0, "REF", device->getGain(SOAPY_SDR_RX, 0, "REF"));
    device->setAntenna(SOAPY_SDR_RX, 0, device->getAntenna(SOAPY_SDR_RX, 0));
    device->setSampleRate(SOAPY_SDR_RX, 0, device->getSampleRate(SOAPY_SDR_RX, 0));
    device->writeSetting("gain_strategy", device->readSetting("gain_strategy"));
    device->writeSetting("lo_mode", device->readSetting("lo_mode"));
    expect(g_configurations == unchanged_configurations, "unchanged live setting reconfigured SDK");

    // A destination sized for the advertised CF32 output must hold all four
    // complex samples and leave the trailing canary unchanged.
    alignas(std::complex<float>) unsigned char destination[sizeof(std::complex<float>) * kPacketSamples + 16];
    const unsigned char canary[16] = {
        0xA5, 0x5A, 0xA5, 0x5A, 0xA5, 0x5A, 0xA5, 0x5A,
        0xA5, 0x5A, 0xA5, 0x5A, 0xA5, 0x5A, 0xA5, 0x5A,
    };
    std::memset(destination, 0x11, sizeof(std::complex<float>) * kPacketSamples);
    std::memcpy(destination + sizeof(std::complex<float>) * kPacketSamples, canary, sizeof(canary));

    void *buffs[] = {destination};
    int flags = 0;
    long long time_ns = 0;
    const int count = device->readStream(stream, buffs, kPacketSamples, flags, time_ns, 1000000);
    expect(count == kPacketSamples, "readStream did not return the stub packet");
    expect(std::memcmp(destination + sizeof(std::complex<float>) * kPacketSamples, canary, sizeof(canary)) == 0,
           "readStream wrote past the complex-float packet");

    auto *samples = reinterpret_cast<std::complex<float> *>(destination);
    for (int i = 0; i < kPacketSamples; ++i) {
        const float expect_i = static_cast<float>(10 + i) / 128.0f;
        const float expect_q = static_cast<float>(-20 - i) / 128.0f;
        expect(samples[i].real() == expect_i && samples[i].imag() == expect_q,
               "readStream did not convert SDK CS8 bytes into complex float");
        expect(std::fabs(samples[i].real()) <= 1.0f && std::fabs(samples[i].imag()) <= 1.0f,
               "converted sample is outside the normalized CF32 range");
    }

    // One SDK packet must satisfy bounded reads without waiting for a full request.
    device->deactivateStream(stream);
    g_packets_remaining = 1;
    expect(device->activateStream(stream) == 0, "partial-read activation failed");
    const std::complex<float> sentinel(12345.0f, -23456.0f);
    std::complex<float> partial[kPacketSamples * 2 + 2];
    std::fill(std::begin(partial), std::end(partial), sentinel);
    void *partial_buffs[] = {partial};
    const auto partial_start = std::chrono::steady_clock::now();
    expect(device->readStream(stream, partial_buffs, kPacketSamples * 2, flags, time_ns, 1000000) == kPacketSamples,
           "readStream did not return a packet shorter than the request");
    expect(std::chrono::steady_clock::now() - partial_start < std::chrono::milliseconds(500),
           "partial read waited for its deadline");
    for (int i = 0; i < kPacketSamples; ++i)
        expect(partial[i] == std::complex<float>(static_cast<float>(10 + i) / 128.0f,
                                                static_cast<float>(-20 - i) / 128.0f),
               "partial read changed packet samples");
    for (size_t i = kPacketSamples; i < std::size(partial); ++i)
        expect(partial[i] == sentinel, "partial read wrote beyond available samples");
    std::fill(std::begin(partial), std::end(partial), sentinel);
    g_packets_remaining = 1;
    expect(device->readStream(stream, partial_buffs, 3, flags, time_ns, 1000000) == 3,
           "readStream exceeded or failed to fill a request smaller than one SDK packet");
    for (size_t i = 3; i < std::size(partial); ++i)
        expect(partial[i] == sentinel, "bounded read modified samples beyond numElems");
    for (int i = 0; i < 3; ++i)
        expect(partial[i] == std::complex<float>(static_cast<float>(10 + i) / 128.0f,
                                                static_cast<float>(-20 - i) / 128.0f),
               "bounded read changed packet samples");
    std::fill(std::begin(partial), std::end(partial), sentinel);
    expect(device->readStream(stream, partial_buffs, kPacketSamples * 2, flags, time_ns, 1000000) == 1,
           "readStream did not return the available packet remainder");
    expect(partial[0] == std::complex<float>(13.0f / 128.0f, -23.0f / 128.0f),
           "partial read lost packet ordering");
    for (size_t i = 1; i < std::size(partial); ++i)
        expect(partial[i] == sentinel, "partial read wrote beyond available samples");
    device->deactivateStream(stream);
    g_packets_remaining = -1;
    expect(device->activateStream(stream) == 0, "reactivation failed");
    device->deactivateStream(stream);

    const auto deactivated_start = std::chrono::steady_clock::now();
    expect(device->readStream(stream, inactive_buffs, kPacketSamples,
                              inactive_flags, inactive_time_ns, inactive_timeout_us) == SOAPY_SDR_TIMEOUT,
           "deactivated readStream did not return TIMEOUT");
    expect(std::chrono::steady_clock::now() - deactivated_start >= std::chrono::microseconds(inactive_timeout_us),
           "deactivated readStream returned before its timeout");
    device->closeStream(stream);
    const int inactive_configurations = g_configurations;
    device->setSampleRate(SOAPY_SDR_RX, 0, 15000000.0);
    expect(device->getSampleRate(SOAPY_SDR_RX, 0) == kNativeRate / 8, "unsupported rate was not mapped to advertised rate");
    device->setFrequency(SOAPY_SDR_RX, 0, "RF", 101000000.0);
    expect(g_configurations == inactive_configurations, "inactive settings were not batched");
    expect_native(*device, SOAPY_SDR_CS16, 32768.0);

    // A fatal SDK failure still reports STREAM_ERROR instead of being mistaken
    // for an inactive stream. closeStream above left the ring empty.
    stream = device->setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, args);
    g_fail_fetch = true;
    expect(device->activateStream(stream) == 0, "fatal-fetch setup activation failed");
    expect(device->readStream(stream, inactive_buffs, kPacketSamples,
                              inactive_flags, inactive_time_ns, 1000000) == SOAPY_SDR_STREAM_ERROR,
           "fatal SDK fetch did not return STREAM_ERROR");
    device->deactivateStream(stream);
    device->closeStream(stream);

    // Observe the fatal state without consuming its residual packet, then race stop.
    for (int attempt = 0; attempt < 32; ++attempt) {
        stream = device->setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, args);
        g_packets_before_failure = 1;
        g_fatal_fetch_returned = false;
        expect(device->activateStream(stream) == 0, "concurrent-stop activation failed");
        const auto fatal_deadline = std::chrono::steady_clock::now() + std::chrono::seconds(1);
        int fatal_state = 0;
        while (fatal_state != SOAPY_SDR_STREAM_ERROR && std::chrono::steady_clock::now() < fatal_deadline) {
            fatal_state = device->readStream(stream, inactive_buffs, 0, inactive_flags, inactive_time_ns, 1000);
            std::this_thread::yield();
        }
        expect(g_fatal_fetch_returned && fatal_state == SOAPY_SDR_STREAM_ERROR,
               "fatal fetch state was not published");
        expect(device->readStream(stream, inactive_buffs, kPacketSamples,
                                  inactive_flags, inactive_time_ns, 1000000) == SOAPY_SDR_STREAM_ERROR,
               "fatal fetch returned residual samples before STREAM_ERROR");
        for (const auto &sample : inactive_destination)
            expect(sample == inactive_sentinel, "fatal read modified its destination");
        std::complex<float> concurrent_destination[kPacketSamples * 2];
        std::fill(std::begin(concurrent_destination), std::end(concurrent_destination), inactive_sentinel);
        std::atomic<bool> reader_started(false);
        int concurrent_count = 0;
        std::thread reader([&] {
            void *concurrent_buffs[] = {concurrent_destination};
            int concurrent_flags = 0;
            long long concurrent_time_ns = 0;
            reader_started = true;
            concurrent_count = device->readStream(stream, concurrent_buffs, kPacketSamples * 2,
                                                  concurrent_flags, concurrent_time_ns, inactive_timeout_us);
        });
        const auto coordination_deadline = std::chrono::steady_clock::now() + std::chrono::seconds(1);
        while ((!g_fatal_fetch_returned || !reader_started) &&
               std::chrono::steady_clock::now() < coordination_deadline)
            std::this_thread::yield();
        expect(g_fatal_fetch_returned && reader_started, "concurrent-stop participants did not start");
        device->deactivateStream(stream);
        reader.join();
        expect(concurrent_count == SOAPY_SDR_STREAM_ERROR || concurrent_count == SOAPY_SDR_TIMEOUT,
               "concurrent fatal read/deactivate returned zero or residual samples");
        for (const auto &sample : concurrent_destination)
            expect(sample == inactive_sentinel, "concurrent fatal read/deactivate modified its destination");
        device->closeStream(stream);
    }
    g_fail_fetch = false;
    g_extrema = true;
    g_fetch_warning = APIRETVAL_WARNING_IFOverflow;
    check_output(*device, SOAPY_SDR_CS8, g_iq8, 128.0);
    check_output(*device, SOAPY_SDR_CS16, g_iq16, 32768.0);
    g_fetch_warning = 0;
    stream = device->setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, {{"native_format", "CS32"}});
    expect(device->activateStream(stream) == 0, "CS32 SDK activation failed");
    std::complex<float> wide[kPacketSamples];
    void *wide_buffs[] = {wide};
    expect(device->readStream(stream, wide_buffs, kPacketSamples, flags, time_ns, 1000000) == kPacketSamples,
           "CS32 SDK read failed");
    for (size_t i = 0; i < kPacketSamples; ++i)
        expect(wide[i] == std::complex<float>(static_cast<float>(g_iq32[2*i]) / 2147483648.0f,
                                             static_cast<float>(g_iq32[2*i+1]) / 2147483648.0f),
               "CS32 extrema normalization mismatch");
    device->closeStream(stream);
    bool mismatch_threw = false;
    try { device->setupStream(SOAPY_SDR_RX, SOAPY_SDR_CS8, {0}, {{"native_format", "CS16"}}); }
    catch (const std::runtime_error &) { mismatch_threw = true; }
    expect(mismatch_threw, "mismatched integer output silently quantized SDK data");
    stream = device->setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, args);
    expect(device->activateStream(stream) == 0, "configuration-failure test activation failed");
    g_fail_configuration = true;
    bool configuration_threw = false;
    try { device->setFrequency(SOAPY_SDR_RX, 0, "RF", 102000000.0); }
    catch (const std::runtime_error &) { configuration_threw = true; }
    expect(configuration_threw, "live configuration failure was swallowed");
    device->closeStream(stream);
    g_fail_configuration = false;
    stream = device->setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, args);
    g_fail_configuration = true;
    expect(device->activateStream(stream) == SOAPY_SDR_STREAM_ERROR,
           "initial configuration failure did not propagate");
    device->closeStream(stream);
    stream = device->setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, args);
    const auto recovery_start = std::chrono::steady_clock::now();
    expect(device->readStream(stream, inactive_buffs, kPacketSamples, inactive_flags,
                              inactive_time_ns, inactive_timeout_us) == SOAPY_SDR_TIMEOUT,
           "fresh inactive stream retained previous activation failure");
    expect(std::chrono::steady_clock::now() - recovery_start >= std::chrono::microseconds(inactive_timeout_us),
           "fresh inactive stream returned before its read timeout");
    g_fail_configuration = false;
    expect(device->activateStream(stream) == 0, "activation did not recover after configuration failure");
    expect(device->readStream(stream, inactive_buffs, kPacketSamples, inactive_flags,
                              inactive_time_ns, 1000000) == kPacketSamples,
           "recovered stream did not return samples");
    device->closeStream(stream);
    expect(g_opens == retained_opens && g_closes == retained_closes,
           "stream reuse reopened or released the retained device handle");
    device.reset();
    expect(g_closes == retained_closes + 1, "destructor did not release the retained device handle");
    const int opens_before_recreation = g_opens;
    const int closes_before_recreation = g_closes;
    device.reset(new SoapyHarogic(SoapySDR::Kwargs{{"device", "2"}}));
    stream = device->setupStream(SOAPY_SDR_RX, SOAPY_SDR_CF32, {0}, args);
    expect(device->activateStream(stream) == 0, "recreated device activation failed");
    expect(g_opens == opens_before_recreation + 1 && g_closes == closes_before_recreation && g_last_device_index == 2,
           "recreated device did not reuse its constructor handle");
    device.reset();
    expect(g_opens == g_closes, "active device destruction leaked its handle");
    check_sdk_bounds();
    expect(g_opens == g_closes, "SDK boundary checks leaked device handles");
    check_gain_bounds();
    check_network_bounds(false);
    check_network_bounds(true);
    expect(g_opens == g_closes, "control/discovery boundary checks leaked device handles");
    std::cout << "soapy_stream_format_check: ok\n";
    return 0;
}
