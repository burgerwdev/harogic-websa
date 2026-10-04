//! Synchronisation stages, in the order the chain runs them.
//!
//! - [`freqacq`]: coarse carrier acquisition from the three continuous pilots (Dream's
//!   `CFreqSyncAcq`) — everything downstream needs the DC carrier removed first.
//! - `timesync` (next): guard-interval correlation on a low-passed, decimated signal, mode
//!   detection and symbol timing with its tracking (Dream's `CTimeSync`).
//! - `framesync` (next): the frame phase from the time pilots (Dream's `CFrameSync`).

pub mod freqacq;
