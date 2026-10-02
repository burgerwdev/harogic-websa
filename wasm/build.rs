//! Links the prebuilt FDK AAC decoder (C++ → wasm32) into the crate.
//!
//! `wasm/fdk/libfdk.a` is compiled once from the Fraunhofer FDK AAC sources with
//! `clang++ --target=wasm32-unknown-unknown -ffreestanding -fno-exceptions …` (see
//! `wasm/fdk/README.md`). It is only linked for the wasm32 target; the native build
//! used by `cargo test` does not link it.

fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if arch == "wasm32" {
        println!("cargo:rustc-link-search=native=fdk");
        println!("cargo:rustc-link-lib=static=fdk");
    }
}
