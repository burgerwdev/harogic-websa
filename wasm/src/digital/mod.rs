//! Digital demodulators: protocol decoders that read the RAW complex baseband.
//!
//! These are the other half of the plugin design. They never see an `AnalogPcm`, so the audio
//! enhancement chain (AGC/denoiser/notch/blanker) is unreachable from here — an FT8 decoder needs
//! the signal's amplitude and phase as they arrived.
//!
//! FT8 is the first; the seam is what the other protocols (FT4, PSK, RTTY, SSTV, FreeDV) plug into.

pub mod ft8;
