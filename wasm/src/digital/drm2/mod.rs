//! The DRM receiver, re-implemented in Rust along Dream's stage structure.
//!
//! Why a new chain rather than more patches to the previous receiver: the previous one found
//! its super-frame phase once and kept it, estimated the channel per symbol with linear
//! interpolation and had no timing or carrier tracking. On one bench capture the reference
//! receiver (DecDRM's `decdrm rx`, which ports Dream's stages) decoded 200 audio frames where
//! the previous chain managed 55, with 9 FAC errors against our 52 — the deficit is in the
//! chain's structure, not in tunable constants.
//!
//! This module therefore follows Dream's stage order, each stage a plain Rust type fed by the
//! previous one and owning the state that belongs to it:
//!
//! ```text
//! baseband 48 kHz
//!   -> sync::freqacq    coarse carrier acquisition from the three continuous pilots
//!   -> sync::timesync   guard-interval correlation (low-passed, decimated), mode detection,
//!                       symbol timing and its tracking
//!   -> sync::framesync  frame phase from the time pilots
//!   -> ofdm             FFT demodulation into cells
//!   -> chanest          gain-reference grid, time- and frequency-Wiener estimation,
//!                       impulse-response tracking (delay/Doppler/SRO)
//!   -> fac / sdc        the signalling channels (FAC every frame, SDC in the super frame)
//!   -> mlc / interleave / msc   FEC, energy dispersal, deinterleaving, multiplex demux
//!   -> audio            the audio super frame (AAC and xHE layouts) into the codecs
//! ```
//!
//! The port is MIT-clean: Dream (`src/`, GPL-2.0) and DecDRM (`crates/decdrm-core`, GPL-2.0)
//! are read as algorithmic references and their behaviour is used as the oracle; no code from
//! either is copied. The stage list and the constants each stage uses are cross-checked against
//! them, and the geometry tables carry the Dream source they were checked against.
//!
//! The application keeps its current interface: the wasm ABI, the readout lines, the audio
//! PCM path and the backend's baseband AGC all stay as they are. The previous chain is deleted
//! once this one decodes (see `docs/en/DRM_HANDOFF.md`).

pub mod dsp;
pub mod cellmap;
pub mod chanest;
pub mod ofdm;
pub mod framesync;
pub mod params;
pub mod tables;
pub mod sync;
pub mod fec;
pub mod fac;
pub mod sdc;

// The stages land here in the order above, each with its own task and tests; `params` is the
// shared foundation.
