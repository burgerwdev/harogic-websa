// Copyright (c) 2026 boybook and Rasterwave contributors
// SPDX-License-Identifier: MIT

use core::fmt;

/// Result type used by radiofax APIs.
pub type Result<T, E = Error> = core::result::Result<T, E>;

/// Errors returned by codec construction and validation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A sample rate cannot represent the required radiofax tones.
    InvalidSampleRate(u32),
    /// An image buffer length does not match its declared dimensions.
    InvalidImageBuffer {
        /// Expected number of pixels.
        expected: usize,
        /// Actual number of pixels.
        actual: usize,
    },
    /// A numeric configuration value is invalid.
    InvalidConfiguration(&'static str),
    /// A PCM chunk contained NaN or infinity.
    NonFiniteSample,
    /// Samples were supplied after the decoder was finalized.
    DecoderFinished,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate(rate) => write!(
                f,
                "sample rate {rate} Hz is outside the supported range 8,000..=384,000 Hz"
            ),
            Self::InvalidImageBuffer { expected, actual } => {
                write!(f, "image buffer has {actual} pixels, expected {expected}")
            }
            Self::InvalidConfiguration(message) => write!(f, "invalid configuration: {message}"),
            Self::NonFiniteSample => f.write_str("PCM input contains a non-finite sample"),
            Self::DecoderFinished => f.write_str(
                "decoder has already been finished; call reset before supplying more input",
            ),
        }
    }
}

impl std::error::Error for Error {}
