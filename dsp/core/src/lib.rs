// SPDX-License-Identifier: LicenseRef-Harogic-WebSA
// Project-owned code: AGPL-3.0-only with ../../../LICENSE-EXCEPTION.
// Third-party modules retain their own terms; see ../../../LICENSING.md.
//! WebSA DSP kernels shared by native receivers and browser Web Workers.
//!
//! Modules follow the signal-processing stages:
//!   * `abi`      — the raw linear-memory boundary (no wasm-bindgen)
//!   * `ddc`      — NCO / FIR / resampler / AGC, the shared base layer for every mode
//!   * `analog`   — analog demodulator plugins (am, dsb, usb, lsb, cw, nfm, wfm, pm)
//!   * `digital`  — FT8, SSTV, radiofax and DRM decoders; separate from the audio chain
//!   * `audio`    — audio enhancement for the analog PCM path only
//!   * `fft`      — the transform those stages share
//!
//! The crate builds natively (`cargo test` runs the kernels) and for
//! `wasm32-unknown-unknown` (the artifact the browser loads). The default/WASM
//! build has no Rust crate dependencies; native-fft enables rustfft for complex
//! FIRs in native receivers. Native DRM audio links FDK AAC; WASM links the codec archives.

pub mod abi;
pub mod analog;
pub mod audio;
pub mod ddc;
pub mod digital;

// The vendored sstv tree is written for `no_std` plus `alloc` and uses `alloc::` paths directly.
extern crate alloc;
pub mod fdk;
pub mod fft;
pub mod pipeline;
pub mod pipeline_abi;
pub mod plugin;
pub mod plugin_abi;
#[cfg(any(target_arch = "wasm32", test))]
pub mod xaac;

/// The ABI version the loader checks before it trusts any exported signature.
#[no_mangle]
pub extern "C" fn websa_dsp_version() -> u32 {
    abi::ABI_VERSION
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_exported_and_matches_the_abi() {
        assert_eq!(super::websa_dsp_version(), super::abi::ABI_VERSION);
    }
}
