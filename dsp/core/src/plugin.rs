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
    digital("drmplus", true),
    digital("sstv", true),
    digital("radiofax", true),
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
    /// Same channel after a backend reconfiguration. WFM keeps confirmed station metadata;
    /// other modes use the ordinary retune behavior.
    fn retune_stationary(&mut self) {
        self.retune();
    }
    fn reset(&mut self);

    /// Change the de-emphasis time constant (microseconds, `<= 0` = none). A mode with no such
    /// control ignores it; the analog modes all have one (it is what a broadcast pre-emphasis needs
    /// undone), so the default is a no-op for stand-ins only.
    fn set_deemph_us(&mut self, _tau_us: f64) {}

    /// The panel's AGC switch: `true` (the default) lets the level stage adapt, `false` passes
    /// the PCM at unity gain. No-op for demodulators without a level stage.
    fn set_agc_enabled(&mut self, _on: bool) {}

    /// The de-emphasis in force, in microseconds (0 = none). Diagnostics and the status readout.
    fn deemph_us(&self) -> f64 {
        0.0
    }

    /// Drain the stereo difference signal (L−R) the demodulator recovered, at the consumer's rate,
    /// aligned with the `process_into` PCM. Empty when the mode has no stereo side chain (every
    /// mode but WFM, or a baseband too narrow to carry the subcarriers).
    fn take_side(&mut self) -> Vec<f32> {
        Vec::new()
    }

    /// Whether the stereo pilot is currently detected (the badge's state).
    fn stereo_detected(&self) -> bool {
        false
    }

    /// The smoothed peak deviation in Hz (0 when the mode has no measurement).
    fn peak_deviation_hz(&self) -> f64 {
        0.0
    }

    /// The smoothed pilot-to-carrier ratio, 0..1 (the stereo badge's confidence).
    fn pilot_ratio(&self) -> f64 {
        0.0
    }

    /// The pilot's own deviation in Hz (a station constant; 0 when there is no pilot).
    fn pilot_deviation_hz(&self) -> f64 {
        0.0
    }

    /// The pilot's narrow-band SNR in dB (the pilot's band over the band beside it).
    /// Bench diagnostics for the WFM side chain (see `FmStereo::diag`); zeros elsewhere.
    fn stereo_diag(&self) -> [f64; 4] {
        [0.0; 4]
    }

    fn pilot_snr_db(&self) -> f64 {
        0.0
    }

    /// The decoded RDS state, when the mode carries the 57 kHz subcarrier.
    fn rds_state(&self) -> Option<crate::analog::rds::RdsState> {
        None
    }

    /// RDS bench diagnostics (see `fm_stereo::FmStereo::rds_diag`); zeros without a decoder.
    fn rds_diag(&self) -> [f64; 4] {
        [0.0; 4]
    }
}

/// What a protocol knows about its most recent decode (shown in the UI).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DigitalReport {
    pub frequency_hz: f64,
    pub time_offset_s: f64,
    pub snr_db: f64,
}

/// An RGB888 picture a digital protocol decoded, borrowed from the demodulator.
///
/// Row-major, `width * height * 3` bytes. A protocol that produces a picture (SSTV) returns it from
/// [`DigitalDemodulator::image`]; the copy out of the module and into the page belongs to the ABI.
pub struct ImageRef<'a> {
    pub rgb: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// What the protocol knows about the picture it is showing.
///
/// Separate from [`ImageRef`] because the metadata must be readable *before* the pixels (the pane
/// lays out its readout and decides whether the picture grew) and because reading it is cheap where
/// copying the picture is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageStatus {
    pub width: u32,
    pub height: u32,
    /// Scanlines decoded in this picture so far.
    pub lines: u32,
    /// `lines / height` as a whole percentage.
    pub progress: u32,
    /// The protocol's own mode identifier (SSTV: the VIS code).
    pub vis: u32,
    /// Bumped once per transmission: two reads with the same serial are the same picture, growing.
    pub serial: u64,
    /// True once the last scanline arrived.
    pub complete: bool,
    /// Index into the protocol's own mode table (SSTV: `digital::sstv::SSTV_MODES`).
    pub mode_index: u32,
}

/// Structured native receiver signalling, independent of presentation text.
#[derive(Debug, Clone, Default)]
pub struct DigitalStatus {
    pub locked: bool,
    pub mode: Option<String>,
    pub station_label: Option<String>,
    pub carrier_offset_hz: f64,
    pub fac_ok: u64,
    pub fac_errors: u64,
    pub sdc_ok: u64,
    pub services: Vec<DigitalService>,
}

#[derive(Debug, Clone, Default)]
pub struct DigitalService {
    pub service_id: u32,
    pub label: Option<String>,
    pub audio_coding: Option<u8>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
}

/// One ordered codec output segment, retaining its own actual format and IQ provenance.
#[derive(Debug)]
pub struct DecodedAudioChunk {
    pub pcm: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u32,
    /// Earliest contributing decoder IQ sample; a provenance bound, not PCM presentation time.
    pub source_position: Option<(u64,u64,u64)>,
    pub codec_delay_samples: Option<u32>,
    /// Known causal buffering in decoder IQ samples, including FDK delay when available.
    pub total_delay_samples: Option<f64>,
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

    /// A retune on the backend: the channel moved. The default drops the decoder state (FT8's window,
    /// DRM's lock). SSTV keeps its buffer, because the backend also flushes on a reconfiguration of the
    /// same channel.
    fn retune(&mut self) {
        self.reset();
    }

    /// Input samples buffered towards the next decode attempt (0 when the protocol decodes per
    /// block). Used by the status readout, so "running but never finishing a transmission" is
    /// visible instead of looking like a quiet band.
    fn buffered_input(&self) -> usize {
        0
    }

    /// RMS of what the decoder has buffered, 0..1. Zero while samples are arriving means the decoder's
    /// own band passes nothing: a tuning or band problem, not a decoding one.
    fn audio_level(&self) -> f32 {
        0.0
    }

    /// Decode attempts since creation or reset. A decoder that is fed and never tries is a different
    /// fault from one that tries and finds nothing.
    fn attempts(&self) -> u64 {
        0
    }

    /// Equalised constellation points (I, Q) for the most recent decode, when the
    /// protocol has any to show (e.g. the DRM FAC 4-QAM constellation). Defaults to
    /// empty: a protocol is not obliged to produce one.
    fn constellation(&self) -> Vec<(f64, f64)> {
        Vec::new()
    }

    /// The picture the protocol has decoded, when it produces one (SSTV). Defaults to nothing: a
    /// protocol that decodes text, audio or nothing at all is unaffected by this method existing.
    fn image(&self) -> Option<ImageRef<'_>> {
        None
    }

    /// The state of that picture (geometry, progress, mode). Read before the pixels: the pane needs
    /// the readout even when nothing decoded yet, and the geometry is what sizes the canvas.
    fn image_status(&self) -> Option<ImageStatus> {
        None
    }

    /// Pin the decoder to one of the protocol's own modes (`None` = decide from the signal). Returns
    /// false when the protocol has no such knob, which is every protocol but SSTV so far.
    fn set_mode_pin(&mut self, _index: Option<usize>) -> bool {
        false
    }

    /// Drain the next decoded PCM format segment. Query audio_rate_hz/audio_channels after
    /// draining for its format; use take_audio_chunks to drain every segment with provenance.
    /// Empty for protocols that produce only text (FT8) or no audio.
    fn take_audio_pcm(&mut self) -> Vec<i16> {
        Vec::new()
    }

    /// Like [`DigitalDemodulator::take_audio_pcm`], but drains at most `limit` samples and keeps
    /// the rest buffered for the next call. The wasm ABI's output block is fixed-size, so a
    /// plain drain would drop everything past the block; this lets a burst (the DRM audio arrives
    /// once per super frame) be handed over across several reads without losing a sample.
    fn drain_audio_pcm(&mut self, limit: usize) -> Vec<i16> {
        let _ = limit;
        self.take_audio_pcm()
    }

    /// The decoded PCM's sample rate in Hz (0 when there is no audio).
    fn audio_rate_hz(&self) -> u32 {
        0
    }

    /// Actual interleaved PCM channel count; codecs override this after decoding.
    fn audio_channels(&self) -> u32 {
        1
    }

    /// Additional decoded PCM delay reported by the codec, at audio_rate_hz().
    fn audio_codec_delay_samples(&self) -> Option<u32> {
        None
    }

    /// Earliest contributing decoder-input IQ position for the first buffered PCM block.
    fn audio_source_position(&self) -> Option<(u64,u64,u64)> {
        None
    }

    /// Observed decoder buffering plus codec delay, in decoder-input IQ samples.
    fn audio_total_delay_samples(&self) -> Option<f64> {
        None
    }

    /// Drain all ordered segments, preserving actual codec format and delay boundaries.
    fn take_audio_chunks(&mut self) -> Vec<DecodedAudioChunk> {
        Vec::new()
    }

    fn status(&self) -> Option<DigitalStatus> {
        None
    }
    /// Downcast to the radiofax page/control surface. Other protocols return `None`.
    fn as_radiofax(&mut self) -> Option<&mut dyn RadiofaxControl> {
        None
    }

}

/// Product controls shared by the plugin and the receiver configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct RadiofaxConfigView {
    pub acquisition: String,
    pub ioc: String,
    pub lpm: String,
    pub modulation: String,
    pub polarity: String,
    pub clock_recovery: String,
    pub phase_pixels: f32,
    pub clock_ppm: f32,
}

/// Gray8 page metadata a radiofax receiver publishes without using the SSTV RGB path.
#[derive(Debug, Clone, PartialEq)]
pub struct RadiofaxImageMeta {
    pub serial: u64,
    pub ioc: u16,
    pub lpm: u16,
    pub lines: u32,
    pub complete: bool,
    pub partial: bool,
    pub segment_index: u32,
    pub transmission_id: u64,
    /// Cumulative sorted unique local row indices that were erased in this
    /// segment. Every index is strictly less than `lines`.
    pub missing_rows: Vec<u32>,
    /// Why the current page is partial, degraded, or closed. `None` while reception is ordinary.
    pub reason: Option<String>,
}

/// One corrected Gray8 row span. `gray` is tightly packed and exactly `width * rows` bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct RadiofaxImageSpan {
    pub width: u32,
    pub row: u32,
    pub rows: u32,
    pub revision: u64,
    pub gray: Vec<u8>,
    pub meta: RadiofaxImageMeta,
}

/// The live radiofax readout. Stage and clock names are the wire vocabulary.
#[derive(Debug, Clone, PartialEq)]
pub struct RadiofaxState {
    pub running: bool,
    pub stage: &'static str,
    pub has_picture: bool,
    pub signal_level: f32,
    pub carrier_coherence: f32,
    pub ioc: Option<u16>,
    pub lpm: Option<u16>,
    pub modulation: &'static str,
    pub polarity: &'static str,
    pub width: u32,
    pub lines: u32,
    pub elapsed_seconds: f64,
    pub clock_status: &'static str,
    pub clock_source: &'static str,
    pub phase_pixels: f32,
    pub clock_ppm: f32,
    pub clock_confidence: f32,
    pub complete: bool,
    pub partial: bool,
    pub reason: Option<String>,
}

/// Read-only page snapshot plus the controls that do not rebuild the RF receiver.
///
/// Only radiofax implements this. SSTV and DRM keep their own image and status paths.
pub trait RadiofaxControl {
    fn radiofax_config(&self) -> RadiofaxConfigView;
    fn radiofax_state(&self) -> RadiofaxState;
    /// Corrected spans produced since the previous call, at most 64 rows each.
    fn take_radiofax_spans(&mut self) -> Vec<RadiofaxImageSpan>;
    fn apply_radiofax(&mut self, config: &RadiofaxConfigView, new_page: bool) -> Result<(), String>;
    fn finish_input(&mut self) -> Result<(), String> { Ok(()) }
    fn mark_discontinuity(&mut self) -> Result<(), String> { Ok(()) }
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

    /// The squelch gate's threshold in dBm.
    fn set_threshold(&mut self, _dbm: f64) -> bool {
        false
    }

    /// A calibrated pre-AGC channel power in dBm (NaN clears an expired measurement).
    fn set_level_dbm(&mut self, _dbm: f64) -> bool {
        false
    }

    /// Actual squelch gate state and linear gain, when this stage is a squelch.
    fn squelch_status(&self) -> Option<(bool, f64)> {
        None
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
