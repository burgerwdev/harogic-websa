//! Executes the production reconfigure entry against an SDK that mutates before rejection.
use std::process::Command;

#[test]
fn rejected_swp_profile_restores_effective_profile_on_existing_handle() {
    let shim = include_str!("../sdk/san90_sdk.cpp");
    let start = shim.find("extern \"C\" int san90_swp_reconfig_v1(").unwrap();
    let end = start + shim[start..].find("// One sweep cut").unwrap();
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
