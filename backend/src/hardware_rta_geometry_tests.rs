//! Real production shim geometry checks with the vendor calls replaced by stubs.
#[test]
#[ignore = "requires /opt/htraapi SDK headers and libhtraapi.so.0.55.100; no hardware access"]
fn production_rta_geometry_bounds_allocations_and_recovers() {
    let directory = std::env::temp_dir().join(format!("websa_rta_geometry_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let binary = directory.join("fixture");
    let built = std::process::Command::new("g++")
        .args(["-O1", "-std=c++17", "-pthread", "-I/opt/htraapi/inc"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/sdk/rta_geometry_test.cpp"
        ))
        .args([
            "-L/opt/htraapi/lib/x86_64",
            "-Wl,-rpath,/opt/htraapi/lib/x86_64",
            "-l:libhtraapi.so.0.55.100",
            "-o",
        ])
        .arg(&binary)
        .status()
        .unwrap();
    assert!(
        built.success(),
        "RTA production shim fixture failed to compile"
    );
    let result = std::process::Command::new(&binary).status().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        result.success(),
        "RTA production shim geometry contract failed"
    );
}
