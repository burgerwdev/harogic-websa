//! The plugin registry: what a mode/stage *is*, independent of how it is implemented.
//!
//! The architecture is a set of three plugin families over one shared DDC base layer:
//!
//! * `AnalogDemodulator` — am/dsb/usb/lsb/cw/nfm/wfm/pm, feeding the audio path
//! * `DigitalDemodulator` — FT8 first, feeding the raw path (never the audio chain)
//! * `AudioStage` — LPF/AGC/squelch/Wiener/notch/blanker, analog PCM only
//!
//! Mode lists are *declared here once* and exported to the browser (`websa_dsp_plugin_*`), so the
//! UI cannot carry a second, drifting list of modes. A descriptor also carries `implemented`, and
//! the registry test asserts that flag agrees with what `build` can actually produce — a mode that
//! is listed but not written yet shows up as listed-and-not-implemented instead of as a crash.

/// Which family a plugin belongs to (the ABI passes this as a number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PluginKind {
    Analog = 0,
    Digital = 1,
    Audio = 2,
    Ddc = 3,
}

impl PluginKind {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(PluginKind::Analog),
            1 => Some(PluginKind::Digital),
            2 => Some(PluginKind::Audio),
            3 => Some(PluginKind::Ddc),
            _ => None,
        }
    }

    pub fn all() -> [PluginKind; 4] {
        [PluginKind::Analog, PluginKind::Digital, PluginKind::Audio, PluginKind::Ddc]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginDescriptor {
    /// Stable id used on the wire (lowercase, no spaces). Never renamed: it is the UI's key.
    pub id: &'static str,
    pub kind: PluginKind,
    /// False while the mode is declared but its kernel is not written yet.
    pub implemented: bool,
    /// True when this plugin is part of the audio-enhancement chain (analog PCM path only).
    pub audio_enhancement: bool,
}

const fn analog(id: &'static str, implemented: bool) -> PluginDescriptor {
    PluginDescriptor { id, kind: PluginKind::Analog, implemented, audio_enhancement: false }
}

const fn digital(id: &'static str, implemented: bool) -> PluginDescriptor {
    PluginDescriptor { id, kind: PluginKind::Digital, implemented, audio_enhancement: false }
}

const fn audio(id: &'static str, implemented: bool) -> PluginDescriptor {
    PluginDescriptor { id, kind: PluginKind::Audio, implemented, audio_enhancement: true }
}

const fn ddc(id: &'static str) -> PluginDescriptor {
    PluginDescriptor { id, kind: PluginKind::Ddc, implemented: true, audio_enhancement: false }
}

/// Analog demodulator families. The `am`/`fm` pair is the first to land; the rest follow the same
/// interface, and `dsb`/`ssb`/`cw`/`pm` differ only in their band selection and detector.
pub const ANALOG_PLUGINS: &[PluginDescriptor] = &[
    analog("am", true),
    analog("dsb", true),
    analog("usb", true),
    analog("lsb", true),
    analog("cw", true),
    analog("nfm", true),
    analog("wfm", true),
    analog("pm", true),
];

/// Digital demodulators. FT8 is the first; DRM joins it on the same seam.
pub const DIGITAL_PLUGINS: &[PluginDescriptor] = &[
    digital("ft8", true),
    digital("drm", true),
];

/// Audio-enhancement stages, in the order the chain applies them. Every one of these is
/// **analog-path only** — see `pipeline`.
pub const AUDIO_PLUGINS: &[PluginDescriptor] = &[
    audio("dc_block", true),
    audio("lpf", true),
    audio("agc", true),
    audio("squelch", true),
    audio("wiener", true),
    audio("notch", true),
    audio("blanker", true),
];

/// The DDC base layer's stages: always present, and the only stage set both paths share.
pub const DDC_PLUGINS: &[PluginDescriptor] = &[
    ddc("nco"),
    ddc("fir"),
    ddc("decimate"),
    ddc("resample"),
    ddc("agc"),
];

pub fn plugins(kind: PluginKind) -> &'static [PluginDescriptor] {
    match kind {
        PluginKind::Analog => ANALOG_PLUGINS,
        PluginKind::Digital => DIGITAL_PLUGINS,
        PluginKind::Audio => AUDIO_PLUGINS,
        PluginKind::Ddc => DDC_PLUGINS,
    }
}

pub fn find(kind: PluginKind, id: &str) -> Option<&'static PluginDescriptor> {
    plugins(kind).iter().find(|plugin| plugin.id == id)
}

/// Everything a demodulator needs to be created: the DDC output rate and the mode's
/// bandwidth/pitch/sideband parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DemodConfig {
    /// Rate of the complex baseband it receives (Hz).
    pub fs: f64,
    /// IF bandwidth of the mode (Hz).
    pub if_bw: f64,
    /// Audio/CW sidetone pitch (Hz); ignored by the modes without one.
    pub pitch: f64,
}

impl DemodConfig {
    pub fn new(fs: f64, if_bw: f64) -> Self {
        Self { fs, if_bw, pitch: 700.0 }
    }
}

/// An analog demodulator: complex baseband in, real audio out.
///
/// It must be streaming: state carries across blocks, and `retune` clears the channel history
/// without resetting the caller's level control (`pipeline` owns that).
pub trait AnalogDemodulator: Send {
    fn id(&self) -> &'static str;
    /// Demodulate `iq` (interleaved complex) into `out` (cleared first).
    fn process_into(&mut self, iq: &[f32], out: &mut Vec<f32>);
    /// New centre within the same capture: drop the channel history, keep the level.
    fn retune(&mut self);
    fn reset(&mut self);

    /// Change the de-emphasis time constant (microseconds, `<= 0` = none). A mode with no such
    /// control ignores it; the analog modes all have one (it is what a broadcast pre-emphasis needs
    /// undone), so the default is a no-op for stand-ins only.
    fn set_deemph_us(&mut self, _tau_us: f64) {}

    /// The de-emphasis in force, in microseconds (0 = none). Diagnostics and the status readout.
    fn deemph_us(&self) -> f64 {
        0.0
    }
}

/// What a protocol knows about its most recent decode (shown in the UI).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DigitalReport {
    pub frequency_hz: f64,
    pub time_offset_s: f64,
    pub snr_db: f64,
}

/// A digital demodulator: complex baseband in, whatever the protocol produces out.
///
/// The output is deliberately opaque: FT8 produces a decoded message, not audio. A protocol that
/// needs the samples keeps its own buffer, and the audio chain is unreachable from here.
pub trait DigitalDemodulator: Send {
    fn id(&self) -> &'static str;
    /// Feed a block of interleaved complex baseband. Returns lines of decoded text produced by
    /// this block (empty when the block completed no message).
    fn process_iq(&mut self, iq: &[f32]) -> Vec<String>;
    fn reset(&mut self);

    /// Measurements for the most recent decode (frequency, slot offset, SNR), when the protocol
    /// has any to report. Defaults to nothing: a protocol is not obliged to measure anything.
    fn last_report(&self) -> Option<DigitalReport> {
        None
    }

    /// Every message the most recent `process_iq` decoded, with its measurements, so the UI can
    /// show each transmission a busy slot contained. Empty when the block decoded nothing.
    fn decoded(&self) -> Vec<(String, DigitalReport)> {
        Vec::new()
    }

    /// Input samples buffered towards the next decode attempt (0 when the protocol decodes per
    /// block). Used by the status readout, so "running but never finishing a transmission" is
    /// visible instead of looking like a quiet band.
    fn buffered_input(&self) -> usize {
        0
    }

    /// Equalised constellation points (I, Q) for the most recent decode, when the
    /// protocol has any to show (e.g. the DRM FAC 4-QAM constellation). Defaults to
    /// empty: a protocol is not obliged to produce one.
    fn constellation(&self) -> Vec<(f64, f64)> {
        Vec::new()
    }

    /// Decoded 16-bit interleaved PCM the demodulator produced (e.g. the DRM audio
    /// stream), DRAINED: the call returns and clears everything accumulated since the
    /// last call, so a streaming consumer (the DSP worker) can never fall behind a
    /// growing buffer and the receiver's memory stays bounded. Empty for protocols
    /// that produce only text (FT8) or no audio.
    fn take_audio_pcm(&mut self) -> Vec<i16> {
        Vec::new()
    }

    /// The decoded PCM's sample rate in Hz (0 when there is no audio).
    fn audio_rate_hz(&self) -> u32 {
        0
    }
}

/// One stage of the analog audio chain (LPF/AGC/squelch/Wiener/notch/blanker).
///
/// Two stages expose a control the listener owns (the noise reducer's strength, the squelch's
/// threshold). They are optional capabilities rather than a separate interface: a stage that has
/// neither answers `false`, and the chain reports whether the setting reached a stage at all.
pub trait AudioStage: Send {
    fn id(&self) -> &'static str;
    /// Process real PCM in place. `hold` freezes any adaptation (a reconfiguration transient).
    fn process_into(&mut self, input: &[f32], hold: bool, out: &mut Vec<f32>);
    fn reset(&mut self);

    /// How hard the stage should work, 0..1. `false` when it has no such knob.
    fn set_strength(&mut self, _strength: f64) -> bool {
        false
    }

    /// A level threshold in dBFS (the squelch gate). `false` when the stage has no such knob.
    fn set_threshold(&mut self, _dbfs: f64) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn plugin_ids_are_unique_inside_their_family() {
        for kind in PluginKind::all() {
            let ids: Vec<&str> = plugins(kind).iter().map(|p| p.id).collect();
            let unique: HashSet<&str> = ids.iter().copied().collect();
            assert_eq!(ids.len(), unique.len(), "{kind:?} has duplicate ids: {ids:?}");
            assert!(!ids.is_empty(), "{kind:?} must not be empty");
            for plugin in plugins(kind) {
                assert_eq!(plugin.kind, kind);
                assert!(
                    plugin.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "plugin id '{}' must be a stable lowercase key",
                    plugin.id
                );
            }
        }
    }

    #[test]
    fn the_analog_family_covers_every_documented_mode() {
        let ids: Vec<&str> = plugins(PluginKind::Analog).iter().map(|p| p.id).collect();
        for expected in ["am", "dsb", "usb", "lsb", "cw", "nfm", "wfm", "pm"] {
            assert!(ids.contains(&expected), "analog mode {expected} is not registered");
        }
    }

    #[test]
    fn only_audio_stages_are_marked_as_audio_enhancement() {
        for kind in [PluginKind::Analog, PluginKind::Digital, PluginKind::Ddc] {
            for plugin in plugins(kind) {
                assert!(!plugin.audio_enhancement, "{} must not be an audio stage", plugin.id);
            }
        }
        assert!(plugins(PluginKind::Audio).iter().all(|p| p.audio_enhancement));
    }

    #[test]
    fn lookup_matches_the_declared_list() {
        for kind in PluginKind::all() {
            for plugin in plugins(kind) {
                assert_eq!(find(kind, plugin.id).map(|p| p.id), Some(plugin.id));
            }
            assert!(find(kind, "no_such_plugin").is_none());
        }
    }

    #[test]
    fn kind_round_trips_through_the_abi_number() {
        for kind in PluginKind::all() {
            assert_eq!(PluginKind::from_u32(kind as u32), Some(kind));
        }
        assert_eq!(PluginKind::from_u32(99), None);
    }
}
