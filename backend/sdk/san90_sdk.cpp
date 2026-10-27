// SDK 0.55.100 ABI boundary for the native SAN-90 capture service.
// Build with the official htra_api.h and libhtraapi.so.0.55.100: make sdk-shim.
#include "htra_api.h"
#include "htra_api_pnm.h"
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <new>
#include <vector>
#include <condition_variable>
#include <mutex>
#include <thread>

struct San90Device;
struct San90RtaDevice;
struct San90SwpDevice;
struct San90PnmDevice;
extern "C" int san90_close(San90Device*);
extern "C" int san90_rta_close_v1(San90RtaDevice*);
extern "C" int san90_swp_close_v1(San90SwpDevice*);
extern "C" int san90_pnm_close_v1(San90PnmDevice*);
static int san90_retire(San90Device* state) { return san90_close(state); }
static int san90_retire(San90RtaDevice* state) { return san90_rta_close_v1(state); }
static int san90_retire(San90SwpDevice* state) { return san90_swp_close_v1(state); }
static int san90_retire(San90PnmDevice* state) { return san90_pnm_close_v1(state); }

// Exactly one synchronous operation may be in flight. The caller's stack owns the
// operation and all sample buffers; this dispatcher never queues or retains IQ.
class San90Session {
    std::mutex callers_;
    std::mutex work_;
    std::condition_variable wake_;
    void* argument_ = nullptr;
    int (*operation_)(void*) = nullptr;
    int result_ = 0;
    bool stopping_ = false;
    std::thread::id owner_;
    std::thread worker_;
    void run() {
        std::unique_lock<std::mutex> lock(work_);
        owner_ = std::this_thread::get_id();
        wake_.notify_all();
        for (;;) {
            wake_.wait(lock, [&] { return operation_ || stopping_; });
            if (stopping_) return;
            auto operation = operation_;
            auto argument = argument_;
            lock.unlock();
            const void* previous_mode = active;
            int result;
            try { result = operation(argument); }
            catch (...) {
                // A throwing open has no wrapper to return to its caller. Retire
                // its partially configured mode here, including the physical
                // session; an existing caller still owns its wrapper until close.
                invalidate = true;
                if (!previous_mode && active) retire(const_cast<void*>(active));
                result = -101;
            }
            if (result == -8 || result == -9) {
                // An unplugged vendor pointer must never reach Device_Close.
                handle = nullptr;
                healthy = false;
            }
            lock.lock();
            result_ = result;
            operation_ = nullptr;
            wake_.notify_all();
        }
    }
    template <typename Call> int submit(Call& call) {
        std::unique_lock<std::mutex> lock(work_);
        if (stopping_) return -112;
        argument_ = &call;
        operation_ = [](void* argument) { return (*static_cast<Call*>(argument))(); };
        wake_.notify_all();
        wake_.wait(lock, [&] { return operation_ == nullptr; });
        return result_;
    }
public:
    // Accessed only on worker_, including mode buffer destruction and DSP calls.
    void* handle = nullptr;
    BootInfo_TypeDef boot_info{};
    HardWareState_TypeDef hardware_state{};
    int hardware_state_status = -100;
    const void* active = nullptr;
    bool healthy = false;
    bool invalidate = false;
    int (*retire)(void*) = nullptr;
    San90Session() {
        worker_ = std::thread([this] { run(); });
        std::unique_lock<std::mutex> lock(work_);
        wake_.wait(lock, [&] { return owner_ != std::thread::id{}; });
    }
    bool on_thread() const { return std::this_thread::get_id() == owner_; }
    template <typename Call> int call(const void* state, Call call) {
        std::lock_guard<std::mutex> serial(callers_);
        auto checked = [&] {
            if (state && state != active) return -110;
            return call();
        };
        return submit(checked);
    }
    bool usable() const { return healthy && handle; }
    int release(void** slot) {
        int status = 0;
        if (invalidate && usable()) status = Device_Close(&handle);
        if (invalidate) { handle = nullptr; healthy = false; }
        active = nullptr;
        invalidate = false;
        *slot = nullptr;
        return status;
    }
    void reset_idle_device() {
        handle = nullptr;
        healthy = false;
        invalidate = false;
        boot_info = {};
        hardware_state = {};
        hardware_state_status = -100;
    }
    // Closes an idle healthy handle and leaves the worker reusable. A null handle
    // owns nothing (never opened, or already released), including !healthy. A
    // non-null stale or invalidated pointer refuses and is never passed to Device_Close.
    int release_idle() {
        std::lock_guard<std::mutex> serial(callers_);
        auto close = [&] {
            if (active) return -110;
            if (!handle) return 0;
            if (invalidate || !healthy) return -111;
            int status = Device_Close(&handle);
            if (status) return status;
            reset_idle_device();
            return 0;
        };
        return submit(close);
    }
    int finish(bool force = false) {
        std::lock_guard<std::mutex> serial(callers_);
        if (!worker_.joinable()) return 0;
        auto close = [&] {
            if (active) {
                if (!force) return -110;
                retire(const_cast<void*>(active));
            }
            int status = usable() ? Device_Close(&handle) : 0;
            reset_idle_device();
            return status;
        };
        int status = submit(close);
        if (status == -110) return status;
        {
            std::lock_guard<std::mutex> lock(work_);
            stopping_ = true;
        }
        wake_.notify_all();
        worker_.join();
        return status;
    }
    ~San90Session() { finish(true); }
};

static San90Session san90_session;
#define SAN90_DISPATCH(state, expression) \
    if (!san90_session.on_thread()) \
        return san90_session.call(state, [&] { return expression; })

extern "C" int san90_session_invalidate_v1(void* state, int abandon) {
    SAN90_DISPATCH(state, san90_session_invalidate_v1(state, abandon));
    if (!state || state != san90_session.active) return -110;
    san90_session.invalidate = true;
    if (abandon) {
        san90_session.handle = nullptr;
        san90_session.healthy = false;
    }
    return 0;
}

extern "C" int san90_session_release_v1() { return san90_session.release_idle(); }
extern "C" int san90_session_finish_v1() { return san90_session.finish(); }

// Front-end and clock fields shared by the SWP, RTA and IQS profiles (v11 entry points).
// Enum values follow htra_api.h; the field mapping follows web_sa/hardware/device.py _profile.
struct San90FrontEnd {
    double ref_dbm;
    int atten;          // -1 = auto
    int preamp;         // 0 = AutoOn, 1 = ForcedOff
    int ifgain;
    int gain_strategy;  // 0 = LowNoisePreferred, 1 = HighLinearityPreferred
    int ref_clock;      // ReferenceClockSource_TypeDef
    int refclk_out;
};

// Dedicated v1 query layout; never extend an existing configure output in place.
struct San90FrontEndActual {
    double ref_dbm;
    int atten;
    int preamp;  // AutoOn / ForcedOff configuration, not physical switch state
    int ifgain;
};

template <typename Profile>
static San90FrontEndActual san90_front_end_actual(const Profile& p) {
    return {p.RefLevel_dBm, static_cast<int>(p.Atten),
            static_cast<int>(p.Preamplifier), static_cast<int>(p.IFGainGrade)};
}

template <typename Profile>
static void san90_apply_front_end(Profile& p, const San90FrontEnd& fe) {
    p.RefLevel_dBm = fe.ref_dbm;
    p.Atten = static_cast<int8_t>(fe.atten);
    p.Preamplifier = fe.preamp == 0 ? AutoOn : ForcedOff;
    p.IFGainGrade = static_cast<uint8_t>(fe.ifgain);
    p.GainStrategy = fe.gain_strategy == 0 ? LowNoisePreferred : HighLinearityPreferred;
    p.ReferenceClockSource = static_cast<ReferenceClockSource_TypeDef>(fe.ref_clock);
    p.ExternalSystemClockFrequency = 10e6;
    // An external reference needs the declared 10 MHz input, else the PLL stays unlocked
    // (measured timebase error ~10 %, Python device.py _profile).
    if (fe.ref_clock == ReferenceClockSource_External ||
        fe.ref_clock == ReferenceClockSource_External_Forced)
        p.ReferenceClockFrequency = 10e6;
    p.EnableReferenceClockOut = fe.refclk_out ? 1 : 0;
}

template <typename State>
static int san90_open_device(State* state) {
    if (san90_session.active) return -110;
    if (!san90_session.usable()) {
        BootProfile_TypeDef boot{};
        BootInfo_TypeDef boot_info{};
        boot.DevicePowerSupply = USBPortAndPowerPort;
        boot.PhysicalInterface = USB;
        // Vendor PNM workers retain this slot until Device_Close completes.
        int status = Device_Open(&san90_session.handle, 0, &boot, &boot_info);
        if (status && status != -49) {
            san90_session.handle = nullptr;
            return status;
        }
        san90_session.healthy = true;
        san90_session.boot_info = boot_info;
        san90_session.hardware_state = {};
        san90_session.hardware_state_status =
            Device_GetHardwareState(&san90_session.handle, &san90_session.hardware_state);
        if (san90_session.hardware_state_status == -8 || san90_session.hardware_state_status == -9) {
            san90_session.handle = nullptr;
            san90_session.healthy = false;
            return san90_session.hardware_state_status;
        }
    }
    state->handle = san90_session.handle;
    san90_session.active = state;
    san90_session.retire = [](void* active) {
        return san90_retire(static_cast<State*>(active));
    };
    return 0;
}

static void san90_open_failed(int status) {
    san90_session.invalidate = true;
    if (status == -8 || status == -9) {
        san90_session.handle = nullptr;
        san90_session.healthy = false;
    }
}

// -10 BusTimeOut and -11 BusDownLoad: the vendor remedy is to call Configuration again.
template <typename Call>
static int san90_retry_bus(Call call) {
    int status = 0;
    for (int attempt = 0; attempt < 5; ++attempt) {
        status = call();
        if (status != -10 && status != -11) return status;
    }
    return status;
}
// Optional v1 getter: per-call timing survives failure without changing PacketInfo
// or any existing exported function signature. Only the IQS owner accesses it.
struct San90IqsTiming {
    uint64_t entry_interval_ns = 0;
    uint64_t fetch_ns = 0;
    uint64_t memcpy_ns = 0;
    uint64_t fft_ns = 0;
    uint32_t measured = 0;  // bits 0..3: interval, fetch, memcpy, FFT
    int32_t fetch_status = 0;
    int32_t fft_status = 0;
};
using San90Clock = std::chrono::steady_clock;
static uint64_t san90_elapsed_ns(San90Clock::time_point start,
                               San90Clock::time_point end = San90Clock::now()) {
    return static_cast<uint64_t>(
        std::chrono::duration_cast<std::chrono::nanoseconds>(end - start).count());
}

struct San90Device {
    void* handle = nullptr;
    void* dsp = nullptr;
    uint32_t fft_points = 0;
    double center_hz = 0;
    double sample_rate_hz = 62500000.0;
    double bandwidth_hz = 0;
    bool triggered = false;
    double refclk_hz = 0;
    int refclk_src = 0;
    int warning = 0;
    San90FrontEndActual front_end_actual{};
    San90IqsTiming timing{};
    San90Clock::time_point last_iqs_entry{};
    bool has_iqs_entry = false;
};

struct San90PacketInfo {
    uint64_t stamp;
    uint32_t samples;
    float scale_to_v;
};

// Memory-only getters: configuration records the SDK's resolved clock source.
extern "C" int san90_iqs_clock_v1(San90Device* state, double* refclk_hz, int* refclk_src) {
    SAN90_DISPATCH(state, san90_iqs_clock_v1(state, refclk_hz, refclk_src));
    if (!san90_session.usable()) return -8;
    if (!state || !refclk_hz || !refclk_src) return -100;
    *refclk_hz = state->refclk_hz;
    *refclk_src = state->refclk_src;
    return 0;
}

extern "C" int san90_iqs_front_end_actual_v1(San90Device* state, San90FrontEndActual* actual) {
    SAN90_DISPATCH(state, san90_iqs_front_end_actual_v1(state, actual));
    if (!san90_session.usable()) return -8;
    if (!state || !actual || san90_session.active != state) return -100;
    *actual = state->front_end_actual;
    return 0;
}

extern "C" int san90_clock_capabilities_v1(void* state, int* ocxo_type, int* internal_ocxo) {
    SAN90_DISPATCH(state, san90_clock_capabilities_v1(state, ocxo_type, internal_ocxo));
    if (!san90_session.usable()) return -8;
    if (!state || !ocxo_type || !internal_ocxo) return -100;
    if (san90_session.hardware_state_status) return san90_session.hardware_state_status;
    *ocxo_type = static_cast<int>(san90_session.hardware_state.OCXOType);
    *internal_ocxo = san90_session.hardware_state.InternalOCXO;
    return 0;
}

static int san90_iqs_configure(San90Device* state, double center_hz, const San90FrontEnd& fe,
                               double* usable_bandwidth, double* configured_center,
                               int device_decimate) {
    if (state->triggered) {
        int status = IQS_BusTriggerStop(&state->handle);
        if (status == -8 || status == -9) return status;
        state->triggered = false;
    }
    SWP_Profile_TypeDef reset{}, reset_out{};
    SWP_TraceInfo_TypeDef reset_trace{};
    int status = SWP_ProfileDeInit(&state->handle, &reset);
    if (status) return status;
    status = san90_retry_bus(
        [&] { return SWP_Configuration(&state->handle, &reset, &reset_out, &reset_trace); });
    if (status) return status;
    IQS_Profile_TypeDef in{}, configured{};
    IQS_StreamInfo_TypeDef stream{};
    status = IQS_ProfileDeInit(&state->handle, &in);
    if (status) return status;
    in.CenterFreq_Hz = center_hz;
    in.DecimateFactor = device_decimate > 0 ? device_decimate : 1;
    in.DataFormat = Complex16bit;
    in.TriggerSource = Bus;
    in.TriggerMode = Adaptive;
    // 250 ms matches Python; a longer timeout was measured to be strictly worse
    // (the device stops delivering, so the extra wait only lengthens each stall).
    in.BusTimeout_ms = 250;
    in.DCCancelerMode = DCCAutoOffsetMode;
    in.QDCMode = QDCOff;
    san90_apply_front_end(in, fe);
    status = san90_retry_bus(
        [&] { return IQS_Configuration(&state->handle, &in, &configured, &stream); });
    if (status) return status;
    // PacketSamples is fixed at 16240 samples per packet at every decimate
    // (measured); the sample RATE scales with the device decimate factor.
    if (stream.PacketSamples != 16240 || stream.IQSampleRate <= 0.0 ||
        std::fabs(stream.IQSampleRate * device_decimate - 62500000.0) > 1.0)
        return -102;
    if (!std::isfinite(stream.Bandwidth) || stream.Bandwidth < 300000.0 ||
        stream.Bandwidth > stream.IQSampleRate)
        return -104;
    if (!std::isfinite(configured.CenterFreq_Hz) || configured.CenterFreq_Hz < 8000.0 ||
        configured.CenterFreq_Hz > 9020000000.0)
        return -105;
    state->center_hz = configured.CenterFreq_Hz;
    state->sample_rate_hz = stream.IQSampleRate;
    state->bandwidth_hz = stream.Bandwidth;
    state->refclk_hz = configured.ReferenceClockFrequency;
    state->refclk_src = static_cast<int>(configured.ReferenceClockSource);
    state->front_end_actual = san90_front_end_actual(configured);
    *usable_bandwidth = stream.Bandwidth;
    *configured_center = configured.CenterFreq_Hz;
    return 0;
}

// v8 prepares the stream without starting acquisition. The consumer must finish
// its metadata and DSP setup before calling san90_iqs_arm_v1.
extern "C" int san90_open_v8(San90Device** out, double center_hz, const San90FrontEnd* fe,
                               int device_decimate, double* usable_bandwidth,
                               double* configured_center) {
    SAN90_DISPATCH(nullptr, san90_open_v8(out, center_hz, fe, device_decimate, usable_bandwidth, configured_center));
    if (!out || !fe || !usable_bandwidth || !configured_center || !std::isfinite(center_hz) ||
        center_hz < 9000.0 || center_hz > 9000000000.0 || !std::isfinite(fe->ref_dbm) ||
        device_decimate <= 0 || (device_decimate & (device_decimate - 1)) != 0 ||
        device_decimate > 2048)
        return -100;
    *out = nullptr;
    auto* state = new (std::nothrow) San90Device;
    if (!state) return -101;
    int status = san90_open_device(state);
    if (status) {
        // A failed Device_Open may leave an unusable slot: never Device_Close it.
        delete state;
        return status;
    }
    {
        status = DSP_Open(&state->dsp);
        if (status) goto fail;
        DSP_FFT_TypeDef in{}, configured{};
        double rbw_ratio = 0;
        status = DSP_FFT_DeInit(&in);
        if (status) goto fail;
        in.Calibration = false;
        in.DetectionRatio = 1;
        in.TraceDetector = TraceDetector_PosPeak;
        in.FFTSize = 16240;
        in.SamplePts = 16240;  // SDK input frame and FFT buffer MUST match.
        in.Intercept = 0.8f;
        in.WindowType = FlatTop;
        status = DSP_FFT_Configuration(&state->dsp, &in, &configured, &state->fft_points,
                                       &rbw_ratio);
        if (status) goto fail;
        if (state->fft_points < 2 || state->fft_points > 16240) {
            status = -106;
            goto fail;
        }
    }
    status = san90_iqs_configure(state, center_hz, *fe, usable_bandwidth, configured_center,
                                 device_decimate);
    if (status) goto fail;
    *out = state;
    return 0;
fail:
    san90_open_failed(status);
    if (san90_session.usable() && state->triggered) {
        int stopped = IQS_BusTriggerStop(&state->handle);
        if (stopped) san90_open_failed(stopped);
    }
    if (state->dsp) DSP_Close(&state->dsp);
    san90_session.release(&state->handle);
    delete state;
    return status;
}

extern "C" int san90_iqs_arm_v1(San90Device* state) {
    SAN90_DISPATCH(state, san90_iqs_arm_v1(state));
    if (!san90_session.usable()) return -8;
    if (!state) return -100;
    if (state->triggered) return 0;
    int status = IQS_BusTriggerStart(&state->handle);
    if (status) return status;
    state->triggered = true;
    return 0;
}

// Retune or re-level the live IQS stream without Device_Close/Device_Open.
// Retune, re-level or re-rate the live IQS stream without Device_Close/Device_Open.
extern "C" int san90_iqs_reconfig_v2(San90Device* state, double center_hz,
                                       const San90FrontEnd* fe, int device_decimate,
                                       double* usable_bandwidth, double* configured_center) {
    SAN90_DISPATCH(state, san90_iqs_reconfig_v2(state, center_hz, fe, device_decimate, usable_bandwidth, configured_center));
    if (!san90_session.usable()) return -8;
    if (!state || !fe || !usable_bandwidth || !configured_center ||
        !std::isfinite(center_hz) || center_hz < 9000.0 || center_hz > 9000000000.0 ||
        !std::isfinite(fe->ref_dbm) || device_decimate <= 0 ||
        (device_decimate & (device_decimate - 1)) != 0 || device_decimate > 2048)
        return -100;
    int status = san90_iqs_configure(state, center_hz, *fe, usable_bandwidth, configured_center,
                                    device_decimate);
    return status ? status : san90_iqs_arm_v1(state);
}

extern "C" int san90_next(San90Device* state, int16_t* dest, uint32_t capacity,
                            San90PacketInfo* info) {
    SAN90_DISPATCH(state, san90_next(state, dest, capacity, info));
    if (!san90_session.usable()) return -8;
    if (!state || !state->triggered || !dest || !info || capacity < 16240) return -100;
    IQStream_TypeDef stream{};
    int status = IQS_GetIQStream_PM1(&state->handle, &stream);
    if (status && status != APIRETVAL_WARNING_IFOverflow) return status;
    auto n = stream.IQS_StreamInfo.PacketSamples;
    if (n != 16240 || n > capacity || !stream.AlternIQStream ||
        !std::isfinite(stream.IQS_ScaleToV)) return -103;
    std::memcpy(dest, stream.AlternIQStream, n * 2 * sizeof(int16_t));
    info->stamp = stream.IQS_TriggerInfo.SysTimerCountOfFirstDataPoint;
    info->samples = n;
    info->scale_to_v = stream.IQS_ScaleToV;
    state->warning = (status == APIRETVAL_WARNING_IFOverflow || stream.DeviceState.IFOverflow)
        ? APIRETVAL_WARNING_IFOverflow : 0;
    return 0;
}

extern "C" int san90_iqs_timing_v1(const San90Device* state, San90IqsTiming* timing) {
    SAN90_DISPATCH(state, san90_iqs_timing_v1(state, timing));
    if (!state || !timing) return -100;
    *timing = state->timing;
    return 0;
}


extern "C" int san90_next_v2(San90Device* state, int16_t* dest, uint32_t capacity,
                                San90PacketInfo* info, double* freq, float* power,
                                uint32_t spectrum_capacity, uint32_t* spectrum_points) {
    SAN90_DISPATCH(state, san90_next_v2(state, dest, capacity, info, freq, power, spectrum_capacity, spectrum_points));
    if (!san90_session.usable()) return -8;
    const auto entered = San90Clock::now();
    if (state) {
        state->timing = {};
        if (state->has_iqs_entry) {
            state->timing.entry_interval_ns = san90_elapsed_ns(state->last_iqs_entry, entered);
            state->timing.measured |= 1;
        }
        state->last_iqs_entry = entered;
        state->has_iqs_entry = true;
    }
    if (!state || !state->triggered || !dest || !info || !spectrum_points || capacity < 16240)
        return -100;
    *spectrum_points = 0;
    IQStream_TypeDef stream{};
    const auto fetch_started = San90Clock::now();
    int status = IQS_GetIQStream_PM1(&state->handle, &stream);
    state->timing.fetch_ns = san90_elapsed_ns(fetch_started);
    state->timing.fetch_status = status;
    state->timing.measured |= 2;
    if (status && status != APIRETVAL_WARNING_IFOverflow) return status;
    auto n = stream.IQS_StreamInfo.PacketSamples;
    if (n != 16240 || n > capacity || !stream.AlternIQStream ||
        !std::isfinite(stream.IQS_ScaleToV)) return -103;
    int warning = (status == APIRETVAL_WARNING_IFOverflow || stream.DeviceState.IFOverflow)
        ? APIRETVAL_WARNING_IFOverflow : 0;
    const auto copy_started = San90Clock::now();
    std::memcpy(dest, stream.AlternIQStream, n * 2 * sizeof(int16_t));
    state->timing.memcpy_ns = san90_elapsed_ns(copy_started);
    state->timing.measured |= 4;
    info->stamp = stream.IQS_TriggerInfo.SysTimerCountOfFirstDataPoint;
    info->samples = n;
    info->scale_to_v = stream.IQS_ScaleToV;
    if (freq || power) {
        if (!freq || !power || spectrum_capacity < state->fft_points) return -107;
        IQStream_TypeDef fft = stream;
        fft.IQS_Profile.CenterFreq_Hz = state->center_hz;
        fft.IQS_Profile.TriggerLength = n;
        fft.IQS_StreamInfo.PacketSamples = n;
        fft.IQS_StreamInfo.StreamSamples = n;
        fft.IQS_StreamInfo.PacketDataSize = n * 4;
        fft.IQS_StreamInfo.IQSampleRate = state->sample_rate_hz;
        fft.IQS_StreamInfo.Bandwidth = state->bandwidth_hz;
        const auto fft_started = San90Clock::now();
        status = DSP_FFT_IQSToSpectrum(&state->dsp, &fft, freq, power);
        state->timing.fft_ns = san90_elapsed_ns(fft_started);
        state->timing.fft_status = status;
        state->timing.measured |= 8;
        if (status && status != APIRETVAL_WARNING_IFOverflow) return status;
        if (status == APIRETVAL_WARNING_IFOverflow) warning = status;
        for (uint32_t i = 0; i < state->fft_points; ++i) {
            if (!std::isfinite(freq[i]) || !std::isfinite(power[i])) return -103;
        }
        *spectrum_points = state->fft_points;
    }
    state->warning = warning;
    return 0;
}

// IQ, spectrum and metadata share one SDK-thread dispatch and one packet boundary.
extern "C" int san90_next_v3(San90Device* state, int16_t* dest, uint32_t capacity,
                                San90PacketInfo* info, double* freq, float* power,
                                uint32_t spectrum_capacity, uint32_t* spectrum_points,
                                San90IqsTiming* timing, int* warning) {
    SAN90_DISPATCH(state, san90_next_v3(state, dest, capacity, info, freq, power,
                                     spectrum_capacity, spectrum_points, timing, warning));
    if (!state || !timing || !warning) return -100;
    *timing = {};
    *warning = 0;
    state->timing = {};
    const int status = san90_next_v2(state, dest, capacity, info, freq, power,
                                    spectrum_capacity, spectrum_points);
    *timing = state->timing;
    if (!status) *warning = state->warning;
    return status;
}

// Versioned, fixed-width trigger readback ABI; never append to San90RtaActual.
struct San90RtaTriggerActual {
    int valid = 0;
    int source = 0;
    int edge = 0;
    int out = 0;
    int polarity = 0;
    uint32_t retrigger = 0;
    double level = 0;
    double safetime = 0;
    double delay = 0;
    double pretime = 0;
    double acqtime = 0;
    double retriggerperiod = 0;
    double poi = 0;
    uint64_t generation = 0;
    uint64_t packet_index = 0;
    uint32_t packet_count = 0;
    int complete = -1;
    int last_sdk_status = 0;
    uint64_t first_data_timestamp = 0;
    uint32_t trigger_edges = 0;
};

struct San90RtaDevice {
    void* handle = nullptr;
    RTA_FrameInfo_TypeDef info{};
    RTA_PlotInfo_TypeDef plot{};
    RTA_TriggerInfo_TypeDef trigger{};
    MeasAuxInfo_TypeDef aux{};
    RTA_Profile_TypeDef configured{};
    San90RtaTriggerActual actual{};
    bool packet_continuity = true;
    std::vector<uint8_t> trace;
    std::vector<uint16_t> bitmap;
    std::vector<float> capture_power;
};

extern "C" int san90_rta_open_v1(San90RtaDevice** out, double center_hz, double span_hz,
                                   double* start_hz, double* stop_hz, uint32_t* points) {
    SAN90_DISPATCH(nullptr, san90_rta_open_v1(out, center_hz, span_hz, start_hz, stop_hz, points));
    if (!out || !start_hz || !stop_hz || !points || !std::isfinite(center_hz) ||
        !std::isfinite(span_hz) || span_hz <= 0.0 || span_hz > 50e6) return -100;
    *out = nullptr;
    auto* state = new (std::nothrow) San90RtaDevice;
    if (!state) return -101;
    int opened = 0;
    int status = san90_open_device(state);
    if (status && status != -49) goto fail;
    opened = 1;
    {
        RTA_Profile_TypeDef in{}, configured{};
        status = RTA_ProfileDeInit(&state->handle, &in);
        if (status) goto fail;
        uint32_t decimate = 1;
        while (50e6 / decimate > span_hz && decimate < 2048) decimate *= 2;
        in.CenterFreq_Hz = center_hz;
        in.RefLevel_dBm = 0.0;
        in.DecimateFactor = decimate;
        in.RBWMode = RBW_Auto;
        in.VBWMode = VBW_EqualToRBW;
        in.Window = FlatTop;
        in.SweepTimeMode = SWTMode_minSWT;
        in.TraceDetectMode = TraceDetectMode_Auto;
        in.TraceDetector = TraceDetector_PosPeak;
        in.TraceDetectRatio = 1;
        in.TriggerSource = Bus;
        in.TriggerMode = Adaptive;
        in.TriggerAcqTime = 0.005;
        in.Preamplifier = AutoOn;
        in.ReferenceClockSource = ReferenceClockSource_Internal;
        in.BusTimeout_ms = 250;
        status = RTA_Configuration(&state->handle, &in, &configured, &state->info);
        if (status) goto fail;
        const uint32_t width = state->info.FrameWidth;
        const uint32_t height = state->info.FrameHeight;
        const uint32_t valid = state->info.PacketValidPoints;
        if (width < 2 || valid < width || width > 65536 || height == 0 || height > 65536) {
            status = -102;
            goto fail;
        }
        state->trace.resize(std::max(valid, state->info.PacketSamplePoints) + 4096);
        state->bitmap.resize(static_cast<size_t>(width) * height + 65536);
        *start_hz = state->info.StartFrequency_Hz;
        *stop_hz = state->info.StopFrequency_Hz;
        *points = width;
    }
    status = RTA_BusTriggerStart(&state->handle);
    if (status) goto fail;
    *out = state;
    return 0;
fail:
    if (opened) san90_open_failed(status);
    // Never Device_Close a slot left by a failed Device_Open.
    if (opened && state->handle) {
        if (san90_session.usable()) {
            int stopped = RTA_BusTriggerStop(&state->handle);
            if (stopped) san90_open_failed(stopped);
        }
        san90_session.release(&state->handle);
    }
    delete state;
    return status;
}

extern "C" int san90_rta_next_v1(San90RtaDevice* state, double* freq, float* power,
                                  uint32_t capacity, uint32_t* points) {
    SAN90_DISPATCH(state, san90_rta_next_v1(state, freq, power, capacity, points));
    if (!san90_session.usable()) return -8;
    if (!state || !freq || !power || !points || capacity < state->info.FrameWidth) return -100;
    int status = RTA_GetRealTimeSpectrum(&state->handle, state->trace.data(), state->bitmap.data(),
                                         &state->plot, &state->trigger, &state->aux);
    if (status) return status;
    const uint32_t width = state->info.FrameWidth;
    const double start = state->info.StartFrequency_Hz;
    const double stop = state->info.StopFrequency_Hz;
    for (uint32_t i = 0; i < width; ++i) {
        freq[i] = start + static_cast<double>(i) * (stop - start) / width;
        power[i] = static_cast<float>(state->trace[i]) * state->plot.ScaleTodBm + state->plot.OffsetTodBm;
    }
    *points = width;
    return 0;
}

extern "C" int san90_rta_close_v1(San90RtaDevice* state) {
    SAN90_DISPATCH(state, san90_rta_close_v1(state));
    if (!state) return -100;
    int status = san90_session.usable() ? RTA_BusTriggerStop(&state->handle) : 0;
    if (status) san90_open_failed(status);
    int closed = san90_session.release(&state->handle);
    if (!status) status = closed;
    delete state;
    return status;
}

// Full RTA request (v2): every field that Python rta.py _configure_locked programs.
struct San90RtaRequest {
    double center_hz;
    uint32_t decimate;
    San90FrontEnd fe;
    int rbw_manual;
    double rbw_hz;
    int vbw_mode;  // VBWMode_TypeDef
    double vbw_hz;
    int sweep_time_mode;
    double sweep_time;
    int trigger_source;  // RTA_TriggerSource_TypeDef
    int trigger_edge;
    double trigger_level_dbm;
    double trigger_safe_time;
    double trigger_delay;
    double trigger_pre_time;
    double trigger_acq_time;
    uint32_t retrigger_count;
    double retrigger_period;
    int trigger_out;
    int trigger_out_polarity;
};

struct San90RtaActual {
    double start_hz;
    double stop_hz;
    uint32_t points;
    double ref_dbm;
    double rbw_hz;
    double vbw_hz;
    double refclk_hz;
    int refclk_src;
    int atten;
    int preamp;
    int ifgain;
};

static int san90_rta_configure(San90RtaDevice* state, const San90RtaRequest& r,
                               San90RtaActual* actual) {
    // RTA_Configuration on a streaming device can hang the firmware (Python rta.py).
    int stopped = RTA_BusTriggerStop(&state->handle);
    if (stopped) return stopped;
    RTA_Profile_TypeDef in{}, configured{};
    int status = RTA_ProfileDeInit(&state->handle, &in);
    if (status) return status;
    in.CenterFreq_Hz = r.center_hz;
    in.DecimateFactor = r.decimate;
    san90_apply_front_end(in, r.fe);
    if (r.rbw_manual && r.rbw_hz > 0) {
        in.RBWMode = RBW_Manual;
        in.RBW_Hz = r.rbw_hz;
    } else {
        in.RBWMode = RBW_Auto;
    }
    in.VBWMode = static_cast<VBWMode_TypeDef>(r.vbw_mode);
    if (r.vbw_mode == VBW_Manual && r.vbw_hz > 0) in.VBW_Hz = r.vbw_hz;
    in.SweepTimeMode = static_cast<SweepTimeMode_TypeDef>(r.sweep_time_mode);
    in.SweepTime = r.sweep_time;
    in.TriggerSource = static_cast<RTA_TriggerSource_TypeDef>(r.trigger_source);
    // Adaptive gives ~150 fps for free run; real trigger sources need FixedPoints.
    in.TriggerMode = (r.trigger_source == Bus || r.trigger_source == FreeRun) ? Adaptive
                                                                              : FixedPoints;
    in.TriggerAcqTime = r.trigger_acq_time;
    in.TriggerEdge = static_cast<TriggerEdge_TypeDef>(r.trigger_edge);
    in.TriggerLevel_dBm = r.trigger_level_dbm;
    in.TriggerLevel_SafeTime = r.trigger_safe_time;
    in.TriggerDelay = r.trigger_delay;
    in.PreTriggerTime = r.trigger_pre_time;
    in.EnableReTrigger = r.retrigger_count > 0 ? 1 : 0;
    in.ReTrigger_Count = static_cast<uint16_t>(r.retrigger_count);
    in.ReTrigger_Period = r.retrigger_period;
    in.TriggerOutMode = static_cast<TriggerOutMode_TypeDef>(r.trigger_out);
    in.TriggerOutPulsePolarity = static_cast<TriggerOutPulsePolarity_TypeDef>(r.trigger_out_polarity);
    in.BusTimeout_ms = 250;
    status = san90_retry_bus(
        [&] { return RTA_Configuration(&state->handle, &in, &configured, &state->info); });
    if (status) return status;
    const uint32_t width = state->info.FrameWidth;
    const uint32_t height = state->info.FrameHeight;
    const uint32_t valid = state->info.PacketValidPoints;
    if (width < 2 || valid < width || width > 65536 || height == 0 || height > 65536)
        return -102;
    // Fresh buffers for the new frame geometry (Python rta.py: stale buffers caused GPF).
    state->trace.assign(std::max(valid, state->info.PacketSamplePoints) + 4096, 0);
    state->bitmap.assign(static_cast<size_t>(width) * height + 65536, 0);
    if (configured.TriggerMode == FixedPoints && configured.TriggerSource == Level && !configured.EnableReTrigger)
        state->capture_power.assign(width, -1.0e30f);
    else state->capture_power.clear();
    actual->start_hz = state->info.StartFrequency_Hz;
    actual->stop_hz = state->info.StopFrequency_Hz;
    actual->points = width;
    actual->ref_dbm = configured.RefLevel_dBm;
    actual->rbw_hz = configured.RBW_Hz;
    actual->vbw_hz = configured.VBW_Hz;
    actual->refclk_hz = configured.ReferenceClockFrequency;
    actual->refclk_src = static_cast<int>(configured.ReferenceClockSource);
    actual->atten = configured.Atten;
    actual->preamp = static_cast<int>(configured.Preamplifier);
    actual->ifgain = configured.IFGainGrade;
    status = RTA_BusTriggerStart(&state->handle);
    if (status) return status;
    const uint64_t generation = state->actual.generation + 1;
    state->configured = configured;
    state->actual = {};
    auto& a = state->actual;
    a.valid = 1;
    a.source = static_cast<int>(configured.TriggerSource);
    a.edge = static_cast<int>(configured.TriggerEdge);
    a.out = static_cast<int>(configured.TriggerOutMode);
    a.polarity = static_cast<int>(configured.TriggerOutPulsePolarity);
    a.retrigger = configured.EnableReTrigger ? configured.ReTrigger_Count : 0;
    a.level = configured.TriggerLevel_dBm;
    a.safetime = configured.TriggerLevel_SafeTime;
    a.delay = configured.TriggerDelay;
    a.pretime = configured.PreTriggerTime;
    a.acqtime = configured.TriggerAcqTime;
    a.retriggerperiod = configured.ReTrigger_Period;
    a.poi = state->info.POI;
    a.generation = generation;
    a.packet_count = configured.TriggerMode == FixedPoints ? state->info.PacketCount : 0;
    state->packet_continuity = true;
    return 0;
}

extern "C" int san90_rta_open_v2(San90RtaDevice** out, const San90RtaRequest* request,
                                   San90RtaActual* actual) {
    SAN90_DISPATCH(nullptr, san90_rta_open_v2(out, request, actual));
    if (!out || !request || !actual || !std::isfinite(request->center_hz) ||
        request->decimate < 1 || request->decimate > 65536)
        return -100;
    *out = nullptr;
    auto* state = new (std::nothrow) San90RtaDevice;
    if (!state) return -101;
    int status = san90_open_device(state);
    if (status) {
        delete state;
        return status;
    }
    status = san90_rta_configure(state, *request, actual);
    if (status) {
        san90_open_failed(status);
        if (san90_session.usable()) {
            int stopped = RTA_BusTriggerStop(&state->handle);
            if (stopped) san90_open_failed(stopped);
        }
        san90_session.release(&state->handle);
        delete state;
        return status;
    }
    *out = state;
    return 0;
}

extern "C" int san90_rta_reconfig_v1(San90RtaDevice* state, const San90RtaRequest* request,
                                       San90RtaActual* actual) {
    SAN90_DISPATCH(state, san90_rta_reconfig_v1(state, request, actual));
    if (!san90_session.usable()) return -8;
    if (!state || !request || !actual || !std::isfinite(request->center_hz) ||
        request->decimate < 1 || request->decimate > 65536)
        return -100;
    return san90_rta_configure(state, *request, actual);
}

extern "C" int san90_rta_trigger_actual_v1(San90RtaDevice* state,
                                            San90RtaTriggerActual* actual) {
    SAN90_DISPATCH(state, san90_rta_trigger_actual_v1(state, actual));
    if (!state || !actual) return -100;
    *actual = state->actual;
    return 0;
}

extern "C" int san90_rta_next_v2(San90RtaDevice* state, double* freq, float* power,
                                  uint32_t capacity, uint32_t* points) {
    SAN90_DISPATCH(state, san90_rta_next_v2(state, freq, power, capacity, points));
    if (!san90_session.usable()) return -8;
    if (!state || !freq || !power || !points || capacity < state->info.FrameWidth) return -100;
    if (!state->actual.valid) return -102;
    const int status = RTA_GetRealTimeSpectrum(&state->handle, state->trace.data(),
        state->bitmap.data(), &state->plot, &state->trigger, &state->aux);
    state->actual.last_sdk_status = status;
    // These vendor warnings are carried by a data packet, unlike bus timeout/download.
    const bool has_data = status == 0 || status == -12 || (status <= -14 && status >= -19) ||
        (status <= -36 && status >= -39) || status == -301 || status == -302 || status == -303;
    if (!has_data) {
        if (status != -10 && status != -304) state->packet_continuity = false;
        state->actual.complete = -1;
        if (!state->packet_continuity) state->actual.packet_index = 0;
        return status;
    }
    const uint32_t width = state->info.FrameWidth;
    for (uint32_t i = 0; i < width; ++i) {
        freq[i] = state->info.StartFrequency_Hz + static_cast<double>(i) *
            (state->info.StopFrequency_Hz - state->info.StartFrequency_Hz) / width;
        // A packet contains PacketFrame traces. Preserve activity across the whole
        // acquired packet rather than silently displaying only its first trace.
        float peak = static_cast<float>(state->trace[i]) * state->plot.ScaleTodBm + state->plot.OffsetTodBm;
        const uint32_t traces = state->configured.TriggerMode == FixedPoints
            ? std::min(state->info.PacketFrame, state->info.PacketValidPoints / width) : 1;
        for (uint32_t trace = 1; trace < traces; ++trace) {
            const float sample = static_cast<float>(state->trace[static_cast<size_t>(trace) * width + i]) *
                state->plot.ScaleTodBm + state->plot.OffsetTodBm;
            peak = std::max(peak, sample);
        }
        power[i] = peak;
        if (state->configured.TriggerMode == FixedPoints && !state->configured.EnableReTrigger &&
            state->configured.TriggerSource == Level && state->packet_continuity &&
            state->actual.packet_index < state->actual.packet_count) {
            state->capture_power[i] = std::max(state->capture_power[i], power[i]);
            power[i] = state->capture_power[i];
        }
    }
    *points = width;
    auto& a = state->actual;
    a.first_data_timestamp = state->trigger.SysTimerCountOfFirstDataPoint;
    a.trigger_edges = state->trigger.InPacketTriggerEdges;
    if (state->packet_continuity) ++a.packet_index;
    // The SDK exposes no capture identity. Only one uninterrupted, non-retriggered
    // level acquisition has a provable end at the configured total packet count.
    const bool finite_level = state->configured.TriggerSource == Level &&
        state->configured.TriggerMode == FixedPoints && !state->configured.EnableReTrigger;
    a.complete = state->packet_continuity && finite_level && (status == -301 || status == -303) ? 1 :
        state->packet_continuity && finite_level && a.packet_count > 0 && a.packet_index <= a.packet_count
        ? static_cast<int>(a.packet_index == a.packet_count) : -1;
    return 0;
}

extern "C" int san90_rta_reconfig_v2(San90RtaDevice* state, const San90RtaRequest* request,
                                       San90RtaActual* actual) {
    SAN90_DISPATCH(state, san90_rta_reconfig_v2(state, request, actual));
    if (!san90_session.usable()) return -8;
    if (!state || !request || !actual || !std::isfinite(request->center_hz) ||
        request->decimate < 1 || request->decimate > 65536) return -100;
    const auto previous = state->configured;
    const auto previous_actual = state->actual;
    const int status = san90_rta_configure(state, *request, actual);
    if (!status || status == -8 || status == -9) return status;
    // Keep the same physical session and restore its last applied profile on failure.
    int restored = RTA_BusTriggerStop(&state->handle);
    RTA_Profile_TypeDef configured{};
    if (!restored) restored = RTA_Configuration(&state->handle, &previous, &configured, &state->info);
    if (!restored) {
        state->trace.assign(std::max(state->info.PacketValidPoints, state->info.PacketSamplePoints) + 4096, 0);
        state->bitmap.assign(static_cast<size_t>(state->info.FrameWidth) * state->info.FrameHeight + 65536, 0);
        if (previous.TriggerMode == FixedPoints && previous.TriggerSource == Level && !previous.EnableReTrigger)
            state->capture_power.assign(state->info.FrameWidth, -1.0e30f);
        else state->capture_power.clear();
        restored = RTA_BusTriggerStart(&state->handle);
    }
    state->configured = previous;
    state->actual = previous_actual;
    if (!restored) ++state->actual.generation;
    state->actual.packet_index = 0;
    state->actual.complete = -1;
    state->actual.last_sdk_status = status;
    state->packet_continuity = !restored;
    if (restored) state->actual.valid = 0;
    return restored == -8 || restored == -9 ? restored : status;
}
struct San90SwpDevice {
    void* handle = nullptr;
    SWP_Profile_TypeDef req{};
    SWP_Profile_TypeDef out{};
    SWP_TraceInfo_TypeDef trace{};
    MeasAuxInfo_TypeDef aux{};
    // SWP_GetFullSweep takes no length argument and writes the device's own trace, which can
    // exceed the count recorded at configuration time; size for the largest possible trace
    // like the Python service (_SWP_TRACE_MAX) and hand out only the configured count.
    std::vector<double> freq;
    std::vector<float> power;
    uint32_t points = 0;
    // Interceptor buffers for DSP_InterceptSpectrum (same capacity as the trace buffers).
    std::vector<double> ifreq;
    std::vector<float> ipower;
};

// Full SWP request (v2). Enum fields hold htra_api.h values. trace_detect_mode,
// trace_detector and frame_detector = -1 keep the DeInit default.
struct San90SwpRequest {
    double center_hz;
    double span_hz;
    San90FrontEnd fe;
    int rbw_mode;
    double rbw_hz;
    int vbw_mode;
    double vbw_hz;
    int window;
    int sweep_time_mode;
    double sweep_time;
    int trace_points;
    int points_strategy;
    int trace_align;
    int spur;
    int trace_detect_mode;
    int trace_detector;
    int frame_detector;
};

struct San90SwpActual {
    double start_hz;
    double stop_hz;
    uint32_t points;
    double sweep_ms;
    double ref_dbm;
    double rbw_hz;
    double vbw_hz;
    double refclk_hz;
    int refclk_src;
};

extern "C" int san90_swp_front_end_actual_v1(San90SwpDevice* state, San90FrontEndActual* actual) {
    SAN90_DISPATCH(state, san90_swp_front_end_actual_v1(state, actual));
    if (!san90_session.usable()) return -8;
    if (!state || !actual || san90_session.active != state) return -100;
    *actual = san90_front_end_actual(state->out);
    return 0;
}

static int san90_swp_configure(San90SwpDevice* state, const San90SwpRequest& r,
                               San90SwpActual* actual) {
    SWP_Profile_TypeDef req{};
    int status = SWP_ProfileDeInit(&state->handle, &req);
    if (status) return status;
    req.FreqAssignment = StartStop;
    req.StartFreq_Hz = r.center_hz - r.span_hz / 2.0;
    req.StopFreq_Hz = r.center_hz + r.span_hz / 2.0;
    san90_apply_front_end(req, r.fe);
    req.RBWMode = static_cast<RBWMode_TypeDef>(r.rbw_mode);
    req.RBW_Hz = r.rbw_hz;
    req.VBWMode = static_cast<VBWMode_TypeDef>(r.vbw_mode);
    req.VBW_Hz = r.vbw_hz;
    req.Window = static_cast<Window_TypeDef>(r.window);
    req.SweepTimeMode = static_cast<SweepTimeMode_TypeDef>(r.sweep_time_mode);
    req.SweepTime = r.sweep_time;
    req.TracePoints = static_cast<uint32_t>(r.trace_points < 51 ? 51 : r.trace_points);
    req.TracePointsStrategy = static_cast<TracePointsStrategy_TypeDef>(r.points_strategy);
    req.TraceAlign = static_cast<TraceAlign_TypeDef>(r.trace_align);
    req.SpurRejection = static_cast<SpurRejection_TypeDef>(r.spur);
    if (r.trace_detect_mode >= 0)
        req.TraceDetectMode = static_cast<TraceDetectMode_TypeDef>(r.trace_detect_mode);
    if (r.trace_detector >= 0)
        req.TraceDetector = static_cast<TraceDetector_TypeDef>(r.trace_detector);
    if (r.frame_detector >= 0) req.Detector = static_cast<Detector_TypeDef>(r.frame_detector);
    SWP_Profile_TypeDef out{};
    SWP_TraceInfo_TypeDef trace{};
    status = san90_retry_bus(
        [&] { return SWP_Configuration(&state->handle, &req, &out, &trace); });
    if (status) return status;
    if (trace.FullsweepTracePoints < 2 || trace.FullsweepTracePoints > 65536 ||
        !std::isfinite(out.StartFreq_Hz) || !std::isfinite(out.StopFreq_Hz) ||
        out.StartFreq_Hz >= out.StopFreq_Hz)
        return -102;
    state->req = req;
    state->out = out;
    state->trace = trace;
    state->points = static_cast<uint32_t>(trace.FullsweepTracePoints);
    actual->start_hz = out.StartFreq_Hz;
    actual->stop_hz = out.StopFreq_Hz;
    actual->points = state->points;
    actual->sweep_ms =
        (std::isfinite(trace.EstimateMinSweepTime) ? trace.EstimateMinSweepTime : 0.0) * 1000.0;
    actual->ref_dbm = out.RefLevel_dBm;
    actual->rbw_hz = out.RBW_Hz;
    actual->vbw_hz = out.VBW_Hz;
    actual->refclk_hz = out.ReferenceClockFrequency;
    actual->refclk_src = static_cast<int>(out.ReferenceClockSource);
    return 0;
}

static bool san90_swp_request_ok(const San90SwpRequest* r) {
    return r && std::isfinite(r->center_hz) && std::isfinite(r->span_hz) && r->span_hz > 0.0 &&
           r->center_hz - r->span_hz / 2.0 >= 0.0 && std::isfinite(r->fe.ref_dbm) &&
           std::isfinite(r->rbw_hz) && std::isfinite(r->vbw_hz) && std::isfinite(r->sweep_time);
}

extern "C" int san90_swp_open_v2(San90SwpDevice** out, const San90SwpRequest* request,
                                   San90SwpActual* actual) {
    SAN90_DISPATCH(nullptr, san90_swp_open_v2(out, request, actual));
    if (!out || !actual || !san90_swp_request_ok(request)) return -100;
    *out = nullptr;
    auto* state = new (std::nothrow) San90SwpDevice;
    if (!state) return -101;
    int status = san90_open_device(state);
    if (status) {
        delete state;
        return status;
    }
    state->freq.assign(65536, 0.0);
    state->power.assign(65536, 0.0f);
    state->ifreq.assign(65536, 0.0);
    state->ipower.assign(65536, 0.0f);
    status = san90_swp_configure(state, *request, actual);
    if (status) {
        san90_open_failed(status);
        san90_session.release(&state->handle);
        delete state;
        return status;
    }
    *out = state;
    return 0;
}

extern "C" int san90_swp_reconfig_v1(San90SwpDevice* state, const San90SwpRequest* request,
                                       San90SwpActual* actual) {
    SAN90_DISPATCH(state, san90_swp_reconfig_v1(state, request, actual));
    if (!san90_session.usable()) return -8;
    if (!state || !actual || !san90_swp_request_ok(request)) return -100;
    const int status = san90_swp_configure(state, *request, actual);
    if (!status || status == -8 || status == -9) return status;
    // Configuration may change device state before rejecting a parameter. Restore the
    // last effective profile on this same SDK owner; cached geometry was not committed.
    auto previous = state->out;
    SWP_Profile_TypeDef restored{};
    SWP_TraceInfo_TypeDef trace{};
    const int rollback = SWP_Configuration(&state->handle, &previous, &restored, &trace);
    if (rollback == -8 || rollback == -9) return rollback;
    if (rollback) san90_session.invalidate = true;
    return status;
}

// IF overflow is a warning that can accompany a complete sweep. The SDK has no
// written-count output: poison the configured trace so a partial/absent sweep
// cannot reuse bins from a previous call. Raw bins must be finite; the v2 output
// alone converts the SDK's zero-power coverage gaps to NaN.
static int san90_swp_fetch(San90SwpDevice* state, bool& available) {
    available = false;
    if (state->points < 2 || state->points > state->freq.size() ||
        state->points > state->power.size()) return -108;
    for (uint32_t i = 0; i < state->points; ++i) {
        state->freq[i] = NAN;
        state->power[i] = NAN;
    }
    state->aux = {};
    const int status = SWP_GetFullSweep(&state->handle, state->freq.data(),
                                       state->power.data(), &state->aux);
    if (status && status != APIRETVAL_WARNING_IFOverflow) return status;
    for (uint32_t i = 0; i < state->points; ++i) {
        if (!std::isfinite(state->freq[i]) || !std::isfinite(state->power[i]))
            return status ? status : -103;
    }
    available = true;
    return status;
}

// One sweep cut to the requested start/stop (Python device.py fetch_sweep): the vendor trace
// can extend past the request and below 0 Hz; unfilled bins read exactly 0.0 dBm -> NaN.
extern "C" int san90_swp_next_v2(San90SwpDevice* state, double* freq, float* power,
                                   uint32_t capacity, uint32_t* points,
                                   double* refclk_ppm) {
    SAN90_DISPATCH(state, san90_swp_next_v2(state, freq, power, capacity, points, refclk_ppm));
    if (!san90_session.usable()) return -8;
    if (!state || !freq || !power || !points || !refclk_ppm) return -100;
    *points = 0;
    bool available = false;
    const int status = san90_swp_fetch(state, available);
    if (!available) return status;
    uint32_t cut = 0;
    DSP_InterceptSpectrum(state->req.StartFreq_Hz, state->req.StopFreq_Hz, state->freq.data(),
                          state->power.data(), state->points, state->ifreq.data(),
                          state->ipower.data(), &cut);
    if (cut > state->points || cut > state->ifreq.size() ||
        cut > state->ipower.size() || cut > capacity) return -108;
    uint32_t n = 0;
    for (uint32_t i = 0; i < cut; ++i) {
        if (!std::isfinite(state->ifreq[i]) || !std::isfinite(state->ipower[i])) return -103;
        if (state->ifreq[i] < 0.0) continue;
        freq[n] = state->ifreq[i];
        power[n] = state->ipower[i] == 0.0f ? NAN : state->ipower[i];
        ++n;
    }
    if (n < 2) return -109;
    *points = n;
    *refclk_ppm = state->aux.RefClkFreqOffset;
    return status;
}

// Stable optional metadata ABI, copied from the successful physical Device_Open.
// Mode transitions retain the same boot result; recovery replaces it on reopen.
struct San90DeviceInfo {
    uint64_t uid;
    uint32_t model, hw, mfw, ffw, bus_speed, bus_ver, api_ver, pmu, agu;
    int32_t warnings, errors;
};
extern "C" int san90_device_info_v1(void* session, San90DeviceInfo* info) {
    SAN90_DISPATCH(session, san90_device_info_v1(session, info));
    if (!session || !info) return -100;
    if (!san90_session.usable()) return -8;
    const auto& boot = san90_session.boot_info;
    const auto& device = boot.DeviceInfo;
    *info = {device.DeviceUID, static_cast<uint32_t>(device.Model),
             device.HardwareVersion, device.MFWVersion, device.FFWVersion,
             boot.BusSpeed, boot.BusVersion, boot.APIVersion,
             device.PMUVersion, device.AGUVersion, boot.Warnings, boot.Errors};
    return 0;
}

// Device power-on SWP defaults (Python device.py load_preset_defaults). Enum fields are
// returned as raw header values; the caller maps them to wire names.
extern "C" int san90_swp_defaults_v1(void* session, double* center_hz, double* span_hz,
                                       double* ref_dbm, double* rbw_hz, double* vbw_hz,
                                       int* points, int* atten, int* window, int* rbw_mode,
                                       int* vbw_mode, int* spur, int* frame_detector,
                                       int* trace_align, int* points_strategy, int* preamp,
                                       int* ifgain, int* gain_strategy, int* sweep_time_mode,
                                       double* sweep_time) {
    SAN90_DISPATCH(session, san90_swp_defaults_v1(session, center_hz, span_hz, ref_dbm, rbw_hz, vbw_hz, points, atten, window, rbw_mode, vbw_mode, spur, frame_detector, trace_align, points_strategy, preamp, ifgain, gain_strategy, sweep_time_mode, sweep_time));
    if (!san90_session.usable()) return -8;
    if (!session || !center_hz || !span_hz || !ref_dbm || !rbw_hz || !vbw_hz || !points ||
        !atten || !window || !rbw_mode || !vbw_mode || !spur || !frame_detector ||
        !trace_align || !points_strategy || !preamp || !ifgain || !gain_strategy ||
        !sweep_time_mode || !sweep_time)
        return -100;
    // Every San90*Device struct starts with the vendor handle (see san90_gnss_v1).
    void* handle = *static_cast<void**>(session);
    if (!handle) return -101;
    SWP_Profile_TypeDef p{};
    int status = SWP_ProfileDeInit(&handle, &p);
    if (status) return status;
    *center_hz = p.CenterFreq_Hz;
    *span_hz = p.Span_Hz;
    *ref_dbm = p.RefLevel_dBm;
    *rbw_hz = p.RBW_Hz;
    *vbw_hz = p.VBW_Hz;
    *points = static_cast<int>(p.TracePoints);
    *atten = p.Atten;
    *window = static_cast<int>(p.Window);
    *rbw_mode = static_cast<int>(p.RBWMode);
    *vbw_mode = static_cast<int>(p.VBWMode);
    *spur = static_cast<int>(p.SpurRejection);
    *frame_detector = static_cast<int>(p.Detector);
    *trace_align = static_cast<int>(p.TraceAlign);
    *points_strategy = static_cast<int>(p.TracePointsStrategy);
    *preamp = static_cast<int>(p.Preamplifier);
    *ifgain = p.IFGainGrade;
    *gain_strategy = static_cast<int>(p.GainStrategy);
    *sweep_time_mode = static_cast<int>(p.SweepTimeMode);
    *sweep_time = p.SweepTime;
    return 0;
}

extern "C" int san90_swp_open_v1(San90SwpDevice** out,
                                   double center_hz, double span_hz, double ref_dbm,
                                   double rbw_hz, int rbw_mode, double vbw_hz, int vbw_mode,
                                   int window, int atten, int preamp, int ifgain,
                                   int gain_strategy, int sweep_time_mode, double sweep_time,
                                   int trace_points, int points_strategy, int trace_align,
                                   int spur, int trace_detect_mode, int trace_detector,
                                   int frame_detector,
                                   double* start_hz, double* stop_hz, int* points,
                                   double* sweep_ms, double* rbw_actual, double* vbw_actual,
                                   double* refclk_hz, int* refclk_src) {
    SAN90_DISPATCH(nullptr, san90_swp_open_v1(out, center_hz, span_hz, ref_dbm, rbw_hz, rbw_mode, vbw_hz, vbw_mode, window, atten, preamp, ifgain, gain_strategy, sweep_time_mode, sweep_time, trace_points, points_strategy, trace_align, spur, trace_detect_mode, trace_detector, frame_detector, start_hz, stop_hz, points, sweep_ms, rbw_actual, vbw_actual, refclk_hz, refclk_src));
    if (!out || !start_hz || !stop_hz || !points || !sweep_ms || !rbw_actual ||
        !vbw_actual || !refclk_hz || !refclk_src || !std::isfinite(center_hz) ||
        !std::isfinite(span_hz) || span_hz <= 0.0 || center_hz - span_hz / 2.0 < 0.0 ||
        !std::isfinite(ref_dbm) || !std::isfinite(rbw_hz) || !std::isfinite(vbw_hz) ||
        !std::isfinite(sweep_time))
        return -100;
    *out = nullptr;
    auto* state = new (std::nothrow) San90SwpDevice;
    if (!state) return -101;
    int opened = 0;
    int status = san90_open_device(state);
    if (status && status != -49) goto fail;
    opened = 1;
    status = SWP_ProfileDeInit(&state->handle, &state->req);
    if (status) goto fail;
    state->req.FreqAssignment = StartStop;
    state->req.StartFreq_Hz = center_hz - span_hz / 2.0;
    state->req.StopFreq_Hz = center_hz + span_hz / 2.0;
    state->req.RefLevel_dBm = ref_dbm;
    state->req.RBWMode = static_cast<RBWMode_TypeDef>(rbw_mode);
    state->req.RBW_Hz = rbw_hz;
    state->req.VBWMode = static_cast<VBWMode_TypeDef>(vbw_mode);
    state->req.VBW_Hz = vbw_hz;
    state->req.Window = static_cast<Window_TypeDef>(window);
    state->req.Atten = static_cast<int8_t>(atten);
    state->req.Preamplifier = static_cast<PreamplifierState_TypeDef>(preamp);
    state->req.IFGainGrade = static_cast<uint8_t>(ifgain);
    state->req.GainStrategy = static_cast<GainStrategy_TypeDef>(gain_strategy);
    state->req.SweepTimeMode = static_cast<SweepTimeMode_TypeDef>(sweep_time_mode);
    state->req.SweepTime = sweep_time;
    state->req.TracePoints = static_cast<uint32_t>(trace_points < 51 ? 51 : trace_points);
    state->req.TracePointsStrategy =
        static_cast<TracePointsStrategy_TypeDef>(points_strategy);
    state->req.TraceAlign = static_cast<TraceAlign_TypeDef>(trace_align);
    state->req.SpurRejection = static_cast<SpurRejection_TypeDef>(spur);
    if (trace_detect_mode >= 0) {
        state->req.TraceDetectMode = static_cast<TraceDetectMode_TypeDef>(trace_detect_mode);
    }
    if (trace_detector >= 0) {
        state->req.TraceDetector = static_cast<TraceDetector_TypeDef>(trace_detector);
    }
    if (frame_detector >= 0) {
        state->req.Detector = static_cast<Detector_TypeDef>(frame_detector);
    }
    status = SWP_Configuration(&state->handle, &state->req, &state->out, &state->trace);
    if (status) goto fail;
    if (state->trace.FullsweepTracePoints < 2 ||
        state->trace.FullsweepTracePoints > 65536 ||
        !std::isfinite(state->out.StartFreq_Hz) || !std::isfinite(state->out.StopFreq_Hz) ||
        state->out.StartFreq_Hz >= state->out.StopFreq_Hz) {
        status = -102;
        goto fail;
    }
    state->points = static_cast<uint32_t>(state->trace.FullsweepTracePoints);
    state->freq.assign(65536, 0.0);
    state->power.assign(65536, 0.0f);
    *start_hz = state->out.StartFreq_Hz;
    *stop_hz = state->out.StopFreq_Hz;
    *points = static_cast<int>(state->points);
    *sweep_ms = (std::isfinite(state->trace.EstimateMinSweepTime)
                     ? state->trace.EstimateMinSweepTime
                     : 0.0) * 1000.0;
    *rbw_actual = state->out.RBW_Hz;
    *vbw_actual = state->out.VBW_Hz;
    *refclk_hz = state->out.ReferenceClockFrequency;
    *refclk_src = static_cast<int>(state->out.ReferenceClockSource);
    *out = state;
    return 0;
fail:
    if (opened) san90_open_failed(status);
    // Never Device_Close a slot left by a failed Device_Open.
    if (opened && state->handle) san90_session.release(&state->handle);
    delete state;
    return status;
}

extern "C" int san90_swp_next_v1(San90SwpDevice* state, double* freq, float* power,
                                   uint32_t capacity, uint32_t* points) {
    SAN90_DISPATCH(state, san90_swp_next_v1(state, freq, power, capacity, points));
    if (!san90_session.usable()) return -8;
    if (!state || !freq || !power || !points || capacity < state->points) return -100;
    *points = 0;
    bool available = false;
    const int status = san90_swp_fetch(state, available);
    if (!available) return status;
    std::memcpy(freq, state->freq.data(), state->points * sizeof(double));
    std::memcpy(power, state->power.data(), state->points * sizeof(float));
    *points = state->points;
    return status;
}

extern "C" int san90_swp_close_v1(San90SwpDevice* state) {
    SAN90_DISPATCH(state, san90_swp_close_v1(state));
    if (!state) return -100;
    int status = san90_session.release(&state->handle);
    delete state;
    return status;
}

struct San90PnmDevice {
    void* handle = nullptr;
    PNM_MeasInfo_TypeDef info{};
    std::vector<double> offset;
    std::vector<float> pn;
    std::vector<uint32_t> updates;
    MeasAuxInfo_TypeDef aux{};
    double carrier_hz = 0;
    float carrier_dbm = 0;
    float ref_level = 0;
    bool started = false;
    uint32_t done_count = 0;
    uint32_t stall = 0;
};

extern "C" int san90_pnm_open_v1(San90PnmDevice** out, double center_hz, float threshold,
                                   double rbw_ratio, double start_offset, double stop_offset,
                                   uint32_t trace_average, uint32_t* trace_points,
                                   uint32_t* segments, uint32_t* partial_updates) {
    SAN90_DISPATCH(nullptr, san90_pnm_open_v1(out, center_hz, threshold, rbw_ratio, start_offset, stop_offset, trace_average, trace_points, segments, partial_updates));
    if (!out || !trace_points || !segments || !partial_updates || !std::isfinite(center_hz) ||
        !std::isfinite(rbw_ratio) || rbw_ratio <= 0.0 || !std::isfinite(start_offset) ||
        !std::isfinite(stop_offset) || start_offset < 1.0 || stop_offset > 1e7 ||
        start_offset >= stop_offset || trace_average < 1 || trace_average > 1000 ||
        center_hz < 9000.0 || center_hz > 9000000000.0)
        return -100;
    *out = nullptr;
    auto* state = new (std::nothrow) San90PnmDevice;
    if (!state) return -101;
    int opened = 0;
    int status = san90_open_device(state);
    if (status && status != -49) goto fail;
    opened = 1;
    {
        PNM_Profile_TypeDef in{}, configured{};
        status = PNM_ProfileDeInit(&state->handle, &in);
        if (status) goto fail;
        in.CenterFreq = center_hz;
        in.Threshold = threshold;
        in.RBWRatio = rbw_ratio;
        in.StartOffsetFreq = start_offset;
        in.StopOffsetFreq = stop_offset;
        in.TraceAverage = trace_average;
        status = PNM_Configuration(&state->handle, &in, &configured, &state->info);
        if (status) goto fail;
        if (state->info.TracePoints < 2 || state->info.TracePoints > 65536 ||
            state->info.Segments < 1 || state->info.Segments > 7 ||
            state->info.PartialUpdateCounts < 1) {
            status = -102;
            goto fail;
        }
        state->offset.assign(state->info.TracePoints, 0.0);
        state->pn.assign(state->info.TracePoints, 0.0f);
        state->updates.assign(state->info.Segments, 0);
    }
    *trace_points = state->info.TracePoints;
    *segments = state->info.Segments;
    *partial_updates = state->info.PartialUpdateCounts;
    *out = state;
    return 0;
fail:
    if (opened) san90_open_failed(status);
    // Never Device_Close a slot left by a failed Device_Open.
    if (opened && state->handle) san90_session.release(&state->handle);
    delete state;
    return status;
}

// One partial-update attempt per call, mirroring the Python session step:
//   0 = new data (fields filled, progress/done set), 1 = no update yet (call again),
//   anything else = hard error. The cycle machine (start/done/stall-restart) lives here so
//   every vendor call stays on the owning thread.
extern "C" int san90_pnm_next_v1(San90PnmDevice* state, double* carrier_hz,
                                   float* carrier_power, double* offset, float* pn,
                                   uint32_t capacity, uint32_t* points, float* ref_level,
                                   uint32_t* progress_pct, uint32_t* done) {
    SAN90_DISPATCH(state, san90_pnm_next_v1(state, carrier_hz, carrier_power, offset, pn, capacity, points, ref_level, progress_pct, done));
    if (!san90_session.usable()) return -8;
    if (!state || !carrier_hz || !carrier_power || !offset || !pn || !points ||
        !ref_level || !progress_pct || !done || capacity < state->info.TracePoints)
        return -100;
    *points = 0;
    *done = 0;
    if (!state->started) {
        int started = PNM_StartMeasure(&state->handle);
        if (started) {
            // Bus errors keep their raw status so the link/watchdog sees them;
            // other start failures stay distinguishable from Get failures.
            if (started == -8 || started == -9) return started;
            return 2000000 + started;
        }
        state->started = true;
        state->done_count = 0;
        state->stall = 0;
    }
    int status = PNM_GetPartialUpdatedFullTrace(&state->handle, &state->carrier_hz,
                                                &state->carrier_dbm, state->offset.data(),
                                                state->pn.data(), state->updates.data(),
                                                &state->aux, &state->ref_level);
    if (status == -8 || status == -9) return status;  // bus errors: the link is lost
    if (status) {
        state->stall += 1;
        if (state->stall > state->info.PartialUpdateCounts + 8) {
            state->started = false;  // restart the cycle like the Python stall watchdog
        }
        // Python-parity: a nonzero Get (e.g. -203 CarrierLoss on a weak bench source) is
        // tolerated silently with the stall-restart above; the cycle machine keeps polling.
        return 1;
    }
    state->done_count += 1;
    state->stall = 0;
    std::memcpy(carrier_hz, &state->carrier_hz, sizeof(double));
    *carrier_power = state->carrier_dbm;
    std::memcpy(offset, state->offset.data(), state->info.TracePoints * sizeof(double));
    std::memcpy(pn, state->pn.data(), state->info.TracePoints * sizeof(float));
    *ref_level = state->ref_level;
    *points = state->info.TracePoints;
    *progress_pct = static_cast<uint32_t>(state->done_count * 100 /
                                          state->info.PartialUpdateCounts);
    if (state->done_count >= state->info.PartialUpdateCounts) {
        *done = 1;
        state->started = false;  // next call starts a fresh cycle (no StopMeasure between)
    }
    return 0;
}

extern "C" int san90_pnm_close_v1(San90PnmDevice* state) {
    SAN90_DISPATCH(state, san90_pnm_close_v1(state));
    if (!state) return -100;
    // The cycle flag can be cleared by completion or a stall while the device
    // still owns a PNM measurement. Every configured PNM mode must be stopped.
    int status = san90_session.usable() ? PNM_StopMeasure(&state->handle) : 0;
    if (status) san90_open_failed(status);
    int closed = san90_session.release(&state->handle);
    if (!status) status = closed;
    delete state;
    return status;
}

// GNSS snapshot on any session state: every San90*Device struct's FIRST member is the
// vendor device handle (do not reorder), so one entry point serves SWP/RTA/PNM/IQS.
extern "C" int san90_gnss_v1(void* state, int* lock, int* sats, int* docxo,
                               int* docxo_mode, int* antenna, float* latitude,
                               float* longitude, int* altitude, int* year, int* month,
                               int* day, int* hour, int* minute, int* second) {
    SAN90_DISPATCH(state, san90_gnss_v1(state, lock, sats, docxo, docxo_mode, antenna, latitude, longitude, altitude, year, month, day, hour, minute, second));
    if (!san90_session.usable()) return -8;
    if (!state || !lock || !sats || !docxo || !docxo_mode || !antenna || !latitude
        || !longitude || !altitude || !year || !month || !day || !hour || !minute
        || !second)
        return -100;
    void* handle = *static_cast<void**>(state);
    if (!handle) return -101;
    GNSSInfo_TypeDef g{};
    // Packet reception refreshes this snapshot without occupying the data channel.
    int status = Device_GetGNSSInfo(&handle, &g);
    if (status) return status;
    *lock = g.GNSS_LockState;
    *sats = g.SatsNum;
    *docxo = g.DOCXO_LockState;
    *docxo_mode = static_cast<int>(g.DOCXO_WorkMode);
    *antenna = static_cast<int>(g.GNSSAntennaState);
    *latitude = g.latitude;
    *longitude = g.longitude;
    *altitude = g.altitude;
    *year = g.Year; *month = g.month; *day = g.day;
    *hour = g.hour; *minute = g.minute; *second = g.second;
    return 0;
}

// Explicit streaming-safe ABI: the vendor snapshot is refreshed by packet reception.
extern "C" int san90_gnss_cached_v1(void* state, int* lock, int* sats, int* docxo,
                               int* docxo_mode, int* antenna, float* latitude,
                               float* longitude, int* altitude, int* year, int* month,
                               int* day, int* hour, int* minute, int* second) {
    return san90_gnss_v1(state, lock, sats, docxo, docxo_mode, antenna, latitude,
                         longitude, altitude, year, month, day, hour, minute, second);
}

extern "C" int san90_close(San90Device* state) {
    SAN90_DISPATCH(state, san90_close(state));
    if (!state) return -100;
    int status = san90_session.usable() && state->triggered ? IQS_BusTriggerStop(&state->handle) : 0;
    if (status) san90_open_failed(status);
    if (state->dsp) {
        int closed = DSP_Close(&state->dsp);
        if (!status) status = closed;
    }
    if (state->handle) {
        int closed = san90_session.release(&state->handle);
        if (!status) status = closed;
    }
    delete state;
    return status;
}
