use super::*;
use super::receiver_workers::ChannelConfig;
use super::sdr_pipeline::ChannelRegistryEntry;
use std::f64::consts::TAU;
use std::sync::LazyLock;
use websa_channelizer::BasebandMagic;

pub(super) const PERIOD: Duration = Duration::from_millis(85);

/// A fixture is a sample-clock source. Scheduler lateness must not change its
/// long-term sample rate: retain every due block and catch up without skipping.
#[derive(Default)]
pub(super) struct Cadence {
    next: Option<(u64, tokio::time::Instant)>,
}
impl Cadence {
    pub(super) fn deadline(&mut self, epoch: u64, started: tokio::time::Instant) -> tokio::time::Instant {
        let next = self.next.filter(|(previous, _)| *previous == epoch)
            .map_or(started, |(_, deadline)| deadline) + PERIOD;
        self.next = Some((epoch, next));
        next
    }
    pub(super) fn reset(&mut self) { self.next = None; }
}

/// Independent native-receiver inputs for the fake device. These signals are
/// already in normalized ADC units, so IQv2 encoding must not scale them again.
#[derive(Clone)]
pub(super) struct FixtureSources {
    slots: BTreeMap<u32, FixtureSlot>,
    registry: Arc<[ChannelRegistryEntry]>,
}

#[derive(Clone)]
struct FixtureSlot {
    config: ChannelConfig,
    entry: ChannelRegistryEntry,
    source: FixtureSource,
    sequence: u32,
    input_index: u64,
    iq: Vec<f32>,
}

#[derive(Clone)]
enum FixtureSource {
    Iq,
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
        if matches!(mode, "ft4" | "wspr") {
            return Err("OnAir FT4/WSPR require UTC-aligned live IQ; looping fixture replay is unsupported".into());
        }
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
            mode if websa_channelizer::catalog::is_external(mode) => {
                let info = websa_channelizer::catalog::mode(mode).ok_or("external mode descriptor missing")?;
                let rate = info.input_rate_hz.ok_or("external fixture rate missing")? as f64;
                // External recordings are explicit inputs, never synthetic successful decodes.
                // Place normalized f32le IQ at this exact advertised rate under WEBSA_FIXTURES.
                (fixture(&format!("external/{mode}.f32"))?, rate, None)
            }
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
            Self::Iq => {
                let tone=config.baseband.as_ref().map_or(200.0,|b|b.passband_hz/8.0);
                for index in first..first+count as u64 {
                    let phase=TAU*tone*index as f64/rate;
                    out.extend_from_slice(&[(0.25*phase.cos()) as f32,(0.25*phase.sin()) as f32]);
                }
            }
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
        let (source, rate) = if !config.processing_enabled {
            (FixtureSource::Iq,service_ddc::channel_plan(COARSE_RATE,&config)?.output_rate)
        } else {FixtureSource::for_mode(&config.mode)?};
        if let Some(baseband)=config.baseband.as_ref().filter(|_|config.processing_enabled) {
            if rate<baseband.minimum_rate_hz || rate<baseband.passband_hz+2.0*baseband.transition_hz {
                return Err(format!("fixture replay rate {rate} S/s cannot satisfy the explicit baseband passband, transition and minimum rate"));
            }
        }
        let epoch = allocate_dsp_epoch().map_err(str::to_owned)?;
        let count = (rate * PERIOD.as_secs_f64()) as usize;
        Ok(Self {
            entry: ChannelRegistryEntry { channel: config.channel, channel_id: epoch, epoch,
                frequency_hz: config.freq, output_rate: rate, input_rate: rate,
                group_delay_samples: 0.0, state: "active" },
            config, source, sequence: 0, input_index: 0, iq: Vec::with_capacity(count * 2),
        })
    }

    fn step(&mut self, frames: &mut IqFrames) -> Result<(), String> {
        let entry = &self.entry;
        let magic = if entry.channel == 1 { BasebandMagic::Primary } else { BasebandMagic::Aux };
        let count = (entry.output_rate * PERIOD.as_secs_f64()) as usize;
        let next_index = self.input_index.checked_add(count as u64)
            .ok_or("fixture source sample clock exhausted")?;
        let sequence = self.sequence.checked_add(1).ok_or("fixture frame sequence exhausted")?;
        if self.sequence == 0 {
            frames.push(IqFrame::native(magic,0,entry.output_rate,entry.frequency_hz,entry.channel_id as u64,entry.epoch as u64,0,0.0,entry.input_rate,Vec::new()).map_err(str::to_owned)?);
        }
        self.source.fill(&self.config, entry.output_rate, self.input_index, count, &mut self.iq);
        frames.push(IqFrame::native(magic,sequence,entry.output_rate,entry.frequency_hz,entry.channel_id as u64,entry.epoch as u64,
            self.input_index,0.0,entry.input_rate,std::mem::take(&mut self.iq)).map_err(str::to_owned)?);
        self.sequence = sequence;
        self.input_index = next_index;
        Ok(())
    }
}

impl FixtureSources {
    pub(super) fn new() -> Self {
        Self { slots: BTreeMap::new(), registry: Arc::from([]) }
    }

    /// Build the complete replacement before channel controls retire any live identity.
    pub(super) fn staged(&self, configs: &[ChannelConfig]) -> Result<Self, String> {
        let mut staged = self.clone();
        staged.configure(configs)?;
        Ok(staged)
    }
    pub(super) fn configure(&mut self, configs: &[ChannelConfig]) -> Result<(), String> {
        let mut seen = vec![false; MAX_CHANNELS];
        let mut replacements = Vec::new();
        for config in configs {
            if !(1..=MAX_CHANNELS as u32).contains(&config.channel) || seen[(config.channel - 1) as usize] {
                return Err("fixture channel slots must be unique values within service capacity".into());
            }
            seen[(config.channel - 1) as usize] = true;
            if !config.freq.is_finite() { return Err("fixture frequency must be finite".into()); }
            if config.enabled && self.slots.get(&config.channel).is_none_or(|slot|
                slot.config.freq != config.freq || slot.config.mode != config.mode
                    || slot.config.baseband != config.baseband || slot.config.processing_enabled != config.processing_enabled
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
        if channel.is_some_and(|channel| !(1..=MAX_CHANNELS as u32).contains(&channel)) {
            return Err("fixture channel exceeds service capacity".into());
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

    pub(super) fn step(&mut self) -> Result<IqFrames, String> {
        let mut frames = Vec::with_capacity(self.slots.len() * 2);
        for slot in self.slots.values_mut() { slot.step(&mut frames)?; }
        Ok(frames)
    }

    pub(super) fn registry(&self) -> Arc<[ChannelRegistryEntry]> { self.registry.clone() }

    fn refresh_registry(&mut self) {
        self.registry = self.slots.values().map(|slot| slot.entry.clone()).collect::<Vec<_>>().into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_deadlines_do_not_accumulate_lateness_or_cross_epochs() {
        let mut cadence = Cadence::default();
        let start = tokio::time::Instant::now();
        let mut actual = start;
        // Relative 85 ms sleeps would turn this into 87 ms/sample block.
        for block in 1..=1000 {
            let deadline = cadence.deadline(1, actual);
            assert_eq!(deadline, start + PERIOD * block);
            actual = deadline + Duration::from_millis(2);
        }
        actual += Duration::from_secs(3);
        assert_eq!(cadence.deadline(1, actual), start + PERIOD * 1001);
        assert_eq!(cadence.deadline(2, actual), actual + PERIOD);
        cadence.reset();
        actual += Duration::from_secs(1);
        assert_eq!(cadence.deadline(2, actual), actual + PERIOD);
    }
    #[test]
    fn failed_fixture_staging_preserves_live_identity_and_samples() {
        let mut source = FixtureSources::new();
        let mut config = ChannelConfig::for_slot(1);
        config.freq = 100_000_000.0;
        source.configure(&[config.clone()]).unwrap();
        source.step().unwrap();
        let mut reference = source.clone();
        let registry = source.registry();
        config.baseband=Some(receiver_workers::BasebandConfig {passband_hz:16000.0,transition_hz:1000.0,minimum_rate_hz:96000.0});
        assert!(source.staged(&[config.clone()]).err().unwrap().contains("fixture replay rate"));
        assert_eq!(source.registry()[0].epoch,registry[0].epoch);
        config.mode = "ft4".into();
        assert!(source.staged(&[config]).err().unwrap().contains("UTC-aligned"));
        assert_eq!(source.registry()[0].epoch, registry[0].epoch);
        assert_eq!(source.step().unwrap(), reference.step().unwrap());
    }
}
