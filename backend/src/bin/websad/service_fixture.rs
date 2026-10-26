use super::*;
use super::receiver_workers::ChannelConfig;
use super::sdr_pipeline::ChannelRegistryEntry;
use std::f64::consts::TAU;
use std::sync::LazyLock;
use websa_channelizer::{encode_baseband, BasebandMagic};

/// Independent native-receiver inputs for the fake device. These signals are
/// already in normalized ADC units, so IQv2 encoding must not scale them again.
pub(super) struct FixtureSources {
    slots: BTreeMap<u32, FixtureSlot>,
    registry: Arc<[ChannelRegistryEntry]>,
}

struct FixtureSlot {
    config: ChannelConfig,
    entry: ChannelRegistryEntry,
    source: FixtureSource,
    sequence: u32,
    input_index: u64,
    iq: Vec<f32>,
}

enum FixtureSource {
    Replay { iq: Arc<[f32]>, period_samples: usize },
    Cw { key: Arc<[f32]> },
    Analog,
    Radiofax,
}

// The launcher runs from the checkout root; direct launches may override the
// external fixture directory without depending on the build machine's paths.
fn fixture_root() -> std::path::PathBuf {
    if let Some(root) = std::env::var_os("WEBSA_FIXTURES") {
        return root.into();
    }
    #[cfg(test)]
    return std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures");
    #[cfg(not(test))]
    std::path::PathBuf::from("tests/fixtures")
}

pub(super) fn fixture(path: &str) -> Result<Arc<[f32]>, String> {
    let full = fixture_root().join(path);
    let bytes = std::fs::read(&full)
        .map_err(|error| format!("reading fixture {}: {error}", full.display()))?;
    if bytes.is_empty() || bytes.len() % 8 != 0 {
        return Err(format!("fixture {} must contain complete f32 IQ pairs", full.display()));
    }
    let iq: Vec<f32> = bytes.chunks_exact(4)
        .map(|value| f32::from_le_bytes(value.try_into().unwrap())).collect();
    if iq.iter().any(|value| !value.is_finite()) {
        return Err(format!("fixture {} contains non-finite IQ", full.display()));
    }
    Ok(iq.into())
}

impl FixtureSource {
    fn for_mode(mode: &str) -> Result<(Self, f64), String> {
        static FT8: LazyLock<Result<Arc<[f32]>, String>> =
            LazyLock::new(|| fixture("ft8/ft8_cq_iq.bin"));
        static SSTV: LazyLock<Result<Arc<[f32]>, String>> =
            LazyLock::new(|| fixture("sstv/robot36_iq.bin"));
        static DRM: LazyLock<Result<Arc<[f32]>, String>> =
            LazyLock::new(|| fixture("drm/drm_modeB_so3_48k_aac.f32"));
        static DRMPLUS: LazyLock<Result<Arc<[f32]>, String>> =
            LazyLock::new(|| fixture("drm/drm_modeE_so0_96k_aac.f32"));
        static CW: LazyLock<Arc<[f32]>> = LazyLock::new(|| {
            let mut key = vec![0.0; 9_600];
            key.extend(cw_keying("CQ DE W1ABC", 20.0, 48_000.0));
            key.extend(std::iter::repeat_n(0.0, 48_000 * 3));
            key.into()
        });
        let (iq, rate, period) = match mode {
            "ft8" => (FT8.clone()?, 48_000.0, Some(720_000)),
            "sstv" => (SSTV.clone()?, 8_000.0, None),
            "drm" => (DRM.clone()?, 48_000.0, None),
            "drmplus" => (DRMPLUS.clone()?, 96_000.0, None),
            "cw" => {
                return Ok((Self::Cw { key: CW.clone() }, 48_000.0));
            }
            "wfm" => return Ok((Self::Analog, 384_000.0)),
            "am" | "dsb" | "usb" | "lsb" | "nfm" | "pm" => {
                return Ok((Self::Analog, 48_000.0));
            }
            "radiofax" => return Ok((Self::Radiofax, 12_000.0)),
            _ => return Err(format!("no fixture source for receiver mode {mode}")),
        };
        let period_samples = period.unwrap_or(iq.len() / 2);
        if iq.len() / 2 > period_samples {
            return Err(format!("{mode} fixture exceeds its transmission period"));
        }
        Ok((Self::Replay { iq, period_samples }, rate))
    }

    fn fill(&self, config: &ChannelConfig, rate: f64, first: u64, count: usize, out: &mut Vec<f32>) {
        let mode = config.mode.as_str();
        out.clear();
        match self {
            Self::Replay { iq, period_samples } => {
                for index in first..first + count as u64 {
                    let offset = (index % *period_samples as u64) as usize * 2;
                    if offset < iq.len() {
                        out.extend_from_slice(&iq[offset..offset + 2]);
                    } else {
                        // FT8 occupies 12.64 seconds of its 15-second slot.
                        out.extend_from_slice(&[0.0, 0.0]);
                    }
                }
            }
            Self::Cw { key } => {
                for index in first..first + count as u64 {
                    let amplitude = 0.4 * key[(index % key.len() as u64) as usize] as f64;
                    let phase = TAU * config.pitch * index as f64 / rate;
                    out.extend_from_slice(&[(amplitude * phase.cos()) as f32,
                        (amplitude * phase.sin()) as f32]);
                }
            }
            Self::Analog => {
                for index in first..first + count as u64 {
                    let t = index as f64 / rate;
                    let audio_phase = TAU * 1_000.0 * t;
                    let (re, im) = match mode {
                        "am" => (0.4 * (1.0 + 0.5 * audio_phase.cos()), 0.0),
                        "dsb" => (0.32 + 0.2 * audio_phase.cos(), 0.0),
                        "usb" | "lsb" => {
                            let phase = TAU * 1_200.0 * t * if mode == "lsb" { -1.0 } else { 1.0 };
                            (0.4 * phase.cos(), 0.4 * phase.sin())
                        }
                        "nfm" => {
                            let phase = audio_phase.sin();
                            (0.4 * phase.cos(), 0.4 * phase.sin())
                        }
                        "pm" => {
                            let phase = 0.6 * audio_phase.cos();
                            (0.4 * phase.cos(), 0.4 * phase.sin())
                        }
                        "wfm" => {
                            // Integrate an FM stereo multiplex analytically:
                            // 12.5 kHz mono, 2.25 kHz pilot and 3.75 kHz L-R.
                            let phase = 12.5 * audio_phase.sin()
                                - (2_250.0 / 19_000.0) * (TAU * 19_000.0 * t).cos()
                                + (1_875.0 / 39_000.0) * (TAU * 39_000.0 * t).sin()
                                + (1_875.0 / 37_000.0) * (TAU * 37_000.0 * t).sin();
                            (0.4 * phase.cos(), 0.4 * phase.sin())
                        }
                        _ => unreachable!("source mode validated during configuration"),
                    };
                    out.extend_from_slice(&[re as f32, im as f32]);
                }
            }
            Self::Radiofax => {
                for index in first..first + count as u64 {
                    let t = index as f64 / rate;
                    let audio = TAU * 1_900.0 * t;
                    out.extend_from_slice(&[(0.25 * audio.cos()) as f32, 0.0]);
                }
            }
        }
    }
}

impl FixtureSlot {
    fn new(config: ChannelConfig) -> Result<Self, String> {
        let (source, rate) = FixtureSource::for_mode(&config.mode)?;
        let epoch = allocate_dsp_epoch().map_err(str::to_owned)?;
        let count = (rate * 0.085) as usize;
        Ok(Self {
            entry: ChannelRegistryEntry { channel: config.channel, channel_id: epoch, epoch,
                frequency_hz: config.freq, output_rate: rate, input_rate: rate,
                group_delay_samples: 0.0, state: "active" },
            config, source, sequence: 0, input_index: 0, iq: Vec::with_capacity(count * 2),
        })
    }

    fn step(&mut self, frames: &mut Vec<Vec<u8>>) -> Result<(), String> {
        let entry = &self.entry;
        let magic = if entry.channel == 1 { BasebandMagic::Primary } else { BasebandMagic::Aux };
        let count = (entry.output_rate * 0.085) as usize;
        let next_index = self.input_index.checked_add(count as u64)
            .ok_or("fixture source sample clock exhausted")?;
        let sequence = self.sequence.checked_add(1).ok_or("fixture frame sequence exhausted")?;
        if self.sequence == 0 {
            frames.push(encode_baseband(magic, 0, entry.output_rate, entry.frequency_hz,
                entry.channel_id as u64, entry.epoch as u64, 0, 0.0, &[]).map_err(str::to_owned)?);
        }
        self.source.fill(&self.config, entry.output_rate, self.input_index, count, &mut self.iq);
        frames.push(encode_baseband(magic, sequence, entry.output_rate, entry.frequency_hz,
            entry.channel_id as u64, entry.epoch as u64, self.input_index, 0.0, &self.iq)
            .map_err(str::to_owned)?);
        self.sequence = sequence;
        self.input_index = next_index;
        Ok(())
    }
}

impl FixtureSources {
    pub(super) fn new() -> Self {
        Self { slots: BTreeMap::new(), registry: Arc::from([]) }
    }

    pub(super) fn configure(&mut self, configs: &[ChannelConfig]) -> Result<(), String> {
        let mut seen = [false; 3];
        let mut replacements = Vec::new();
        for config in configs {
            if !(1..=3).contains(&config.channel) || seen[(config.channel - 1) as usize] {
                return Err("fixture channel slots must be unique values in 1..=3".into());
            }
            seen[(config.channel - 1) as usize] = true;
            if !config.freq.is_finite() { return Err("fixture frequency must be finite".into()); }
            if config.enabled && self.slots.get(&config.channel).is_none_or(|slot|
                slot.config.freq != config.freq || slot.config.mode != config.mode
                    || slot.config.if_bw != config.if_bw || slot.config.pitch != config.pitch
                    || slot.config.audio_rate != config.audio_rate || slot.config.stereo != config.stereo
                    || slot.config.sstv_mode != config.sstv_mode
                    || slot.config.radiofax_acquisition != config.radiofax_acquisition || slot.config.radiofax_ioc != config.radiofax_ioc
                    || slot.config.radiofax_lpm != config.radiofax_lpm || slot.config.radiofax_modulation != config.radiofax_modulation
                    || slot.config.radiofax_polarity != config.radiofax_polarity || slot.config.radiofax_clock_recovery != config.radiofax_clock_recovery) {
                replacements.push((config.channel, FixtureSlot::new(config.clone())?));
            }
        }
        let changed = !replacements.is_empty() || self.slots.keys().any(|channel|
            !configs.iter().any(|config| config.channel == *channel && config.enabled));
        if !changed {
            for config in configs {
                if let Some(slot) = self.slots.get_mut(&config.channel) { slot.config = config.clone(); }
            }
            return Ok(());
        }
        // Load and validate every changed source before replacing any live slot.
        self.slots.retain(|channel, _| configs.iter().any(|config|
            config.channel == *channel && config.enabled));
        for (channel, slot) in replacements { self.slots.insert(channel, slot); }
        for config in configs {
            if let Some(slot) = self.slots.get_mut(&config.channel) { slot.config = config.clone(); }
        }
        self.refresh_registry();
        Ok(())
    }

    /// Explicit capture geometry changes restart selected fixture clocks.
    pub(super) fn restart(&mut self, channel: Option<u32>) -> Result<(), String> {
        if channel.is_some_and(|channel| !(1..=3).contains(&channel)) {
            return Err("fixture channel must be in 1..=3".into());
        }
        let mut identities = Vec::new();
        for (&slot, _) in &self.slots {
            if channel.is_none_or(|channel| channel == slot) {
                identities.push((slot, allocate_dsp_epoch().map_err(str::to_owned)?));
            }
        }
        if identities.is_empty() { return Ok(()); }
        for (channel, epoch) in identities {
            let slot = self.slots.get_mut(&channel).unwrap();
            slot.entry.channel_id = epoch;
            slot.entry.epoch = epoch;
            slot.sequence = 0;
            slot.input_index = 0;
            slot.iq.clear();
        }
        self.refresh_registry();
        Ok(())
    }

    pub(super) fn step(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let mut frames = Vec::with_capacity(self.slots.len() * 2);
        for slot in self.slots.values_mut() { slot.step(&mut frames)?; }
        Ok(frames)
    }

    pub(super) fn registry(&self) -> Arc<[ChannelRegistryEntry]> { self.registry.clone() }

    fn refresh_registry(&mut self) {
        self.registry = self.slots.values().map(|slot| slot.entry.clone()).collect::<Vec<_>>().into();
    }
}
