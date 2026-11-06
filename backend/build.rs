mod cw_build;

fn main() {
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
