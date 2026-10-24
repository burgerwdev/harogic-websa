//! Native build of the same MIT ggmorse core used by the browser.
pub fn build() {
    let vendor = "../tools/vendor/ggmorse";
    println!("cargo:rerun-if-changed=native/ggmorse_bridge.cpp");
    println!("cargo:rerun-if-changed={vendor}/src");
    println!("cargo:rerun-if-changed={vendor}/include");
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include(format!("{vendor}/include"))
        .include(format!("{vendor}/src"))
        .file("native/ggmorse_bridge.cpp")
        .file(format!("{vendor}/src/ggmorse.cpp"))
        .file(format!("{vendor}/src/resampler.cpp"))
        .compile("native_ggmorse");
}
