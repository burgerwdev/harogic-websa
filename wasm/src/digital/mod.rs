//! Digital demodulators: protocol decoders that read the RAW complex baseband.
//!
//! These are the other half of the plugin design. They never see an `AnalogPcm`, so the audio
//! enhancement chain (AGC/denoiser/notch/blanker) is unreachable from here — an FT8 decoder needs
//! the signal's amplitude and phase as they arrived.
//!
//! FT8 is the first; the seam is what the other protocols (FT4, PSK, RTTY, SSTV, FreeDV) plug into.

pub mod drm;
pub mod drm2;
pub mod ft8;

use crate::plugin::DigitalDemodulator;

/// Build a digital demodulator for a plugin id, or `None` when the protocol has no kernel.
///
/// Mirrors `analog::build`: the registry's `implemented` flag and what can actually be built are
/// held together by a test, so a protocol cannot be advertised (or silently disabled) by accident.
/// This is also what makes the UI's digital mode reachable: the flag the UI reads comes from here.
pub fn build(id: &str, rate: f64) -> Option<Box<dyn DigitalDemodulator>> {
    match id {
        "ft8" => Some(Box::new(ft8::Ft8Plugin::new(rate))),
        // The drm2 receiver is the ported chain (sync -> OFDM -> chanest/tracking -> FAC/SDC/MSC
        // -> audio framing). It decodes live30.f32 end to end (FAC 64 ok/10 bad, 240 audio AUs,
        // station SAN90 DRM BENCH) and is now streaming-correct: the block-fed decode matches the
        // batch decode on the committed bench capture, which is what the wasm worker drives.
        "drm" => Some(Box::new(drm2::DrPlugin::new(rate))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{plugins, PluginKind};

    #[test]
    fn every_declared_digital_plugin_agrees_with_what_can_be_built() {
        let declared: Vec<&str> = plugins(PluginKind::Digital).iter().map(|p| p.id).collect();
        assert!(!declared.is_empty());
        for descriptor in plugins(PluginKind::Digital) {
            let built = build(descriptor.id, 48_000.0).is_some();
            assert_eq!(
                built, descriptor.implemented,
                "{}: registry says implemented={} but build() says {built}",
                descriptor.id, descriptor.implemented
            );
        }
        assert!(build("no_such_protocol", 48_000.0).is_none());
    }

    #[test]
    fn a_built_protocol_reports_its_registry_id() {
        for descriptor in plugins(PluginKind::Digital) {
            if let Some(demod) = build(descriptor.id, 48_000.0) {
                assert_eq!(demod.id(), descriptor.id);
            }
        }
    }
}
