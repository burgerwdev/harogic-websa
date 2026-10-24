//! Link the committed wasm codec archives or the native FDK AAC library.
//!
//! The live receiver uses FDK's DRM transport for AAC, HE-AAC and xHE-AAC/USAC.
//! Native builds require `pkg-config` and `fdk-aac >= 2.0.3`; the standalone
//! libxaac diagnostic remains wasm-only.

fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if arch == "wasm32" {
        println!("cargo:rustc-link-search=native=fdk");
        println!("cargo:rustc-link-lib=static=fdk");
        println!("cargo:rustc-link-search=native=xaac");
        println!("cargo:rustc-link-lib=static=xaac");
    } else {
        println!("cargo:rerun-if-env-changed=PKG_CONFIG");
        println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
        println!("cargo:rerun-if-env-changed=PKG_CONFIG_LIBDIR");
        println!("cargo:rerun-if-env-changed=PKG_CONFIG_SYSROOT_DIR");
        let pkg_config = std::env::var_os("PKG_CONFIG").unwrap_or_else(|| "pkg-config".into());
        let version = std::process::Command::new(&pkg_config)
            .args(["--atleast-version=2.0.3", "fdk-aac"])
            .status()
            .expect("native DRM audio requires pkg-config and fdk-aac >= 2.0.3");
        assert!(version.success(), "native DRM audio requires fdk-aac >= 2.0.3 with DRM and USAC support");
        let output = std::process::Command::new(pkg_config)
            .args(["--libs", "fdk-aac"])
            .output()
            .expect("failed to query native fdk-aac linkage");
        assert!(output.status.success(), "pkg-config --libs fdk-aac failed");
        for flag in String::from_utf8(output.stdout).expect("pkg-config emitted invalid UTF-8").split_whitespace() {
            if let Some(path) = flag.strip_prefix("-L") {
                println!("cargo:rustc-link-search=native={path}");
            } else if let Some(lib) = flag.strip_prefix("-l") {
                println!("cargo:rustc-link-lib={lib}");
            } else {
                println!("cargo:rustc-link-arg={flag}");
            }
        }
    }
}
