//! Executes production SWP entries against SDK stubs for rollback and overflow behavior.
use std::process::Command;

#[test]
fn rejected_swp_profile_restores_effective_profile_on_existing_handle() {
    let shim = include_str!("../sdk/san90_sdk.cpp");
    let start = shim.find("extern \"C\" int san90_swp_reconfig_v1(").unwrap();
    let end = start + shim[start..].find("// IF overflow is a warning").unwrap();
    let harness = r#"
#include <cassert>
#include <cstdint>
#define SAN90_DISPATCH(state, expression)
struct Profile { double start, stop; unsigned points; };
using SWP_Profile_TypeDef = Profile;
struct SWP_TraceInfo_TypeDef {};
struct San90SwpRequest { int failure; };
struct San90SwpActual { Profile geometry; };
struct San90SwpDevice { void* handle; Profile req, out; };
static int physical_handle;
static Profile physical;
static int configurations = 0;
static int rollback_status = 0;
static int opens = 0;
static void* Device_Open() { ++opens; return &physical_handle; }
struct Session { bool invalidate = false; bool usable() const { return true; } } san90_session;
static bool san90_swp_request_ok(const San90SwpRequest* request) { return request; }
static int SWP_Configuration(void** handle, Profile* request, Profile* out, SWP_TraceInfo_TypeDef*) {
    assert(*handle == &physical_handle);
    ++configurations;
    if (rollback_status) return rollback_status;
    physical = *request;
    *out = physical;
    return 0;
}
static unsigned SWP_GetFullSweep(void** handle, double* frequency, float* power) {
    assert(*handle == &physical_handle);
    for (unsigned i = 0; i < physical.points; ++i) {
        frequency[i] = physical.start + i * (physical.stop - physical.start) / (physical.points - 1);
        power[i] = static_cast<float>(physical.start);
    }
    return physical.points;
}
static int san90_swp_configure(San90SwpDevice* state, const San90SwpRequest& request,
                               San90SwpActual* actual) {
    assert(state->handle == &physical_handle);
    // Model an SDK that has already changed frequency/trace size when it rejects.
    physical = {800, 900, 51};
    if (request.failure) return request.failure;
    state->out = physical;
    actual->geometry = physical;
    return 0;
}
"#;
    let assertions = r#"
int main() {
    // Requested and effective profiles deliberately differ: rollback must use effective.
    San90SwpDevice state{Device_Open(), {90, 210, 201}, {100, 200, 101}};
    physical = state.out;
    San90SwpActual actual{{100, 200, 101}};
    San90SwpRequest request{-7};
    assert(san90_swp_reconfig_v1(&state, &request, &actual) == -7);
    assert(configurations == 1 && opens == 1);
    assert(state.handle == &physical_handle && !san90_session.invalidate);
    assert(state.out.start == 100 && state.out.stop == 200 && state.out.points == 101);
    assert(actual.geometry.start == 100 && actual.geometry.stop == 200 && actual.geometry.points == 101);
    // A subsequent sweep uses the actual device profile, not the cached geometry.
    double frequency[201]{};
    float power[201]{};
    const unsigned count = SWP_GetFullSweep(&state.handle, frequency, power);
    assert(count == 101 && frequency[0] == 100 && frequency[count - 1] == 200);
    assert(power[0] == 100 && power[count - 1] == 100);
    for (int status : {-8, -9}) {
        request.failure = status;
        const int before = configurations;
        assert(san90_swp_reconfig_v1(&state, &request, &actual) == status);
        assert(configurations == before); // Do not touch an unplugged SDK pointer.
    }
    request.failure = -7;
    rollback_status = -9;
    assert(san90_swp_reconfig_v1(&state, &request, &actual) == -9);
    rollback_status = -5;
    assert(san90_swp_reconfig_v1(&state, &request, &actual) == -7);
    assert(san90_session.invalidate);
}
"#;
    let directory = std::env::temp_dir().join(format!("websa_swp_transaction_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("rollback.cpp");
    let binary = directory.join("rollback");
    std::fs::write(&source, format!("#include <initializer_list>\n{harness}\n{}\n{assertions}", &shim[start..end])).unwrap();
    assert!(Command::new("g++").args(["-std=c++17", "-O1"]).arg(&source).arg("-o").arg(&binary)
        .status().expect("g++ required for SWP transaction regression").success());
    assert!(Command::new(&binary).status().unwrap().success());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn production_swp_overflow_preserves_fresh_frames_and_rejects_missing_data() {
    let shim = include_str!("../sdk/san90_sdk.cpp");
    let fetch_start = shim.find("static int san90_swp_fetch(").unwrap();
    let fetch_end = fetch_start + shim[fetch_start..].find("// Stable optional metadata ABI").unwrap();
    let v1_start = shim.find("extern \"C\" int san90_swp_next_v1(").unwrap();
    let v1_end = v1_start + shim[v1_start..].find("extern \"C\" int san90_swp_close_v1(").unwrap();
    let harness = r#"
#include <cassert>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <initializer_list>
#include <vector>
#define SAN90_DISPATCH(state, expression)
#define APIRETVAL_WARNING_IFOverflow -12
struct Profile { double StartFreq_Hz = 0, StopFreq_Hz = 3; };
struct Aux { double RefClkFreqOffset = 0; };
struct San90SwpDevice {
    void* handle = this;
    Profile req;
    Aux aux;
    std::vector<double> freq = std::vector<double>(8);
    std::vector<float> power = std::vector<float>(8);
    uint32_t points = 5;
    std::vector<double> ifreq = std::vector<double>(8);
    std::vector<float> ipower = std::vector<float>(8);
};
struct Session { bool usable() const { return true; } } san90_session;
static int sdk_status = -12, mode = 0, fetches = 0;
static int SWP_GetFullSweep(void** handle, double* frequency, float* power, Aux* aux) {
    assert(handle && *handle);
    ++fetches;
    if (mode == 1) return sdk_status; // No frame: leave the prior trace untouched.
    for (uint32_t i = 0; i < (mode == 2 ? 4u : 5u); ++i) {
        frequency[i] = static_cast<double>(i) - 1;
        power[i] = i == 2 ? 0.0f : -40.0f - fetches;
    }
    if (mode == 3) frequency[1] = NAN;
    if (mode == 4) power[1] = INFINITY;
    aux->RefClkFreqOffset = 1.25;
    return sdk_status;
}
static void DSP_InterceptSpectrum(double start, double stop, const double* frequency,
        const float* power, uint32_t count, double* out_frequency, float* out_power,
        uint32_t* written) {
    *written = 0;
    for (uint32_t i = 0; i < count; ++i) {
        if (frequency[i] < start || frequency[i] > stop) continue;
        out_frequency[*written] = frequency[i];
        out_power[(*written)++] = power[i];
    }
}
"#;
    let assertions = r#"
int main() {
    San90SwpDevice state;
    void* handle = state.handle;
    double frequency[8]{};
    float power[8]{};
    uint32_t written = 0;
    double ppm = 0;
    // Several saturated sweeps must contain each new SDK measurement, then clear.
    for (int status : {-12, -12, -12, 0}) {
        sdk_status = status;
        assert(san90_swp_next_v2(&state, frequency, power, 8, &written, &ppm) == status);
        assert(written == 4 && frequency[0] == 0 && frequency[3] == 3);
        assert(power[0] == -40.0f - fetches && power[3] == power[0]);
        assert(std::isnan(power[1])); // Existing coverage-gap representation survives.
        assert(ppm == 1.25 && state.handle == handle);
    }
    // An absent/short/non-finite overflow frame never republishes the prior sweep.
    for (int invalid_mode : {1, 2, 3, 4}) {
        mode = invalid_mode;
        sdk_status = -12;
        written = 99;
        assert(san90_swp_next_v2(&state, frequency, power, 8, &written, &ppm) == -12);
        assert(written == 0 && state.handle == handle);
        sdk_status = 0;
        assert(san90_swp_next_v2(&state, frequency, power, 8, &written, &ppm) == -103);
        assert(written == 0);
    }
    mode = 0;
    for (int status : {-10, -8, -9, -7}) {
        sdk_status = status;
        assert(san90_swp_next_v2(&state, frequency, power, 8, &written, &ppm) == status);
        assert(written == 0);
    }
    sdk_status = -12;
    assert(san90_swp_next_v2(&state, frequency, power, 3, &written, &ppm) == -108);
    assert(written == 0); // Never truncate a sweep to a falsely complete frame.
    for (int status : {-12, -12, 0}) {
        sdk_status = status;
        assert(san90_swp_next_v1(&state, frequency, power, 8, &written) == status);
        assert(written == 5 && frequency[0] == -1 && frequency[4] == 3);
        assert(power[0] == -40.0f - fetches && power[2] == 0);
    }
    mode = 1;
    sdk_status = -12;
    assert(san90_swp_next_v1(&state, frequency, power, 8, &written) == -12);
    assert(written == 0);
}
"#;
    let directory = std::env::temp_dir().join(format!("websa_swp_overflow_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("overflow.cpp");
    let binary = directory.join("overflow");
    std::fs::write(&source, format!("{harness}\n{}\n{}\n{assertions}",
        &shim[fetch_start..fetch_end], &shim[v1_start..v1_end])).unwrap();
    assert!(Command::new("g++").args(["-std=c++17", "-O1"]).arg(&source).arg("-o").arg(&binary)
        .status().expect("g++ required for SWP overflow regression").success());
    assert!(Command::new(&binary).status().unwrap().success());
    std::fs::remove_dir_all(directory).unwrap();
}
