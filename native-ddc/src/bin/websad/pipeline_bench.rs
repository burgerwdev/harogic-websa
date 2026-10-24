//! Opt-in hardware validation tool, invoked after OpenMP setup with
//! `--sdr-pipeline-bench --channels N --seconds S --taps 4|8|16
//! --display-decimate D --fine-decimate F --shim PATH --center HZ --output PATH`.
//! `--product-channels true` selects the actual WFM 180 kHz, AM 6 kHz,
//! and CW 500 Hz service channels and native receiver workers at full capture bandwidth.
//! `--warmup S` defaults to five seconds. N is a requested benchmark workload,
//! never a product admission limit. JSON contains separate warmup, measurement,
//! and shutdown boundaries; payloads are inspected and immediately released.
use super::sdr_pipeline::{ChannelRegistryEntry, Pipeline, PipelineConfig, PipelineOutput, Snapshot};
use super::receiver_workers::{ChannelConfig, ReceiverFrames, ReceiverWorkers};
use super::{capture_decimate, hardware_pan, FrontEnd, BIN_SPACING, COARSE_RATE, IQ_FS};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};
use websa_channelizer::{coarse_passband_hz, timing::Timing};

struct Options {
    channels: usize,
    seconds: f64,
    warmup: f64,
    taps: usize,
    display: i32,
    fine: i32,
    product_channels: bool,
    shim: String,
    center: f64,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut fields = BTreeMap::new();
        let mut index = usize::from(args.first().is_some_and(|s| s == "--sdr-pipeline-bench"));
        while index < args.len() {
            let key = args[index].as_str();
            if !matches!(key, "--channels" | "--seconds" | "--warmup" | "--taps" |
                "--display-decimate" | "--fine-decimate" | "--shim" | "--center" | "--output" |
                "--product-channels") {
                return Err(format!("unknown benchmark option {key}"));
            }
            let value = args.get(index + 1).ok_or_else(|| format!("missing value for {key}"))?;
            if fields.insert(key, value.as_str()).is_some() {
                return Err(format!("duplicate benchmark option {key}"));
            }
            index += 2;
        }
        let required = |key| fields.get(key).copied().ok_or_else(|| format!("required option {key}"));
        let integer = |key| required(key)?.parse::<u64>().map_err(|_| format!("invalid integer {key}"));
        let number = |key| required(key)?.parse::<f64>().map_err(|_| format!("invalid number {key}"));
        let product_channels = match fields.get("--product-channels").copied().unwrap_or("false") {
            "true" => true,
            "false" => false,
            _ => return Err("--product-channels must be true or false".into()),
        };
        let channels = if product_channels && !fields.contains_key("--channels") { 3 }
            else { integer("--channels")? };
        if product_channels && channels != 3 {
            return Err("--product-channels true requires exactly three channels".into());
        }
        if channels == 0 || channels > u32::MAX as u64 || channels > usize::MAX as u64 {
            return Err("channels must fit nonzero u32 protocol channel IDs".into());
        }
        let seconds = number("--seconds")?;
        let warmup = fields.get("--warmup").unwrap_or(&"5").parse::<f64>()
            .map_err(|_| "invalid --warmup")?;
        if !seconds.is_finite() || seconds <= 0.0 || !warmup.is_finite() || warmup < 0.0
            || Duration::try_from_secs_f64(seconds + warmup).is_err() {
            return Err("seconds must be finite and positive; warmup finite and nonnegative".into());
        }
        let taps = if product_channels && !fields.contains_key("--taps") { 16 }
            else { integer("--taps")? };
        if !matches!(taps, 4 | 8 | 16) { return Err("taps must be 4, 8, or 16".into()); }
        let display = if product_channels && !fields.contains_key("--display-decimate") { 1 }
            else { integer("--display-decimate")? };
        if !display.is_power_of_two() || display > 2048 {
            return Err("display decimation must be a power of two in 1..=2048".into());
        }
        if product_channels && (display != 1 || taps != 16) {
            return Err("--product-channels true requires full-band --display-decimate 1 --taps 16".into());
        }
        let fine = if product_channels && !fields.contains_key("--fine-decimate") { 4 }
            else { integer("--fine-decimate")? };
        // Native narrow::geometry at COARSE_RATE has d1=1 and supports d2=2..24.
        if !(2..=24).contains(&fine) { return Err("fine decimation must be in 2..=24".into()); }
        let center = number("--center")?;
        if !center.is_finite() { return Err("center must be finite".into()); }
        if required("--output")?.is_empty() { return Err("output path must not be empty".into()); }
        let shim = required("--shim")?.to_owned();
        if shim.is_empty() { return Err("shim path must not be empty".into()); }
        Ok(Self { channels: channels as usize, seconds, warmup, taps: taps as usize,
            display: display as i32, fine: fine as i32, product_channels, shim, center })
    }

    fn frequencies(&self) -> Result<Vec<f64>, String> {
        if self.product_channels {
            return Ok(vec![self.center, self.center + 5_000.0, self.center - 5_000.0]);
        }
        let flat = coarse_passband_hz(COARSE_RATE, self.taps).ok_or("unsupported taps")?;
        let rate = COARSE_RATE / self.fine as f64;
        let device = capture_decimate(self.center, self.center, &[], self.display, self.taps);
        let half = IQ_FS * 0.8 / device as f64 / 2.0;
        // All channels stay inside the capture selected for the display. Reuse
        // bins with distinct integer-Hz offsets when N exceeds the bin count.
        // The one-Hz lattice is this fixture's choice, not a product limit.
        let mut bins = Vec::new();
        for magnitude in 0..=64 {
            for sign in [1, -1] {
                if magnitude == 0 && sign == -1 { continue; }
                let bin = magnitude * sign;
                let margin = (flat - rate / 2.0).min(BIN_SPACING / 2.0 - 1.0)
                    .min(half - (bin as f64 * BIN_SPACING).abs() - flat).floor();
                if margin >= 0.0 { bins.push((bin, margin as usize)); }
            }
        }
        let capacity: u64 = bins.iter().map(|(_, margin)| 2 * *margin as u64 + 1).sum();
        if self.channels as u64 > capacity {
            return Err(format!("requested {} distinct channels exceed this capture/passband geometry ({capacity} one-Hz slots)", self.channels));
        }
        let mut result = Vec::new();
        result.try_reserve_exact(self.channels).map_err(|e| format!("channel allocation: {e}"))?;
        let mut seen = HashMap::new();
        seen.try_reserve(self.channels).map_err(|e| format!("frequency allocation: {e}"))?;
        for level in 0.. {
            let offset = if level % 2 == 0 { -(level as f64 / 2.0) } else { (level + 1) as f64 / 2.0 };
            for &(bin, margin) in &bins {
                if offset.abs() > margin as f64 { continue; }
                let frequency = self.center + bin as f64 * BIN_SPACING + offset;
                if !frequency.is_finite() || seen.insert(frequency.to_bits(), ()).is_some() {
                    return Err("center cannot represent distinct legal channel frequencies".into());
                }
                super::validate_channel_passband(self.center, frequency, self.fine, self.taps)?;
                result.push(frequency);
                if result.len() == self.channels { return Ok(result); }
            }
        }
        unreachable!()
    }
}

#[derive(Clone, Default)]
struct Channel {
    channel_id: u32,
    epoch: u32,
    group_delay: f64,
    hop: u64,
    samples: u64,
    frames: u64,
    gaps: u64,
    next_sequence: u64,
    first_index: Option<u64>,
    last_index: Option<u64>,
    last_count: u64,
}

#[derive(Clone)]
struct Registered {
    slot: usize,
    channel_id: u32,
    epoch: u32,
    frequency: f64,
    output_rate: f64,
    input_rate: f64,
    group_delay: f64,
}

fn registry(actual: &Value, frequencies: &[f64]) -> Result<Vec<Registered>, String> {
    let rows = actual.get("channel_registry").and_then(Value::as_array)
        .ok_or("channel_registry unavailable")?;
    if rows.len() != frequencies.len() {
        return Err(format!("registry has {} channels, expected {}", rows.len(), frequencies.len()));
    }
    let mut result: Vec<Option<Registered>> = vec![None; frequencies.len()];
    let mut keys = HashMap::new();
    for row in rows {
        let slot = row["channel"].as_u64().ok_or("registry channel slot missing")? as usize;
        let channel_id = row["channel_id"].as_u64().ok_or("registry channel_id missing")? as u32;
        let epoch = row["epoch"].as_u64().ok_or("registry epoch missing")? as u32;
        let frequency = row["frequency_hz"].as_f64().ok_or("registry frequency missing")?;
        let output_rate = row["output_rate"].as_f64().ok_or("registry output rate missing")?;
        let input_rate = row["input_rate"].as_f64().ok_or("registry input rate missing")?;
        let group_delay = row["group_delay_samples"].as_f64().ok_or("registry delay missing")?;
        if row["state"].as_str() != Some("active") || channel_id == 0
            || slot == 0 || slot > frequencies.len()
            || !frequency.is_finite() || !output_rate.is_finite() || !input_rate.is_finite()
            || !group_delay.is_finite() || output_rate <= 0.0 || group_delay < 0.0
            || frequency.to_bits() != frequencies[slot - 1].to_bits()
            || keys.insert((epoch, channel_id), slot).is_some() {
            return Err(format!("invalid registry identity or metadata for slot {slot}"));
        }
        let hop = input_rate / output_rate;
        if !hop.is_finite() || hop < 1.0 || (hop - hop.round()).abs() > 1e-9 * hop {
            return Err(format!("registry slot {slot} input/output rate ratio is not integral"));
        }
        result[slot - 1] = Some(Registered { slot, channel_id, epoch, frequency,
            output_rate, input_rate, group_delay });
    }
    result.into_iter().map(|entry| entry.ok_or_else(|| "registry slot missing".into())).collect()
}

#[derive(Clone, Default)]
struct AudioClock {
    next_sequence: u64,
    frames: u64,
    samples: u64,
    rate: f64,
    channels: u32,
    first_source_index: Option<u64>,
    previous_position: Option<(u64, u64, u64, u64)>,
    next_decode_sequence: u64,
    decode_frames: u64,
}

struct ProductReceiver {
    workers: ReceiverWorkers,
    configs: Vec<ChannelConfig>,
    registry: std::sync::Arc<[ChannelRegistryEntry]>,
    clocks: Vec<AudioClock>,
    input_rate_numerator: u64,
    input_rate_denominator: u64,
}

impl ProductReceiver {
    fn new(configs: &[ChannelConfig], snapshot: &Snapshot, workers: ReceiverWorkers) -> Self {
        Self { workers, configs:configs.to_vec(), registry:snapshot.registry.clone(),
            clocks:vec![AudioClock::default(); configs.len()], input_rate_numerator:IQ_FS as u64,
            input_rate_denominator:snapshot.device_decimate }
    }

    fn submit(&mut self, frame: &[u8]) -> Result<(), String> {
        self.workers.submit(frame, &self.registry)?;
        self.poll()
    }

    fn poll(&mut self) -> Result<(), String> {
        let frames = self.workers.poll()?;
        self.consume(frames)
    }

    fn finish(&mut self) -> Result<(), String> {
        let frames = self.workers.finish()?;
        self.consume(frames)
    }

    fn consume(&mut self, frames: ReceiverFrames) -> Result<(), String> {
        for frame in frames.audio {
            if frame.len() < 96 || &frame[..4] != b"AUD2" { return Err("invalid AUD2 receiver frame".into()); }
            let u32_at = |i| u32::from_le_bytes(frame[i..i + 4].try_into().unwrap());
            let u64_at = |i| u64::from_le_bytes(frame[i..i + 8].try_into().unwrap());
            let f64_at = |i| f64::from_le_bytes(frame[i..i + 8].try_into().unwrap());
            let (id, epoch) = (u32_at(32), u32_at(36));
            let index = self.registry.iter().position(|row| row.channel_id == id && row.epoch == epoch)
                .ok_or_else(|| format!("unexpected audio identity {epoch}:{id}"))?;
            let row = &self.registry[index];
            let config = &self.configs[index];
            let clock = &mut self.clocks[index];
            let (sequence, count, rate, channels) = (u32_at(8), u32_at(12), f64_at(16), u32_at(56));
            let (source, audio_index, numerator, denominator) = (u64_at(40), u64_at(64), u64_at(80), u64_at(88));
            if u32_at(4) != 1 || sequence as u64 != clock.next_sequence
                || (sequence == 0) != (count == 0) || audio_index != clock.samples
                || rate != config.audio_rate as f64 || !rate.is_finite() || rate <= 0.0
                || !matches!(channels, 1 | 2) || u32_at(60) & 0xffff != 1
                || u32_at(60) & !0x0003_ffff != 0
                || frame.len() as u64 != 96 + count as u64 * channels as u64 * 4
                || f64_at(24) != row.frequency_hz || f64_at(72) != row.input_rate
                || !f64_at(48).is_finite() || f64_at(48) < 0.0
                || denominator == 0 || numerator >= denominator
                || (clock.frames > 0 && (clock.rate != rate || clock.channels != channels))
                || !frame[96..].chunks_exact(4).all(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()).is_finite()) {
                return Err(format!("AUD2 identity/sequence/count/rate/position failure for channel {}", row.channel));
            }
            if let Some((previous, previous_num, previous_den, previous_count)) = clock.previous_position {
                let gcd = |mut a: u64, mut b: u64| {
                    while b != 0 { (a, b) = (b, a % b); }
                    a
                };
                let divisor = gcd(denominator, previous_den);
                let common = denominator as u128 * (previous_den / divisor) as u128;
                let current = source as u128 * common + numerator as u128 * (previous_den / divisor) as u128;
                let prior = previous as u128 * common + previous_num as u128 * (denominator / divisor) as u128;
                let scale = self.input_rate_denominator as u128 * rate as u128;
                if current < prior || (current - prior).checked_mul(scale)
                    != (previous_count as u128 * self.input_rate_numerator as u128).checked_mul(common) {
                    return Err(format!("AUD2 exact causal source clock failure for channel {}", row.channel));
                }
            } else { clock.first_source_index = Some(source); }
            clock.previous_position = Some((source, numerator, denominator, count as u64));
            clock.next_sequence += 1;
            clock.frames += 1;
            clock.samples += count as u64;
            clock.rate = rate;
            clock.channels = channels;
        }
        for frame in frames.decode {
            if frame.len() < 8 || &frame[..4] != b"SDE2"
                || u32::from_le_bytes(frame[4..8].try_into().unwrap()) as usize != frame.len() - 8 {
                return Err("invalid SDE2 receiver frame".into());
            }
            let event: Value = serde_json::from_slice(&frame[8..]).map_err(|e| format!("SDE2 JSON: {e}"))?;
            let id = event["channel_id"].as_u64().ok_or("SDE2 identity missing")?;
            let epoch = event["epoch"].as_u64().ok_or("SDE2 epoch missing")?;
            let index = self.registry.iter().position(|row| u64::from(row.channel_id) == id && u64::from(row.epoch) == epoch)
                .ok_or_else(|| format!("unexpected decode identity {epoch}:{id}"))?;
            let clock = &mut self.clocks[index];
            let position = |key: &str| event[key].as_str().and_then(|text| text.parse::<u64>().ok());
            let num = position("input_fraction_num").ok_or("SDE2 fraction numerator missing")?;
            let den = position("input_fraction_den").ok_or("SDE2 fraction denominator missing")?;
            if event["version"].as_u64() != Some(1)
                || event["channel"].as_u64() != Some(self.registry[index].channel as u64)
                || event["seq"].as_u64() != Some(clock.next_decode_sequence)
                || event["mode"].as_str() != Some(self.configs[index].mode.as_str())
                || event["input_rate"].as_f64() != Some(self.registry[index].input_rate)
                || position("input_index").is_none() || den == 0 || num >= den
                || !event["delay_seconds"].as_f64().is_some_and(|delay| delay.is_finite() && delay >= 0.0)
                || (clock.next_decode_sequence == 0 && event["event"]["type"].as_str() != Some("start")) {
                return Err(format!("SDE2 identity/sequence/source clock failure for channel {}", index + 1));
            }
            if event["event"]["type"].as_str() == Some("error") {
                return Err(format!("native receiver error for channel {}: {}", index + 1, event["event"]));
            }
            clock.next_decode_sequence += 1;
            clock.decode_frames += 1;
        }
        Ok(())
    }

    fn proof(&self, iq: &[Channel], errors: &mut Vec<String>) -> Value {
        let stats = self.workers.stats();
        let audio_frames: u64 = self.clocks.iter().map(|clock| clock.frames).sum();
        let audio_samples: u64 = self.clocks.iter().map(|clock| clock.samples).sum();
        let decode_frames: u64 = self.clocks.iter().map(|clock| clock.decode_frames).sum();
        let iq_samples: u64 = iq.iter().map(|channel| channel.samples).sum();
        let iq_frames: u64 = iq.iter().map(|channel| channel.frames).sum();
        let counters_match = stats["queue_full"].as_u64() == Some(0)
            && stats["pending"].as_u64() == Some(0)
            && stats["submitted"].as_u64() == Some(iq_frames)
            && stats["completed"].as_u64() == Some(iq_frames)
            && stats["submitted_samples"].as_u64() == Some(iq_samples)
            && stats["completed_samples"].as_u64() == Some(iq_samples)
            && stats["audio_frames"].as_u64() == Some(audio_frames)
            && stats["audio_samples"].as_u64() == Some(audio_samples)
            && stats["decode_frames"].as_u64() == Some(decode_frames);
        if !counters_match { errors.push("receiver worker queue or input/output conservation failed".into()); }
        let channels: Vec<_> = self.clocks.iter().enumerate().map(|(i, clock)| {
            if clock.samples == 0 { errors.push(format!("channel {} produced no native audio", i + 1)); }
            // The analog interpolator retains the final source interval. FIRs
            // and NR-off preserve length; floating phase recurrence may move
            // an output exactly at an interval boundary by one audio frame.
            let expected_audio = iq[i].samples.checked_sub(1).map(|intervals|
                (intervals as u128 * iq[i].hop as u128 * self.input_rate_denominator as u128
                    * self.configs[i].audio_rate as u128 / self.input_rate_numerator as u128 + 1) as u64);
            let audio_matches = expected_audio.is_some_and(|expected| clock.samples.abs_diff(expected) <= 1)
                && clock.samples > 0 && clock.first_source_index == iq[i].first_index;
            if !audio_matches {
                errors.push(format!("channel {} IQ/audio sample conservation failed: {} audio frames, expected {expected_audio:?} with one-frame phase tolerance", i + 1, clock.samples));
            }
            json!({"channel":i + 1,"channel_id":self.registry[i].channel_id,"epoch":self.registry[i].epoch,
                "mode":self.configs[i].mode,"if_bandwidth_hz":self.configs[i].if_bw,
                "selected_iq_rate_hz":self.registry[i].output_rate,"iq_samples":iq[i].samples,
                "audio_rate_hz":clock.rate,"audio_channels":clock.channels,"audio_samples":clock.samples,
                "expected_audio_samples":expected_audio,"audio_sample_tolerance":1,"audio_samples_match":audio_matches,
                "audio_frames":clock.frames,"last_audio_sequence":clock.next_sequence.checked_sub(1),
                "first_source_input_index":clock.first_source_index,"last_source_position":clock.previous_position,
                "decode_frames":clock.decode_frames,"last_decode_sequence":clock.next_decode_sequence.checked_sub(1)})
        }).collect();
        let channels_match = channels.iter().all(|channel| channel["audio_samples_match"].as_bool() == Some(true));
        json!({"workers":stats,"conservation_passed":counters_match && channels_match,"channels":channels,
            "source_clock_proof":"Every AUD2 source position advances by the prior frame count times the exact capture-rate/audio-rate ratio; audio_index and sequence are contiguous from zero.",
            "audio_count_scope":"NR-off analog interpolation retains the final input interval; expected PCM count is floor((IQ samples-1)*audio rate/selected IQ rate)+1 with at most one audio frame of floating phase rounding.",
            "timing_scope":"native receiver work as exposed by worker stats; no legacy fine profile certifies receiver DSP"})
    }
}

struct Consumer {
    channels: Vec<Channel>,
    by_key: HashMap<(u32, u32), usize>,
    registry: Vec<Registered>,
    center: f64,
    bandwidth: f64,
    spectra: u64,
    spectrum_bytes: u64,
    encode: Timing,
    receiver: Option<ProductReceiver>,
}

impl Consumer {
    fn new(frequencies: &[f64], snapshot: &Snapshot, display: i32) -> Result<Self, String> {
        let registry = registry(&snapshot.actual, frequencies)?;
        let mut channels = vec![Channel::default(); frequencies.len()];
        let mut by_key = HashMap::new();
        for entry in &registry {
            by_key.insert((entry.epoch, entry.channel_id), entry.slot - 1);
            channels[entry.slot - 1] = Channel { channel_id: entry.channel_id, epoch: entry.epoch,
                group_delay: entry.group_delay, hop: (entry.input_rate / entry.output_rate).round() as u64,
                ..Channel::default() };
        }
        Ok(Self { channels, by_key, registry, center: snapshot.capture_center,
            bandwidth: (IQ_FS * 0.8 / display as f64).min(snapshot.usable_bandwidth),
            spectra: 0, spectrum_bytes: 0, encode: Timing::default(), receiver:None })
    }

    fn verify_registry(&self, snapshot: &Snapshot) -> Result<(), String> {
        let rows = snapshot.actual.get("channel_registry").and_then(Value::as_array)
            .ok_or("stopped channel_registry unavailable")?;
        if rows.len() != self.registry.len() {
            return Err("registry identity count changed during a stable capture".into());
        }
        for entry in &self.registry {
            let row = rows.iter().find(|row| row["channel"].as_u64() == Some(entry.slot as u64))
                .ok_or_else(|| format!("registry slot {} disappeared", entry.slot))?;
            if row["channel_id"].as_u64() != Some(entry.channel_id as u64)
                || row["epoch"].as_u64() != Some(entry.epoch as u64) {
                return Err(format!("registry slot {} identity changed during a stable capture", entry.slot));
            }
        }
        Ok(())
    }

    fn consume(&mut self, output: PipelineOutput) -> Result<(), String> {
        for frame in output.frames {
            if frame.len() < 56 { return Err("short IQ v2 frame header".into()); }
            let u32_at = |i| u32::from_le_bytes(frame[i..i + 4].try_into().unwrap());
            let u64_at = |i| u64::from_le_bytes(frame[i..i + 8].try_into().unwrap());
            let f64_at = |i| f64::from_le_bytes(frame[i..i + 8].try_into().unwrap());
            let (version, sequence, count, rate, frequency) =
                (u32_at(4), u32_at(8), u32_at(12), f64_at(16), f64_at(24));
            let (channel_id, epoch, input_index, delay) =
                (u32_at(32), u32_at(36), u64_at(40), f64_at(48));
            let index = *self.by_key.get(&(epoch, channel_id))
                .ok_or_else(|| format!("unexpected IQ identity {epoch}:{channel_id}"))?;
            let registered = &self.registry[index];
            let expected_magic: &[u8] = if index == 0 { b"IQBF" } else { b"IQAX" };
            if &frame[..4] != expected_magic || version != 2
                || (frame.len() - 56) as u64 != count as u64 * 8
                || !rate.is_finite() || rate != registered.output_rate
                || frequency != registered.frequency || delay != registered.group_delay
                || input_index == u64::MAX {
                return Err(format!("invalid IQ v2 header/count/rate/metadata for channel {}", index + 1));
            }
            let channel = &mut self.channels[index];
            if sequence as u64 != channel.next_sequence {
                channel.gaps += 1;
                return Err(format!("channel {} sequence {}, expected {}", index + 1, sequence, channel.next_sequence));
            }
            if (sequence == 0) != (count == 0) {
                return Err(format!("invalid IQ start/empty frame for channel {}", index + 1));
            }
            if !frame[56..].chunks_exact(4).all(|bytes|
                f32::from_le_bytes(bytes.try_into().unwrap()).is_finite()) {
                return Err(format!("nonfinite IQ data for channel {}", index + 1));
            }
            if count > 0 {
                if let Some(previous) = channel.last_index {
                    let expected = channel.last_count.checked_mul(channel.hop)
                        .and_then(|step| previous.checked_add(step));
                    if expected != Some(input_index) {
                        return Err(format!("channel {} causal input index {input_index} differs from expected {expected:?} (prior {previous}, count {}, stride {})",
                            index + 1, channel.last_count, channel.hop));
                    }
                } else {
                    channel.first_index = Some(input_index);
                }
                channel.last_index = Some(input_index);
                channel.last_count = count as u64;
            }
            channel.next_sequence += 1;
            channel.frames += 1;
            channel.samples += count as u64;
            if let Some(receiver) = &mut self.receiver { receiver.submit(&frame)?; }
        }
        for (frequencies, powers, _warning, _generation, _seq) in output.pan {
            if !frequencies.iter().all(|f| f.is_finite()) || !powers.iter().all(|p| p.is_finite()) {
                return Err("nonfinite SDK spectrum".into());
            }
            let started = Instant::now();
            let encoded = hardware_pan(1, self.center, self.bandwidth, &frequencies, &powers);
            self.encode.observe(started.elapsed());
            let encoded = encoded?;
            self.spectrum_bytes += encoded.len() as u64;
            self.spectra += 1;
        }
        if let Some(receiver) = &mut self.receiver { receiver.poll()?; }
        Ok(())
    }
}

fn snapshot_json(snapshot: &Snapshot) -> Value {
    json!({ "capture_center_hz":snapshot.capture_center, "usable_bandwidth_hz":snapshot.usable_bandwidth,
        "output_rate_hz":snapshot.rate, "taps":snapshot.taps, "device_decimate":snapshot.device_decimate,
        "aux":&*snapshot.aux, "actual":snapshot.actual })
}

// Only additive counters have meaningful deltas. Quantiles and maxima remain
// full cumulative snapshots from this fresh capture; never subtract p99 values.
fn counter_deltas(before: &Value, after: &Value) -> Value {
    let mut result = serde_json::Map::new();
    if let (Some(a), Some(b)) = (before.as_object(), after.as_object()) {
        for (key, value) in b {
            if value.is_object() {
                result.insert(key.clone(), counter_deltas(a.get(key).unwrap_or(&Value::Null), value));
            } else if matches!(key.as_str(), "input_samples" | "iqs_stamp_gaps" | "packets" |
                "host_seconds" | "device_seconds" | "queue_full" | "queue_wait_ms" |
                "spectrum_frames" | "fail_calls" | "skips" | "recoveries" | "pfb_submitted" |
                "pfb_completed" | "pool_clone_ns" | "pool_history_ns" | "pool_task_wait_ns" |
                "pool_result_wait_ns" | "count" | "total_ns") {
                if let (Some(start), Some(end)) = (a.get(key).and_then(Value::as_u64), value.as_u64()) {
                    result.insert(key.clone(), json!({"delta":end.checked_sub(start), "reset":end < start}));
                } else if let (Some(start), Some(end)) = (a.get(key).and_then(Value::as_f64), value.as_f64()) {
                    result.insert(key.clone(), json!(end - start));
                }
            }
        }
    }
    Value::Object(result)
}

fn check_counters(before: &Value, after: &Value, errors: &mut Vec<String>) {
    for path in ["/iqs_stamp_gaps", "/iqs_capture/queue_full", "/iqs_capture/fail_calls", "/iqs_capture/recoveries"] {
        match (before.pointer(path).and_then(Value::as_u64), after.pointer(path).and_then(Value::as_u64)) {
            (Some(start), Some(end)) if end == start => (),
            (Some(start), Some(end)) => errors.push(format!("{path} changed {start} -> {end}")),
            _ => errors.push(format!("required capture counter missing: {path}")),
        }
    }
}

fn channel_rows(frequencies: &[f64], current: &[Channel], baseline: &[Channel], registry: &[Registered]) -> Vec<Value> {
    current.iter().zip(baseline).enumerate().map(|(i, (now, before))| json!({
        "channel":i + 1, "channel_id":now.channel_id, "epoch":now.epoch,
        "frequency_hz":frequencies[i], "group_delay_samples":now.group_delay,
        "selected_iq_rate_hz":registry[i].output_rate, "capture_rate_hz":registry[i].input_rate,
        "input_hop":now.hop, "first_input_index":now.first_index, "last_input_index":now.last_index,
        "samples":now.samples,
        "frames":now.frames, "gaps":now.gaps, "last_sequence":now.next_sequence.checked_sub(1),
        "window_samples":now.samples - before.samples, "window_frames":now.frames - before.frames,
        "window_gaps":now.gaps - before.gaps, "wire_seconds":now.samples as f64 / registry[i].output_rate,
    })).collect()
}

fn execute(options: &Options) -> Value {
    let frequencies = match options.frequencies() {
        Ok(value) => value,
        Err(error) => return json!({"passed":false,"errors":[error]}),
    };
    let rate = COARSE_RATE / options.fine as f64;
    let requested = json!({"channels":options.channels,"seconds":options.seconds,"warmup_seconds":options.warmup,
        "taps":options.taps,"display_decimate":options.display,
        "fine_decimate":if options.product_channels { None } else { Some(options.fine) },
        "center_hz":options.center,"shim":options.shim,"channel_budget":options.channels,
        "product_channels":options.product_channels,
        "output_rate_budget_hz":if options.product_channels { None } else { Some(rate * options.channels as f64) }});
    let admission = if options.product_channels {
        json!({"scope":"service planned fine kernel and framing only; excludes receiver DSP, PFB and transport",
            "performed_by":"Pipeline::start_service","legacy_fine_profile_applied":false,
            "receiver_cost_evidence":"actual worker native_chain runtime counters"})
    } else {
        let profile = match super::sdr_pipeline::fine_cost_profile() {
            Ok(profile) => profile,
            Err(error) => return json!({"passed":false,"requested":requested,"errors":[error]}),
        };
        let decision = super::sdr_pipeline::check_plan_admission(&profile, options.fine, options.channels,
            options.channels, rate * options.channels as f64);
        let admission = json!({"accepted":decision.is_ok(),"scope":"internal fine processing and framing only; excludes receiver DSP",
            "calibration_ns_per_coarse_sample":profile.ns_per_coarse_sample,
            "calibration_decimation_first":2,"calibration_decimation_last":24,
            "required_ns_per_second":profile.required_ns_per_second(COARSE_RATE,
                std::iter::repeat_n(options.fine, options.channels)).ok(),
            "limit_ns_per_second":profile.limit_ns_per_second,"reserve":profile.reserve});
        if let Err(error) = decision {
            return json!({"schema":"websa-sdr-pipeline-bench-v1","passed":false,
                "requested":requested,"admission":admission,"errors":[error]});
        }
        admission
    };
    let product_configs: Vec<_> = if options.product_channels {
        [("wfm", 180_000.0), ("am", 6_000.0), ("cw", 500.0)].into_iter().enumerate().map(|(i, (mode, bandwidth))| {
            let mut config = ChannelConfig::for_slot((i + 1) as u32);
            config.enabled = true;
            config.freq = frequencies[i];
            config.mode = mode.into();
            config.if_bw = bandwidth;
            config.squelch = -200.0;
            config
        }).collect()
    } else { Vec::new() };
    let config = PipelineConfig { center:options.center, listen:frequencies[0], fine:options.fine,
        display_decimate:options.display, taps:options.taps, shim:options.shim.clone(),
        front_end:FrontEnd { ref_dbm:0.0, atten:-1, preamp:0, ifgain:2, gain_strategy:0, ref_clock:0, refclk_out:0 },
        aux:frequencies.iter().enumerate().skip(1).map(|(i, f)| ((i + 1) as u32, *f)).collect(),
        channel_budget:options.channels, output_rate_budget:rate * options.channels as f64 };
    // Prepare receiver models before capture starts producing IQ.
    let prepared_workers = if options.product_channels {
        let mut workers = match ReceiverWorkers::new() {
            Ok(workers) => workers,
            Err(error) => return json!({"passed":false,"requested":requested,"admission":admission,"errors":[error]}),
        };
        if let Err(error) = workers.configure(&product_configs) {
            return json!({"passed":false,"requested":requested,"admission":admission,"errors":[error]});
        }
        Some(workers)
    } else { None };
    let started = if options.product_channels { Pipeline::start_service(config, product_configs.clone()) }
        else { Pipeline::start(config) };
    let mut pipeline = match started {
        Ok(pipeline) => pipeline,
        Err(error) => return json!({"passed":false,"requested":requested,"admission":admission,"errors":[error]}),
    };
    let began = Instant::now();
    pipeline.encode_snapshot();
    let initial = pipeline.snapshot.clone();
    let mut consumer = match Consumer::new(&frequencies, &initial, options.display) {
        Ok(consumer) => consumer,
        Err(error) => return json!({"passed":false,"requested":requested,"admission":admission,"errors":[error]}),
    };
    if let Some(workers) = prepared_workers {
        consumer.receiver = Some(ProductReceiver::new(&product_configs, &initial, workers));
    }
    let mut errors = Vec::new();
    let mut rows = Vec::new();
    let mut baseline = consumer.channels.clone();
    let mut baseline_audio = consumer.receiver.as_ref().map(|receiver| receiver.clocks.clone());
    let mut baseline_worker_stats = consumer.receiver.as_ref().map(|receiver| receiver.workers.stats());
    let mut window_snapshot = initial.clone();
    let mut window_start = None;
    let mut baseline_spectra = 0;
    let mut warmup_encode = Timing::default();
    let mut next_row = 5.0;
    loop {
        match pipeline.poll().and_then(|output| consumer.consume(output)) {
            Ok(()) => (),
            Err(error) => { errors.push(error); break; }
        }
        let now = Instant::now();
        if window_start.is_none() && began.elapsed().as_secs_f64() >= options.warmup {
            window_start = Some(now);
            baseline = consumer.channels.clone();
            baseline_audio = consumer.receiver.as_ref().map(|receiver| receiver.clocks.clone());
            baseline_worker_stats = consumer.receiver.as_ref().map(|receiver| receiver.workers.stats());
            baseline_spectra = consumer.spectra;
            pipeline.encode_snapshot();
            window_snapshot = pipeline.snapshot.clone();
            warmup_encode = std::mem::take(&mut consumer.encode);
        }
        if let Some(start) = window_start {
            let elapsed = now.duration_since(start).as_secs_f64();
            if elapsed >= options.seconds { break; }
            if elapsed >= next_row {
                pipeline.encode_snapshot();
                rows.push(json!({"window_elapsed_seconds":elapsed,"snapshot":snapshot_json(&pipeline.snapshot),
                    "delta_from_window_start":counter_deltas(&window_snapshot.actual, &pipeline.snapshot.actual),
                    "channels":channel_rows(&frequencies, &consumer.channels, &baseline, &consumer.registry),
                    "receiver_workers":consumer.receiver.as_ref().map(|receiver| receiver.workers.stats()),
                    "spectra":consumer.spectra - baseline_spectra,
                    "spectrum_encode":consumer.encode.snapshot()}));
                next_row = elapsed + 5.0;
            }
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    let window_end = Instant::now();
    pipeline.encode_snapshot();
    if window_start.is_none() {
        baseline = consumer.channels.clone();
        baseline_audio = consumer.receiver.as_ref().map(|receiver| receiver.clocks.clone());
        baseline_worker_stats = consumer.receiver.as_ref().map(|receiver| receiver.workers.stats());
        baseline_spectra = consumer.spectra;
        window_snapshot = pipeline.snapshot.clone();
        warmup_encode = std::mem::take(&mut consumer.encode);
    }
    let elapsed = window_start.map_or(0.0, |start| window_end.duration_since(start).as_secs_f64());
    let measured_channels = consumer.channels.clone();
    let measured_audio = consumer.receiver.as_ref().map(|receiver| receiver.clocks.clone());
    let measured_worker_stats = consumer.receiver.as_ref().map(|receiver| receiver.workers.stats());
    let measured_spectra = consumer.spectra - baseline_spectra;
    let measured_snapshot = pipeline.snapshot.clone();
    let measured_encode = std::mem::take(&mut consumer.encode);
    let stop_started = Instant::now();
    match pipeline.stop().and_then(|output| consumer.consume(output)) {
        Ok(()) => (),
        Err(error) => errors.push(format!("stop: {error}")),
    }
    if let Some(receiver) = &mut consumer.receiver {
        if let Err(error) = receiver.finish() { errors.push(format!("receiver finish: {error}")); }
    }
    pipeline.encode_snapshot();
    let final_snapshot = pipeline.snapshot.clone();
    if let Err(error) = consumer.verify_registry(&final_snapshot) {
        errors.push(error);
    }
    let stop_seconds = stop_started.elapsed().as_secs_f64();
    let mut lifetime_counter_errors = Vec::new();
    let mut window_counter_errors = Vec::new();
    check_counters(&initial.actual, &final_snapshot.actual, &mut lifetime_counter_errors);
    check_counters(&window_snapshot.actual, &measured_snapshot.actual, &mut window_counter_errors);
    errors.extend(lifetime_counter_errors.iter().map(|error| format!("lifetime: {error}")));
    errors.extend(window_counter_errors.iter().map(|error| format!("measurement: {error}")));
    if elapsed < options.seconds { errors.push("measurement window did not complete".into()); }
    // Wall-clock checks detect an unsustainable pipeline. Their tolerances come
    // from two framing/capture boundaries and the production consumer budget;
    // they are separate from the strict lifetime internal-clock check below.
    let packet_seconds = 16240.0 * final_snapshot.device_decimate as f64 / IQ_FS;
    let boundary_seconds = 2.0 * (super::CONSUMER_BUDGET.as_secs_f64() + packet_seconds);
    let wire_tolerance_seconds = boundary_seconds + 2.0 / 30.0;
    let spectrum_tolerance_seconds = boundary_seconds + 2.0 / 20.0;
    if elapsed <= wire_tolerance_seconds.max(spectrum_tolerance_seconds) {
        errors.push("measurement window is too short to distinguish steady throughput from boundary buffering".into());
    }
    for (index, (current, before)) in measured_channels.iter().zip(&baseline).enumerate() {
        let wire_seconds = (current.samples - before.samples) as f64 / consumer.registry[index].output_rate;
        if current.samples == before.samples || (wire_seconds - elapsed).abs() > wire_tolerance_seconds {
            errors.push(format!("channel {} window wire duration {wire_seconds:.6}s differs from {elapsed:.6}s (boundary tolerance {wire_tolerance_seconds:.6}s)", index + 1));
        }
    }
    if let (Some(current), Some(before)) = (&measured_audio, &baseline_audio) {
        for (index, (current, before)) in current.iter().zip(before).enumerate() {
            let audio_seconds = (current.samples - before.samples) as f64 / current.rate;
            if current.samples == before.samples || (audio_seconds - elapsed).abs() > wire_tolerance_seconds {
                errors.push(format!("channel {} native audio window duration {audio_seconds:.6}s differs from {elapsed:.6}s", index + 1));
            }
        }
    }
    if measured_spectra == 0 || (measured_spectra as f64 / 20.0 - elapsed).abs() > spectrum_tolerance_seconds {
        errors.push(format!("spectrum cadence differs from 20 fps: {measured_spectra} frames in {elapsed:.6}s"));
    }
    let clock_proof = validate_clocks(&consumer.channels, &consumer.registry, &final_snapshot, &mut errors);
    let receiver_proof = consumer.receiver.as_ref().map(|receiver| receiver.proof(&consumer.channels, &mut errors));
    if options.product_channels && final_snapshot.device_decimate != 1 {
        errors.push(format!("product capture decimation is {}, expected full-band 1", final_snapshot.device_decimate));
    }
    if options.product_channels && consumer.registry.iter().enumerate().any(|(i, entry)|
        consumer.registry[..i].iter().any(|prior| prior.output_rate == entry.output_rate)) {
        errors.push("product fixture must select three distinct actual IQ rates".into());
    }
    json!({"schema":"websa-sdr-pipeline-bench-v1","passed":errors.is_empty(),"errors":errors,"requested":requested,
        "admission":admission,"product_channels":options.product_channels,"receiver":receiver_proof,
        "elapsed_seconds":began.elapsed().as_secs_f64(),"stop_seconds":stop_seconds,
        "warmup":{"elapsed_seconds":window_start.map_or(window_end.duration_since(began).as_secs_f64(), |s| s.duration_since(began).as_secs_f64()),
            "channels":channel_rows(&frequencies,&baseline,&vec![Channel::default();options.channels],&consumer.registry),
            "spectra":baseline_spectra,"spectrum_encode":warmup_encode.snapshot(),"receiver_workers":baseline_worker_stats},
        "measurement":{"elapsed_seconds":elapsed,"channels":channel_rows(&frequencies,&measured_channels,&baseline,&consumer.registry),
            "spectra":measured_spectra,"spectra_fps":if elapsed > 0.0 { measured_spectra as f64 / elapsed } else { 0.0 },
            "receiver_workers":measured_worker_stats,
            "receiver_audio_channels":measured_audio.as_ref().zip(baseline_audio.as_ref()).map(|(current, before)|
                current.iter().zip(before).enumerate().map(|(i, (now, before))| json!({"channel":i + 1,
                    "audio_samples":now.samples - before.samples,"audio_frames":now.frames - before.frames,
                    "audio_rate_hz":now.rate,"wire_seconds":(now.samples - before.samples) as f64 / now.rate})).collect::<Vec<_>>()),
            "spectrum_encode":measured_encode.snapshot(),"wire_duration_tolerance_seconds":wire_tolerance_seconds,
            "spectrum_duration_tolerance_seconds":spectrum_tolerance_seconds,"target_spectra_fps":20,
            "capture_counters_passed":window_counter_errors.is_empty(),"capture_counter_errors":window_counter_errors,
            "delta":counter_deltas(&window_snapshot.actual,&measured_snapshot.actual)},
        "lifetime":{"channels":channel_rows(&frequencies,&consumer.channels,&vec![Channel::default();options.channels],&consumer.registry),
            "spectra":consumer.spectra,"spectrum_bytes":consumer.spectrum_bytes,"clock_proof":clock_proof,
            "capture_counters_passed":lifetime_counter_errors.is_empty(),"capture_counter_errors":lifetime_counter_errors,
            "delta":counter_deltas(&initial.actual,&final_snapshot.actual)},
        "stop":{"channels":channel_rows(&frequencies,&consumer.channels,&measured_channels,&consumer.registry),
            "spectra":consumer.spectra - baseline_spectra - measured_spectra,"spectrum_encode":consumer.encode.snapshot()},
        "snapshots":{"initial":snapshot_json(&initial),"window_start":snapshot_json(&window_snapshot),
            "window_end":snapshot_json(&measured_snapshot),"stopped":snapshot_json(&final_snapshot)},
        "timing_scope":"Each full timing snapshot is cumulative for this fresh capture; p99/max are not subtracted. Counter deltas contain only additive fields.",
        "samples":rows})
}

fn validate_clocks(channels: &[Channel], registry: &[Registered], snapshot: &Snapshot, errors: &mut Vec<String>) -> Value {
    let actual = &snapshot.actual;
    let Some(clocks) = actual.get("channel_clocks").and_then(Value::as_array) else {
        errors.push("internal per-channel fine clocks unavailable; wire continuity does not prove sample conservation".into());
        return json!({"available":false,"mode":"wire duration tolerance only","input_samples":actual.get("input_samples")});
    };
    if clocks.len() != channels.len() {
        errors.push(format!("internal clock channel count {} differs from requested {}", clocks.len(), channels.len()));
    }
    let input = actual["input_samples"].as_u64();
    let hop = if snapshot.device_decimate.is_power_of_two() && snapshot.device_decimate <= 64 {
        Some(64 / snapshot.device_decimate)
    } else { None };
    // PFB phase and the native fine phase start at zero. Each emits after a
    // complete hop/decimation period; stop drains without padding either one.
    let coarse = input.zip(hop).map(|(samples, hop)| samples / hop);
    let submitted = actual["pfb_submitted"].as_u64();
    let completed = actual["pfb_completed"].as_u64();
    let pool_groups = actual["pfb_groups"].as_u64();
    let fine_packets = actual["fine_packets"].as_u64();
    let fine_next_seq = actual["fine_next_seq"].as_u64();
    let groups_match = submitted.is_some() && submitted == completed && completed == fine_packets
        && fine_packets == fine_next_seq && coarse.is_some() && coarse == pool_groups;
    if !groups_match { errors.push("stopped PFB submitted/completed, coarse groups, and fine packet/sequence clocks differ or are unavailable".into()); }
    let mut proof = Vec::new();
    for (index, channel) in channels.iter().enumerate() {
        let factor = COARSE_RATE / registry[index].output_rate;
        let integral = factor.is_finite() && factor >= 1.0
            && (factor - factor.round()).abs() <= 1e-9 * factor;
        let decimation = integral.then_some(factor.round() as u64);
        let expected = coarse.zip(decimation).map(|(samples, factor)| samples / factor);
        let clock = clocks.iter().find(|clock| clock["channel"].as_u64() == Some(index as u64 + 1)
            && clock["channel_id"].as_u64() == Some(channel.channel_id as u64)
            && clock["epoch"].as_u64() == Some(channel.epoch as u64));
        let kernel_input = clock.and_then(|clock| clock["input_samples"].as_u64());
        let kernel_output = clock.and_then(|clock| clock["output_samples"].as_u64());
        let causal_first = channel.hop.checked_sub(1);
        let causal_end = channel.last_index.and_then(|index| channel.last_count.checked_sub(1)
            .and_then(|count| count.checked_mul(channel.hop)).and_then(|tail| index.checked_add(tail)));
        let expected_end = channel.samples.checked_mul(channel.hop).and_then(|end| end.checked_sub(1));
        let indices_match = channel.first_index == causal_first && causal_end == expected_end
            && hop.zip(decimation).and_then(|(hop, factor)| hop.checked_mul(factor)) == Some(channel.hop);
        let matches = clock.is_some() && channel.channel_id != 0 && expected.is_some()
            && coarse == kernel_input && expected == kernel_output
            && kernel_output == Some(channel.samples) && channel.samples > 0 && indices_match;
        if !matches {
            errors.push(format!("channel {} identity {}:{} clock conservation failed: input-derived coarse {coarse:?}/output {expected:?}, kernel {kernel_input:?}/{kernel_output:?}, wire {}",
                index + 1, channel.epoch, channel.channel_id, channel.samples));
        }
        proof.push(json!({"channel":index + 1,"channel_id":channel.channel_id,"epoch":channel.epoch,
            "internal":clock,"wire_samples":channel.samples,"matches":matches,
            "fine_decimation":decimation,"selected_iq_rate_hz":registry[index].output_rate,
            "expected_output_samples":expected,
            "first_input_index":channel.first_index,"last_sample_input_index":causal_end,
            "indices_match":indices_match}));
    }
    json!({"available":true,"input_samples":input,"input_hop":hop,"expected_coarse_samples":coarse,
        "pfb_submitted":submitted,"pfb_completed":completed,"pfb_groups":pool_groups,
        "fine_packets":fine_packets,"fine_next_seq":fine_next_seq,"groups_match":groups_match,"channels":proof})
}

pub fn run(args: &[String]) -> Result<(), String> {
    let output = args.windows(2).find(|pair| pair[0] == "--output").map(|pair| pair[1].as_str());
    let report = match Options::parse(args) {
        Ok(options) => execute(&options),
        Err(error) => json!({"schema":"websa-sdr-pipeline-bench-v1","passed":false,"errors":[error]}),
    };
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    if let Some(path) = output {
        std::fs::write(path, format!("{text}\n")).map_err(|e| {
            println!("{text}");
            format!("cannot write benchmark report {path}: {e}")
        })?;
    } else {
        println!("{text}");
    }
    if report["passed"].as_bool() == Some(true) { Ok(()) }
    else { Err(format!("SDR pipeline benchmark failed: {}", report["errors"])) }
}
