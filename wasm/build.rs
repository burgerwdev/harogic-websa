//! Links the prebuilt FDK AAC decoder (C++ → wasm32) and the Ittiam libxaac
//! xHE-AAC decoder (C → wasm32) into the crate.
//!
//! `wasm/fdk/libfdk.a` and `wasm/xaac/libxaac.a` are compiled once from their
//! sources (see `wasm/fdk/README.md`, `wasm/xaac/README.md`). They are only linked
//! for the wasm32 target; the native build used by `cargo test` does not link them.

fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if arch == "wasm32" {
        println!("cargo:rustc-link-search=native=fdk");
        println!("cargo:rustc-link-lib=static=fdk");
        println!("cargo:rustc-link-search=native=xaac");
        println!("cargo:rustc-link-lib=static=xaac");
    }
}
