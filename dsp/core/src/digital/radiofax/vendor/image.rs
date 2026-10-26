// Copyright (c) 2026 boybook and Rasterwave contributors
// SPDX-License-Identifier: MIT

use super::{Error, Result};

/// Owned row-major eight-bit grayscale image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrayImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl GrayImage {
    /// Construct an image after validating its buffer size.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        let expected = pixel_count(width, height);
        if pixels.len() != expected {
            return Err(Error::InvalidImageBuffer {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Image width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Image height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Row-major luminance buffer.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

fn pixel_count(width: u32, height: u32) -> usize {
    (width as usize).saturating_mul(height as usize)
}
