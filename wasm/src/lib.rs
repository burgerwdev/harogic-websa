//! WebSA DSP core — the real-time signal processing the browser runs in a Web Worker.
//!
//! Layout (grown by the following steps, one module per stage of the pipeline):
//!   * `abi`      — the raw linear-memory boundary (no wasm-bindgen)
//!   * `ddc`      — NCO / FIR / resampler / AGC, the shared base layer for every mode
//!   * `analog`   — analog demodulator plugins (am, dsb, usb, lsb, cw, nfm, wfm, pm)
//!   * `digital`  — digital demodulator plugins (FT8 first); never sees the audio chain
//!   * `audio`    — audio enhancement for the analog PCM path only
//!   * `fft`      — the transform those stages share
//!
//! The crate builds natively (`cargo test` runs the kernels) and for
//! `wasm32-unknown-unknown` (the artifact the browser loads), with no dependencies.

pub mod abi;
pub mod ddc;

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
