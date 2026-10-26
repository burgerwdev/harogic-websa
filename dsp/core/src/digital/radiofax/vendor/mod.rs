// Copyright (c) 2026 boybook and Rasterwave contributors
// SPDX-License-Identifier: MIT

//! Dependency-free radiofax decoder vendored from rasterwave.rs 0.5.0.

mod decoder;
mod error;
pub mod fax;
mod fax_paper;
mod image;
#[cfg(test)]
mod oscillator;
mod paper;

pub use error::{Error, Result};
pub use fax_paper::{
    FaxPaperConfig, FaxPaperDecoder, FaxPaperEvent, FaxPaperEventRef, FaxPaperMode, FaxPaperSink,
};
pub use image::GrayImage;
pub use paper::PaperBoundaryKind;

#[cfg(test)]
mod fax_paper_tests;
#[cfg(test)]
mod fax_streaming_tests;
