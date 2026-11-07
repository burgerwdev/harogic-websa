//! Shared IQ archive clocks and bounded production recording format.
//! Sample bytes and clocks are immutable; encoding never rescales components.
pub mod v2;
#[cfg(feature = "iq-experiments")]
mod v1;
#[cfg(feature = "iq-experiments")]
pub use v1::*;

use serde::{Deserialize, Serialize};
use std::io;

pub const MAX_BLOCK_BYTES: usize = 1_048_576;
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IqBlockMeta {
    pub receiver_uid: String,
    pub source_id: String,
    pub track_id: String,
    pub epoch: String,
    pub channel_id: String,
    pub config_revision: String,
    pub first_input_index: String,
    pub first_output_index: String,
    pub fraction_num: String,
    pub fraction_den: String,
    pub samples: u32,
    pub sample_format: String,
    pub amplitude_scale: f64,
    pub input_rate_hz: f64,
    pub actual_rate_hz: f64,
    pub delay_seconds: Option<f64>,
}
impl IqBlockMeta {
    fn validate(&self, bytes: usize) -> io::Result<()> {
        let indices = [
            &self.epoch,
            &self.channel_id,
            &self.config_revision,
            &self.first_input_index,
            &self.first_output_index,
            &self.fraction_num,
            &self.fraction_den,
        ];
        if self.sample_format != "cf32_le"
            || !self.amplitude_scale.is_finite()
            || self.amplitude_scale <= 0.0
            || self.samples == 0
            || self.samples as usize * 8 != bytes
            || bytes > MAX_BLOCK_BYTES
            || indices.iter().any(|s| {
                s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || s.parse::<u64>().is_err()
            })
            || self.fraction_den.parse::<u64>().unwrap_or(0) == 0
            || self.fraction_num.parse::<u64>().unwrap_or(u64::MAX) >= self.fraction_den.parse::<u64>().unwrap_or(0)
            || !self.input_rate_hz.is_finite()
            || self.input_rate_hz <= 0.0
            || !self.actual_rate_hz.is_finite()
            || self.actual_rate_hz <= 0.0
            || self
                .delay_seconds
                .is_some_and(|s| !s.is_finite() || s < 0.0)
        {
            return Err(invalid("invalid CF32 shape, size, identity or clock"));
        }
        Ok(())
    }
}
fn json<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| invalid(e.to_string()))
}
fn parse<T: for<'a> Deserialize<'a>>(bytes: &[u8]) -> io::Result<T> {
    serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))
}
