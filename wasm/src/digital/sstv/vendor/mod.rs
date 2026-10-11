//! Vendored from `sstv` 0.3.1 — repo `unexcellent/sstv`, the whole `src/` tree.
//! Licence: MIT. Copyright the sstv authors.
//!
//! Slow-scan television, decode and encode, with no dependencies beyond `libm`. The upstream crate is
//! `no_std`-capable and its decode side needs only `alloc`, which is why it can live inside this
//! crate at all.
//!
//! Changes this project made. The protocol code is unchanged.
//!
//! * This is a *module* of `websa-dsp`, not a crate: the crate-level attributes are gone, the module
//!   table below is public, and `include!(concat!(env!("OUT_DIR"), ...))` became `include!` of a
//!   committed table (this crate has no build script of its own).
//! * Every `crate::` path was re-pointed at this subtree (`crate::digital::sstv::vendor::…`), which
//!   includes the `$crate::` inside the `#[macro_export]` unit macros. The macros land at this
//!   crate's root when exported, so the re-export at the bottom of this file makes the rewritten
//!   macro paths resolve.
//! * Upstream's doctests name the upstream crate (`use sstv::…`), so they are marked `ignore` rather
//!   than rewritten.
//! * One `let`-chain (`modes/mode.rs::sync_pulse`) needs edition 2024 and is rewritten as nested
//!   `if let`s with the same semantics.
//! * The one upstream test helper that used the `rand`/`rand_distr` dev-dependencies is rewritten on
//!   a small deterministic generator (`decode/demodulator.rs`, `noise`), so the whole upstream test
//!   suite runs here. The rest of the test code is untouched.
//! * The crate keeps one local cargo feature, `alloc` (on by default), so the upstream
//!   `#[cfg(feature = "alloc")]` gates keep their meaning without editing them. The upstream `wav`,
//!   `mp3` and `image` features are simply not enabled, which drops the code that needs those crates.
//!
//! The plugin that drives it is `crate::digital::sstv`.

// The upstream gates for `wav`, `mp3` and `image` name crates this build does not carry, so those
// features are not declared here, the code behind them is not compiled, and the lint is allowed
// rather than the gates rewritten (which would diverge from upstream for no benefit).
#![allow(unexpected_cfgs)]

pub mod decode;
pub mod encode;
pub mod error;
pub mod image;
pub mod modes;
pub mod units;

// The same re-exports the upstream `lib.rs` provides at the crate root, so the vendored files'
// root-relative imports keep working after the path rewrite above.
pub use decode::Demodulator;
pub use decode::{DecodedImage, DecodedRow, Decoder, Rows};
pub use encode::{Encoder, Synthesizer};
pub use error::{Error, Result};
pub use image::{RgbPixel, YuvPixel};
pub use modes::{Mode, VisCode};
pub use units::{Duration, Frequency, Tone};

// The unit macros are `#[macro_export]`, so they sit at this crate's root and the vendored files
// import them by name. Re-export them under the vendored path, which is what the path rewrite above
// made those imports say.
pub(crate) use crate::{Hz, ms, ns, tone, us, vis_code};
