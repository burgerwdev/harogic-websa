// SPDX-License-Identifier: LicenseRef-Harogic-WebSA
// Project-owned code: AGPL-3.0-only with ../../LICENSE-EXCEPTION; see ../../LICENSING.md.
//! Native IQ channelization, fine DDC, and ordered baseband framing.
mod fine;
mod narrow;
mod fir_fft;
mod pfb;
mod pfb_pool;
mod rational;
pub mod rate_plan;
pub mod receiver;
pub mod rf_measure;
pub mod cw;
pub mod nr;
mod nr_neural;
mod nr_resample;
pub mod catalog;
pub mod external;
mod wire;
/// Shared Rust/browser wire budgets, generated from contracts/stream-limits.json.
pub mod stream_limits { include!(concat!(env!("OUT_DIR"), "/stream_limits.rs")); }
pub mod service_capacity { include!(concat!(env!("OUT_DIR"), "/service_capacity.rs")); }
mod iq_block;
pub use iq_block::*;
pub mod filter_plans;
pub mod host_resources;
pub mod model_preparation;
pub mod timing;
pub mod capture_scheduling;
#[cfg(feature="bench-instrument")]
pub mod execution_probe;

pub use fine::*;
pub use narrow::*;
pub use pfb::*;
pub use pfb_pool::*;
pub use wire::*;

pub mod iq_archive;
