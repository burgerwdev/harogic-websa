mod cw_build;

fn main() {
    generate_service_capacity();
    let path = "../contracts/stream-limits.json";
    println!("cargo:rerun-if-changed={path}");
    let values: std::collections::BTreeMap<String, usize> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("stream limits contract"))
            .expect("positive integer stream limits");
    assert!(values["MAX_DECODE_EVENT_BYTES"] + 8 <= values["DECODE_ACK_BYTES"],
        "ACK credit must admit a maximum-sized SDE2 frame including its header");
    let mut source = String::from("// Generated from contracts/stream-limits.json. Do not edit.\n");
    for (name, value) in values {
        assert!(value > 0 && name.bytes().all(|c| c.is_ascii_uppercase() || c == b'_'));
        source.push_str(&format!("pub const {name}: usize = {value};\n"));
    }
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(output.join("stream_limits.rs"), source).unwrap();
    cw_build::build();
}

fn generate_service_capacity() {
    let path = "../contracts/service-capacity.json";
    println!("cargo:rerun-if-changed={path}");
    let values: std::collections::BTreeMap<String, usize> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("service capacity contract"))
            .expect("positive integer service capacities");
    assert!(values["MAX_CHANNELS"] > 0 && values["CAPACITY_TARGET"] >= values["MAX_CHANNELS"]);
    let mut source = String::from("// Generated from contracts/service-capacity.json. Do not edit.\n");
    for (name, value) in values {
        assert!(value > 0 && name.bytes().all(|c| c.is_ascii_uppercase() || c == b'_'));
        source.push_str(&format!("pub const {name}: usize = {value};\n"));
    }
    // IQ, audio and results share one pool, sized for one task per channel.
    source.push_str("pub const MAX_RECORDING_TASKS: usize = MAX_CHANNELS;\n");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(output.join("service_capacity.rs"), source).unwrap();
}
