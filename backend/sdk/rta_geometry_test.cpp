// Production RTA ABI exercised with SDK stubs. No physical device is opened.
// Geometry is deliberately malformed before any allocation or acquisition.
#include "san90_sdk.cpp"
#include <atomic>
#include <cstddef>
#include <cstdlib>
#include <limits>

namespace {
constexpr size_t budget = 64u * 1024u * 1024u;
std::atomic<bool> track_allocations{false};
std::atomic<size_t> live_bytes{0}, peak_bytes{0}, blocked_allocations{0};
struct alignas(std::max_align_t) Allocation { size_t tracked; };
}

// Refuse unsafe allocations even against the unfixed shim. Tracking large
// allocations also catches retained vector capacity across valid reconfigures.
void* operator new(size_t size) {
    const bool tracked = track_allocations && size >= 4096;
    if (tracked) {
        const size_t previous = live_bytes.fetch_add(size);
        if (size > budget || previous > budget - size) {
            live_bytes.fetch_sub(size);
            ++blocked_allocations;
            throw std::bad_alloc();
        }
        size_t peak = peak_bytes.load();
        while (peak < previous + size && !peak_bytes.compare_exchange_weak(peak, previous + size)) {}
    }
    if (size > std::numeric_limits<size_t>::max() - sizeof(Allocation)) {
        if (tracked) live_bytes.fetch_sub(size);
        throw std::bad_alloc();
    }
    auto* allocation = static_cast<Allocation*>(std::malloc(sizeof(Allocation) + size));
    if (!allocation) {
        if (tracked) live_bytes.fetch_sub(size);
        throw std::bad_alloc();
    }
    allocation->tracked = tracked ? size : 0;
    return allocation + 1;
}
void operator delete(void* pointer) noexcept {
    if (!pointer) return;
    auto* allocation = static_cast<Allocation*>(pointer) - 1;
    live_bytes.fetch_sub(allocation->tracked);
    std::free(allocation);
}
void operator delete(void* pointer, size_t) noexcept { ::operator delete(pointer); }
void* operator new[](size_t size) { return ::operator new(size); }
void operator delete[](void* pointer) noexcept { ::operator delete(pointer); }
void operator delete[](void* pointer, size_t) noexcept { ::operator delete(pointer); }

static int physical_device, opened = 0, closed = 0, starts = 0, stops = 0;
static int failures = 0, candidate_error = 0;
static bool inject = false, reject_restore = false;
static constexpr double candidate_center = 102e6;
static RTA_FrameInfo_TypeDef good{}, malformed{};

static void check(bool condition, const char* operation, const char* reason) {
    if (!condition) {
        std::fprintf(stderr, "%s: %s\n", operation, reason);
        ++failures;
    }
}

extern "C" int Device_Open(void** slot, int, const BootProfile_TypeDef*, BootInfo_TypeDef* info) {
    ++opened;
    *slot = &physical_device;
    *info = {};
    return 0;
}
extern "C" int Device_GetHardwareState(void**, HardWareState_TypeDef* state) { *state = {}; return 0; }
extern "C" int Device_Close(void** slot) { ++closed; *slot = nullptr; return 0; }
extern "C" int RTA_ProfileDeInit(void**, RTA_Profile_TypeDef* profile) { *profile = {}; return 0; }
extern "C" int RTA_Configuration(void**, const RTA_Profile_TypeDef* input,
        RTA_Profile_TypeDef* output, RTA_FrameInfo_TypeDef* geometry) {
    if (inject && input->CenterFreq_Hz == candidate_center && candidate_error) return candidate_error;
    *output = *input;
    output->RBW_Hz = 1000;
    output->VBW_Hz = 1000;
    output->ReferenceClockFrequency = 10e6;
    *geometry = inject && (input->CenterFreq_Hz == candidate_center || reject_restore) ? malformed : good;
    return 0;
}
extern "C" int RTA_BusTriggerStart(void**) { ++starts; return 0; }
extern "C" int RTA_BusTriggerStop(void**) { ++stops; return 0; }

static RTA_FrameInfo_TypeDef normal_geometry() {
    RTA_FrameInfo_TypeDef frame{};
    frame.FrameWidth = 1024;
    frame.FrameHeight = 256;
    frame.PacketValidPoints = 1024;
    frame.PacketSamplePoints = 2048;
    frame.StartFrequency_Hz = 99e6;
    frame.StopFrequency_Hz = 101e6;
    frame.POI = 0.001;
    return frame;
}

static San90RtaRequest request(double center = 100e6) {
    San90RtaRequest result{};
    result.center_hz = center;
    result.decimate = 1;
    result.trigger_source = Level;
    result.trigger_acq_time = 0.005;
    return result;
}

static void rejected_open(const char* name, bool legacy) {
    const auto allocations_before = blocked_allocations.load();
    const int starts_before = starts;
    San90RtaDevice* state = nullptr;
    San90RtaActual actual{};
    double start = 0, stop = 0;
    uint32_t points = 0;
    auto candidate = request(candidate_center);
    const int status = legacy
        ? san90_rta_open_v1(&state, candidate_center, 50e6, &start, &stop, &points)
        : san90_rta_open_v2(&state, &candidate, &actual);
    check(status == -102, name, "malformed geometry did not return -102");
    check(blocked_allocations == allocations_before, name, "rejection happened after an unsafe allocation attempt");
    check(starts == starts_before, name, "acquisition started with malformed geometry");
    if (state) san90_rta_close_v1(state);
    check(san90_session.call(nullptr, [] { return san90_session.active ? 1 : 0; }) == 0,
        name, "failed open retained a mode owner");
    check(san90_session_release_v1() == 0, name, "failed open prevented physical release");
}

int main() {
    good = normal_geometry();
    track_allocations = true;
    inject = true;
    for (bool legacy : {false, true}) {
        malformed = good;
        malformed.PacketSamplePoints = UINT32_MAX;
        rejected_open(legacy ? "legacy sample overflow" : "sample overflow", legacy);
        malformed = good;
        malformed.PacketValidPoints = UINT32_MAX;
        rejected_open(legacy ? "legacy valid overflow" : "valid overflow", legacy);
        malformed = good;
        malformed.FrameWidth = malformed.FrameHeight = 65536;
        malformed.PacketValidPoints = malformed.PacketSamplePoints = 65536;
        rejected_open(legacy ? "legacy bitmap budget" : "bitmap budget", legacy);
        malformed = good;
        malformed.FrameWidth = 65536;
        malformed.FrameHeight = 32;
        malformed.PacketValidPoints = 65536;
        malformed.PacketSamplePoints = 60u * 1024u * 1024u;
        rejected_open(legacy ? "legacy combined budget" : "combined budget", legacy);
        for (double invalid : {NAN, INFINITY, -INFINITY}) {
            malformed = good;
            malformed.StartFrequency_Hz = invalid;
            rejected_open(legacy ? "legacy nonfinite start" : "nonfinite start", legacy);
            malformed = good;
            malformed.StopFrequency_Hz = invalid;
            rejected_open(legacy ? "legacy nonfinite stop" : "nonfinite stop", legacy);
        }
        malformed = good;
        malformed.StopFrequency_Hz = malformed.StartFrequency_Hz;
        rejected_open(legacy ? "legacy empty frequency range" : "empty frequency range", legacy);
        malformed = good;
        malformed.StartFrequency_Hz = -std::numeric_limits<double>::max();
        malformed.StopFrequency_Hz = std::numeric_limits<double>::max();
        rejected_open(legacy ? "legacy frequency span overflow" : "frequency span overflow", legacy);
    }

    inject = false;
    auto initial = request();
    San90RtaDevice* state = nullptr;
    San90RtaActual actual{};
    check(san90_rta_open_v2(&state, &initial, &actual) == 0, "recovery", "valid open failed after rejected geometry");
    if (state) {
        const int opens_before = opened;
        const auto allocations_before = blocked_allocations.load();
        inject = true;
        malformed = good;
        malformed.PacketSamplePoints = UINT32_MAX;
        auto candidate = request(candidate_center);
        check(san90_rta_reconfig_v2(state, &candidate, &actual) == -102,
            "reconfigure", "invalid geometry was not rejected");
        check(blocked_allocations == allocations_before, "reconfigure", "invalid geometry reached the allocator");
        check(state->configured.CenterFreq_Hz == initial.center_hz && state->info.FrameWidth == good.FrameWidth
            && state->actual.valid && opened == opens_before, "reconfigure", "rollback did not restore the same session");
        inject = false;
        check(san90_rta_reconfig_v2(state, &initial, &actual) == 0,
            "recovery", "valid reconfiguration failed after malformed metadata");
        inject = true;
        // The SDK can also return malformed metadata while restoring the old profile.
        reject_restore = true;
        candidate_error = -7;
        const int starts_before = starts;
        check(san90_rta_reconfig_v2(state, &candidate, &actual) == -7,
            "rollback", "failed rollback lost the original configuration error");
        check(!state->actual.valid && starts == starts_before,
            "rollback", "malformed rollback metadata restarted acquisition");
        check(blocked_allocations == allocations_before, "rollback", "malformed rollback reached the allocator");
        san90_rta_close_v1(state);
        state = nullptr;
    }
    inject = reject_restore = false;
    candidate_error = 0;
    check(san90_session_release_v1() == 0, "recovery", "session did not release after rollback failure");

    // Two individually valid shapes must not retain >64 MiB of vector capacity,
    // or allocate the new large buffer before retiring the previous large one.
    good = normal_geometry();
    good.PacketSamplePoints = 40u * 1024u * 1024u;
    initial.trigger_source = Bus;
    const auto allocations_before = blocked_allocations.load();
    check(san90_rta_open_v2(&state, &initial, &actual) == 0, "reshape", "large valid trace was rejected");
    if (state) {
        good = normal_geometry();
        good.FrameWidth = good.PacketValidPoints = 4096;
        good.FrameHeight = 5120;
        check(san90_rta_reconfig_v2(state, &initial, &actual) == 0,
            "reshape", "valid bitmap replacement exceeded the live allocation budget");
        const size_t owned = state->trace.capacity() + state->bitmap.capacity() * sizeof(uint16_t)
            + state->capture_power.capacity() * sizeof(float);
        check(owned <= budget && blocked_allocations == allocations_before,
            "reshape", "retained capacities exceeded the allocation budget");
        san90_rta_close_v1(state);
    }
    check(san90_session_finish_v1() == 0, "shutdown", "SDK session did not finish");
    check(live_bytes == 0 && peak_bytes <= budget && opened == closed,
        "shutdown", "mode buffers or physical sessions leaked");
    return failures ? 1 : 0;
}
