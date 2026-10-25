//! WebSA binary frame layout (little-endian), matched against Python golden fixtures.
//!
//! IQBF/IQAX carry linear complex f32 in ADC full-scale units: native channelization
//! preserves the signed-i16 amplitude, so framing applies only the fixed 1/32768 scale.
//! No signal-dependent gain belongs here; transport preserves keying and modulation depth.
//! Native receiver workers and raw IQ subscribers consume the same normalized samples.
use crate::FineOutput;
const BASEBAND_VERSION: u32 = 2;
const IQ_SCALE: f32 = 1.0 / 32768.0;
pub const MAX_BASEBAND_SAMPLES: usize = 65_536;

#[derive(Clone, Copy)]
pub enum BasebandMagic {
    Primary,
    Aux,
}

/// One selected fine channel's ordered IQBF/IQAX blocks. No silent packet/sample skipping.
pub struct BasebandFramer {
    channel_id: u64,
    magic: BasebandMagic,
    epoch: u64,
    center_hz: f64,
    rate: f64,
    block_samples: usize,
    next_packet: Option<u64>,
    next_sample: Option<u64>,
    wire_seq: u32,
    started: bool,
    pending: Vec<f32>,
    pending_input_index: u64,
    pending_delay: f64,
    finished: bool,
}

impl BasebandFramer {
    pub fn new(channel_id: u64, epoch: u64, center_hz: f64, rate: f64) -> Option<Self> {
        Self::with_magic(channel_id, epoch, center_hz, rate, BasebandMagic::Primary)
    }

    pub fn new_aux(channel_id: u64, epoch: u64, center_hz: f64, rate: f64) -> Option<Self> {
        Self::with_magic(channel_id, epoch, center_hz, rate, BasebandMagic::Aux)
    }

    fn with_magic(
        channel_id: u64,
        epoch: u64,
        center_hz: f64,
        rate: f64,
        magic: BasebandMagic,
    ) -> Option<Self> {
        if !center_hz.is_finite() || !(1.0..=1e7).contains(&rate)
            || (rate / 30.0).ceil() > MAX_BASEBAND_SAMPLES as f64
            || channel_id == 0 || channel_id > u32::MAX as u64
            || epoch == 0 || epoch > u32::MAX as u64
        {
            return None;
        }
        Some(Self {
            channel_id,
            magic,
            epoch,
            center_hz,
            rate,
            block_samples: (rate / 30.0).ceil() as usize,
            next_packet: None,
            next_sample: None,
            wire_seq: 0,
            started: false,
            pending: Vec::new(),
            pending_input_index: 0,
            pending_delay: 0.0,
            finished: false,
        })
    }

    pub fn push(&mut self, output: &FineOutput<'_>) -> Result<Vec<Vec<u8>>, &'static str> {
        if self.finished
            || output.channel_id != self.channel_id
            || output.epoch != self.epoch
            || output.output_rate != self.rate
            || output.iq.len() % 2 != 0
            || self.pending.len().checked_add(output.iq.len())
                .is_none_or(|len| len / 2 > MAX_BASEBAND_SAMPLES)
            || output.iq.iter().any(|value| !value.is_finite())
            || output.input_index == u64::MAX
            || !output.delay_samples.is_finite()
            || output.delay_samples < 0.0
            || self
                .next_sample
                .is_some_and(|next| output.output_index != next)
            || self.next_packet.is_some_and(|next| output.seq != next)
        {
            return Err("baseband sequence/clock mismatch");
        }
        self.next_packet = Some(
            output
                .seq
                .checked_add(1)
                .ok_or("baseband packet sequence overflow")?,
        );
        self.next_sample = Some(
            output
                .output_index
                .checked_add((output.iq.len() / 2) as u64)
                .ok_or("baseband sample clock overflow")?,
        );
        let mut frames = Vec::new();
        if output.iq.is_empty() {
            return Ok(frames);
        }
        if self.pending.is_empty() {
            self.pending_input_index = output.input_index;
            self.pending_delay = output.delay_samples;
        }
        if !self.started {
            frames.push(encode_baseband(
                self.magic,
                0,
                self.rate,
                self.center_hz,
                self.channel_id,
                self.epoch,
                self.pending_input_index,
                self.pending_delay,
                &[],
            )?);
            self.started = true;
        }
        self.pending
            .extend(output.iq.iter().map(|value| *value * IQ_SCALE));
        if self.pending.len() / 2 >= self.block_samples {
            self.wire_seq = self
                .wire_seq
                .checked_add(1)
                .ok_or("baseband wire sequence overflow")?;
            frames.push(encode_baseband(
                self.magic,
                self.wire_seq,
                self.rate,
                self.center_hz,
                self.channel_id,
                self.epoch,
                self.pending_input_index,
                self.pending_delay,
                &self.pending,
            )?);
            self.pending.clear();
        }
        Ok(frames)
    }

    /// Caller must send this tail frame at stream shutdown or report an incomplete stream.
    pub fn finish(&mut self) -> Result<Option<Vec<u8>>, &'static str> {
        if self.finished {
            return Err("baseband stream already finished");
        }
        self.finished = true;
        if self.pending.is_empty() {
            return Ok(None);
        }
        self.wire_seq = self
            .wire_seq
            .checked_add(1)
            .ok_or("baseband wire sequence overflow")?;
        let frame = encode_baseband(
            self.magic,
            self.wire_seq,
            self.rate,
            self.center_hz,
            self.channel_id,
            self.epoch,
            self.pending_input_index,
            self.pending_delay,
            &self.pending,
        )?;
        self.pending.clear();
        Ok(Some(frame))
    }
}

pub fn encode_baseband(
    magic: BasebandMagic,
    seq: u32,
    rate: f64,
    center_hz: f64,
    channel_id: u64,
    epoch: u64,
    input_index: u64,
    delay: f64,
    iq: &[f32],
) -> Result<Vec<u8>, &'static str> {
    let samples = u32::try_from(iq.len() / 2).map_err(|_| "baseband frame too large")?;
    let channel_id = u32::try_from(channel_id).map_err(|_| "baseband channel id exceeds u32")?;
    let epoch = u32::try_from(epoch).map_err(|_| "baseband epoch exceeds u32")?;
    if iq.len() % 2 != 0
        || iq.len() / 2 > MAX_BASEBAND_SAMPLES
        || !(1.0..=1e9).contains(&rate)
        || !center_hz.is_finite()
        || channel_id == 0
        || epoch == 0
        || !delay.is_finite()
        || delay < 0.0
        || input_index == u64::MAX
        || (seq == 0) != iq.is_empty()
    {
        return Err("invalid baseband frame");
    }
    let mut frame = Vec::with_capacity(56 + iq.len() * 4);
    frame.extend_from_slice(match magic {
        BasebandMagic::Primary => b"IQBF",
        BasebandMagic::Aux => b"IQAX",
    });
    frame.extend_from_slice(&BASEBAND_VERSION.to_le_bytes());
    frame.extend_from_slice(&seq.to_le_bytes());
    frame.extend_from_slice(&samples.to_le_bytes());
    frame.extend_from_slice(&rate.to_le_bytes());
    frame.extend_from_slice(&center_hz.to_le_bytes());
    frame.extend_from_slice(&channel_id.to_le_bytes());
    frame.extend_from_slice(&epoch.to_le_bytes());
    frame.extend_from_slice(&input_index.to_le_bytes());
    frame.extend_from_slice(&delay.to_le_bytes());
    for sample in iq {
        if !sample.is_finite() { return Err("nonfinite baseband IQ sample"); }
        frame.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(frame)
}

/// PCM source position uses a nonnegative proper fraction of one capture sample.
#[derive(Clone, Copy, Debug)]
pub struct AudioFrameMetadata {
    pub seq: u32,
    pub rate: f64,
    pub frequency_hz: f64,
    pub channel_id: u32,
    pub epoch: u32,
    pub input_index: u64,
    pub delay_seconds: f64,
    pub channels: u32,
    pub audio_index: u64,
    pub input_rate: f64,
    pub input_fraction_num: u64,
    pub input_fraction_den: u64,
    pub delay_known: bool,
    pub causal_presentation: bool,
}

pub const AUDIO_HEADER_BYTES: usize = 96;
pub const MAX_DECODE_EVENT_BYTES: usize = 2 * 1024 * 1024;

pub fn encode_audio(meta: AudioFrameMetadata, samples: &[f32]) -> Result<Vec<u8>, &'static str> {
    if !(1..=2).contains(&meta.channels)
        || samples.len() % meta.channels as usize != 0
        || !meta.rate.is_finite() || meta.rate <= 0.0
        || !meta.input_rate.is_finite() || meta.input_rate <= 0.0
        || !meta.frequency_hz.is_finite()
        || !meta.delay_seconds.is_finite() || meta.delay_seconds < 0.0
        || meta.channel_id == 0 || meta.epoch == 0
        || meta.input_index == u64::MAX
        || meta.input_fraction_den == 0
        || meta.input_fraction_num >= meta.input_fraction_den
        || (meta.seq == 0) != samples.is_empty()
        || samples.iter().any(|sample| !sample.is_finite())
    {
        return Err("invalid audio frame");
    }
    let frames = u32::try_from(samples.len() / meta.channels as usize)
        .map_err(|_| "audio frame too large")?;
    let mut frame = Vec::with_capacity(AUDIO_HEADER_BYTES + samples.len() * 4);
    frame.extend_from_slice(b"AUD2");
    frame.extend_from_slice(&1u32.to_le_bytes());
    frame.extend_from_slice(&meta.seq.to_le_bytes());
    frame.extend_from_slice(&frames.to_le_bytes());
    frame.extend_from_slice(&meta.rate.to_le_bytes());
    frame.extend_from_slice(&meta.frequency_hz.to_le_bytes());
    frame.extend_from_slice(&meta.channel_id.to_le_bytes());
    frame.extend_from_slice(&meta.epoch.to_le_bytes());
    frame.extend_from_slice(&meta.input_index.to_le_bytes());
    frame.extend_from_slice(&meta.delay_seconds.to_le_bytes());
    frame.extend_from_slice(&meta.channels.to_le_bytes());
    let format_flags = 1u32 | ((!meta.delay_known as u32) << 16)
        | ((meta.causal_presentation as u32) << 17);
    frame.extend_from_slice(&format_flags.to_le_bytes());
    frame.extend_from_slice(&meta.audio_index.to_le_bytes());
    frame.extend_from_slice(&meta.input_rate.to_le_bytes());
    frame.extend_from_slice(&meta.input_fraction_num.to_le_bytes());
    frame.extend_from_slice(&meta.input_fraction_den.to_le_bytes());
    for sample in samples { frame.extend_from_slice(&sample.to_le_bytes()); }
    Ok(frame)
}

pub fn encode_decode_event(json: &[u8]) -> Result<Vec<u8>, &'static str> {
    if json.len() > MAX_DECODE_EVENT_BYTES || json.is_empty() || std::str::from_utf8(json).is_err() {
        return Err("invalid decode event size or encoding");
    }
    let mut frame = Vec::with_capacity(8 + json.len());
    frame.extend_from_slice(b"SDE2");
    frame.extend_from_slice(&(json.len() as u32).to_le_bytes());
    frame.extend_from_slice(json);
    Ok(frame)
}

pub fn encode_rta(
    version: u32,
    freq: &[f64],
    spec: &[f32],
    waterfall: &[u16],
    max_density: u16,
    start_hz: f64,
    stop_hz: f64,
) -> Result<Vec<u8>, &'static str> {
    let points = u32::try_from(spec.len()).map_err(|_| "RTA frame too large")?;
    let wf_len = u16::try_from(waterfall.len()).map_err(|_| "RTA waterfall too large")?;
    if points < 2
        || wf_len == 0
        || freq.len() != spec.len()
        || !start_hz.is_finite()
        || !stop_hz.is_finite()
        || start_hz >= stop_hz
    {
        return Err("invalid RTA frame");
    }
    let mut frame = Vec::with_capacity(32 + freq.len() * 12 + waterfall.len() * 2);
    frame.extend_from_slice(b"RTAF");
    frame.extend_from_slice(&version.to_le_bytes());
    frame.extend_from_slice(&points.to_le_bytes());
    frame.extend_from_slice(&wf_len.to_le_bytes());
    frame.extend_from_slice(&max_density.to_le_bytes());
    frame.extend_from_slice(&start_hz.to_le_bytes());
    for hz in freq {
        frame.extend_from_slice(&hz.to_le_bytes());
    }
    for dbm in spec {
        frame.extend_from_slice(&dbm.to_le_bytes());
    }
    for value in waterfall {
        frame.extend_from_slice(&value.to_le_bytes());
    }
    frame.extend_from_slice(&stop_hz.to_le_bytes());
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_or_nonfinite_baseband_before_emitting_wire_data() {
        for magic in [BasebandMagic::Primary, BasebandMagic::Aux] {
            for iq in [vec![0.0; (MAX_BASEBAND_SAMPLES + 1) * 2], vec![f32::NAN, 0.0], vec![0.0, f32::INFINITY]] {
                assert!(encode_baseband(magic, 1, 1200.0, 100e6, 1, 1, 0, 0.0, &iq).is_err());
                let mut framer = BasebandFramer::with_magic(1, 1, 100e6, 1200.0, magic).unwrap();
                let mut output = FineOutput {
                    channel_id:1, epoch:1, bin:0, seq:0, stamp:0, input_index:0, output_index:0,
                    output_rate:1200.0, center_offset_hz:Some(0.0), delay_samples:0.0, iq:&iq,
                };
                assert!(framer.push(&output).is_err());
                let valid = [8192.0; 80];
                output.iq = &valid;
                let frames = framer.push(&output).unwrap();
                assert_eq!(u32::from_le_bytes(frames[0][8..12].try_into().unwrap()), 0);
                assert_eq!(u32::from_le_bytes(frames[1][8..12].try_into().unwrap()), 1);
                assert!(frames[1][56..].chunks_exact(4).all(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()) == 0.25));
                assert!(framer.finish().unwrap().is_none());
            }
        }
    }

    #[test]
    fn matches_python_and_browser_baseband_fixture() {
        let iq = [
            0.0, 0.0, 0.5, -0.5, 0.25, -0.25, 1.0, -1.0, 0.001, 1e-6, -0.001, -1e-6,
        ];
        let rate = 62.5e6 / 1024.0 / 32.0;
        let frame = encode_baseband(BasebandMagic::Primary, 7, rate, 100.2e6, 1, 1, 0, 0.0, &iq).unwrap();
        assert_eq!(
            frame,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../tests/fixtures/frames/baseband.bin"
            ))
        );
        assert_eq!(
            &encode_baseband(BasebandMagic::Aux, 7, rate, 100.2e6, 1, 1, 0, 0.0, &iq).unwrap()[..4],
            b"IQAX"
        );
        assert_eq!(
            encode_baseband(BasebandMagic::Primary, 0, rate, 100.2e6, 1, 1, 0, 0.0, &[])
                .unwrap()
                .len(),
            56
        );
        assert!(encode_baseband(BasebandMagic::Primary, 0, rate, 100.2e6, 1, 1, 0, 0.0, &iq).is_err());
        assert!(encode_baseband(BasebandMagic::Primary, 1, rate, 100.2e6, 1, 1, 0, 0.0, &iq[..11]).is_err());
    }

    #[test]
    fn preserves_first_nonempty_metadata_for_reset_coalescing_and_tail() {
        for magic in [BasebandMagic::Primary, BasebandMagic::Aux] {
            let mut framer = BasebandFramer::with_magic(9, 17, 100e6, 1200.0, magic).unwrap();
            let iq = [16384.0; 40];
            let mut output = FineOutput {
                channel_id: 9,
                epoch: 17,
                bin: 0,
                seq: 0,
                stamp: 0,
                input_index: 100,
                output_index: 0,
                output_rate: 1200.0,
                center_offset_hz: Some(0.0),
                delay_samples: 1.0,
                iq: &[],
            };
            let metadata = |frame: &[u8]| {
                assert_eq!(u32::from_le_bytes(frame[4..8].try_into().unwrap()), 2);
                assert_eq!(u32::from_le_bytes(frame[32..36].try_into().unwrap()), 9);
                assert_eq!(u32::from_le_bytes(frame[36..40].try_into().unwrap()), 17);
                (
                    u64::from_le_bytes(frame[40..48].try_into().unwrap()),
                    f64::from_le_bytes(frame[48..56].try_into().unwrap()),
                )
            };
            assert!(framer.push(&output).unwrap().is_empty());
            output.seq = 1;
            output.input_index = 767;
            output.delay_samples = 16.25;
            output.iq = &iq;
            let reset = framer.push(&output).unwrap();
            assert_eq!(reset.len(), 1);
            assert_eq!(reset[0].len(), 56);
            assert_eq!(metadata(&reset[0]), (767, 16.25));
            output.seq = 2;
            output.output_index = 20;
            output.input_index = 16127;
            output.delay_samples = 33.5;
            output.iq = &[];
            assert!(framer.push(&output).unwrap().is_empty());
            output.seq = 3;
            output.iq = &iq;
            let full = framer.push(&output).unwrap();
            assert_eq!(full.len(), 1);
            assert_eq!(metadata(&full[0]), (767, 16.25));
            assert_eq!(u32::from_le_bytes(full[0][12..16].try_into().unwrap()), 40);
            assert!(full[0][56..].chunks_exact(4).all(|bytes|
                f32::from_le_bytes(bytes.try_into().unwrap()) == 0.5));
            output.seq = 4;
            output.output_index = 40;
            output.input_index = 31487;
            output.delay_samples = 7.75;
            assert!(framer.push(&output).unwrap().is_empty());
            output.seq = 5;
            output.output_index = 60;
            output.input_index = 46847;
            output.delay_samples = 99.0;
            output.iq = &[];
            assert!(framer.push(&output).unwrap().is_empty());
            let tail = framer.finish().unwrap().unwrap();
            assert_eq!(metadata(&tail), (31487, 7.75));
            assert_eq!(u32::from_le_bytes(tail[12..16].try_into().unwrap()), 20);
        }
    }

    #[test]
    fn rejects_unrepresentable_identity_and_invalid_delay() {
        let max = u32::MAX as u64;
        for magic in [BasebandMagic::Primary, BasebandMagic::Aux] {
            for rate in [0.5, f64::NAN, f64::INFINITY, 1e9 + 1.0] {
                assert!(encode_baseband(magic, 0, rate, 100e6, 1, 1, 0, 0.0, &[]).is_err());
            }
            assert!(BasebandFramer::with_magic(1, 1, 100e6, 1e7, magic).is_none());
            assert!(encode_baseband(magic, 1, 1200.0, 100e6, 1, 1, 0, 0.0, &[]).is_err());
            for (id, epoch) in [(0, 1), (1, 0), (max + 1, 1), (1, max + 1)] {
                assert!(BasebandFramer::with_magic(id, epoch, 100e6, 1200.0, magic).is_none());
                assert!(encode_baseband(magic, 0, 1200.0, 100e6, id, epoch, 0, 0.0, &[]).is_err());
            }
            assert!(encode_baseband(magic, 0, 1200.0, 100e6, max, max, u64::MAX, 0.0, &[]).is_err());
            for delay in [-1.0, f64::NAN, f64::INFINITY] {
                assert!(encode_baseband(magic, 0, 1200.0, 100e6, 1, 1, 0, delay, &[]).is_err());
            }
        }
    }

    #[test]
    fn coalesces_fine_output_without_holes_and_flushes_on_new_channel() {
        let iq = vec![0.25_f32; 40];
        fn block(id: u64, epoch: u64, seq: u64, index: u64, iq: &[f32]) -> FineOutput<'_> {
            FineOutput {
                channel_id: id,
                epoch,
                bin: 5,
                seq,
                stamp: 0,
                input_index: 0,
                output_index: index,
                output_rate: 1200.0,
                center_offset_hz: Some(0.0),
                delay_samples: 0.0,
                iq,
            }
        }
        let mut framer = BasebandFramer::new(2, 7, 100_200_000.0, 1200.0).unwrap();
        let a = framer.push(&block(2, 7, 11, 0, &iq)).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(&a[0][..4], b"IQBF");
        assert_eq!(u32::from_le_bytes(a[0][8..12].try_into().unwrap()), 0);
        assert_eq!(a[0].len(), 56);
        assert!(framer.push(&block(2, 7, 12, 21, &iq)).is_err());
        assert!(framer.push(&block(2, 8, 12, 20, &iq)).is_err());
        let b = framer.push(&block(2, 7, 12, 20, &iq)).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(u32::from_le_bytes(b[0][8..12].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(b[0][12..16].try_into().unwrap()), 40);
        let tail = vec![1.0; 20];
        assert!(framer.push(&block(2, 7, 14, 40, &tail)).is_err());
        assert!(framer.push(&block(2, 7, 13, 40, &tail)).unwrap().is_empty());
        let end = framer.finish().unwrap().unwrap();
        assert_eq!(u32::from_le_bytes(end[8..12].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(end[12..16].try_into().unwrap()), 10);
        assert!(framer.push(&block(2, 7, 14, 50, &tail)).is_err());
        let mut tuned = BasebandFramer::new(3, 7, 100_210_000.0, 1200.0).unwrap();
        assert!(tuned.push(&block(2, 7, 14, 0, &tail)).is_err());
        let reset = tuned.push(&block(3, 7, 14, 0, &tail)).unwrap();
        assert_eq!(u32::from_le_bytes(reset[0][8..12].try_into().unwrap()), 0);
    }

    #[test]
    fn late_client_begins_at_current_fine_clock_then_checks_every_sample() {
        let mut framer = BasebandFramer::new(7, 3, 100e6, 1200.0).unwrap();
        let iq = [1.0_f32, 0.0].repeat(20);
        let packet = |seq, index| FineOutput {
            channel_id: 7,
            epoch: 3,
            bin: 5,
            seq,
            stamp: 0,
            input_index: 0,
            output_index: index,
            output_rate: 1200.0,
            center_offset_hz: Some(0.0),
            delay_samples: 0.0,
            iq: &iq,
        };
        assert_eq!(framer.push(&packet(190, 300)).unwrap().len(), 1);
        assert!(framer.push(&packet(191, 321)).is_err());
        let frames = framer.push(&packet(191, 320)).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(
            u32::from_le_bytes(frames[0][12..16].try_into().unwrap()),
            40
        );
    }

    fn framed_iq(iq: &[f32], packet_samples: usize, magic: BasebandMagic) -> Vec<f32> {
        let mut framer =
            BasebandFramer::with_magic(1, 2, 100e6, 12_000.0, magic).unwrap();
        let mut received = Vec::new();
        let mut append = |frame: Vec<u8>| {
            received.extend(
                frame[56..]
                    .chunks_exact(4)
                    .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())),
            );
        };
        for (seq, block) in iq.chunks(packet_samples * 2).enumerate() {
            let output = FineOutput {
                channel_id: 1,
                epoch: 2,
                bin: 5,
                seq: seq as u64,
                stamp: 0,
                input_index: 0,
                output_index: (seq * packet_samples) as u64,
                output_rate: 12_000.0,
                center_offset_hz: Some(0.0),
                delay_samples: 0.0,
                iq: block,
            };
            for frame in framer.push(&output).unwrap() {
                append(frame);
            }
        }
        if let Some(frame) = framer.finish().unwrap() {
            append(frame);
        }
        received
    }

    #[test]
    fn preserves_cw_dots_dashes_and_quiet_gaps_without_key_on_overshoot() {
        // 20 WPM: 60 ms dots, 180 ms dashes, with a quiet lead-in and inter-key gaps.
        let segments = [(false, 3), (true, 1), (false, 1), (true, 3), (false, 3)];
        let mut iq = Vec::new();
        let mut spans = Vec::new();
        for (keyed, units) in segments {
            let start = iq.len() / 2;
            for n in start..start + units * 720 {
                let phase = std::f64::consts::TAU * 700.0 * n as f64 / 12_000.0;
                let noise_phase = std::f64::consts::TAU * 2300.0 * n as f64 / 12_000.0;
                let carrier = if keyed { 8192.0 } else { 0.0 };
                iq.push((carrier * phase.cos() + 8.0 * noise_phase.cos()) as f32);
                iq.push((carrier * phase.sin() + 8.0 * noise_phase.sin()) as f32);
            }
            spans.push((keyed, start, iq.len() / 2));
        }
        for magic in [BasebandMagic::Primary, BasebandMagic::Aux] {
            for packet_samples in [1, 96, 511] {
                let received = framed_iq(&iq, packet_samples, magic);
                assert_eq!(received.len(), iq.len());
                for &(keyed, start, end) in &spans {
                    let envelope: Vec<f64> = received[start * 2..end * 2]
                        .chunks_exact(2)
                        .map(|z| (z[0] as f64).hypot(z[1] as f64))
                        .collect();
                    if keyed {
                        // Every point, including key-on and the dash tail, keeps its amplitude.
                        assert!(envelope.iter().all(|&v| (v - 0.25).abs() < 0.00025));
                    } else {
                        assert!(envelope.iter().all(|&v| (v - 8.0 / 32768.0).abs() < 1e-9));
                    }
                }
            }
        }
    }

    #[test]
    fn preserves_am_carrier_and_modulation_depth_across_packet_sizes() {
        // A 75%-modulated carrier: detect its envelope after the real wire round trip.
        let iq: Vec<f32> = (0..12_000)
            .flat_map(|n| {
                let t = n as f64 / 12_000.0;
                let envelope = 8192.0 * (1.0 + 0.75 * (std::f64::consts::TAU * 20.0 * t).cos());
                let phase = std::f64::consts::TAU * 1000.0 * t;
                [(envelope * phase.cos()) as f32, (envelope * phase.sin()) as f32]
            })
            .collect();
        for magic in [BasebandMagic::Primary, BasebandMagic::Aux] {
            for packet_samples in [17, 96, 511] {
                let received = framed_iq(&iq, packet_samples, magic);
                assert_eq!(received.len(), iq.len());
                let mut carrier = 0.0;
                let mut modulation = 0.0;
                for (n, z) in received.chunks_exact(2).enumerate() {
                    let envelope = (z[0] as f64).hypot(z[1] as f64);
                    carrier += envelope / 12_000.0;
                    modulation += 2.0 * envelope
                        * (std::f64::consts::TAU * 20.0 * n as f64 / 12_000.0).cos()
                        / 12_000.0;
                }
                assert!((carrier - 0.25).abs() < 1e-7, "carrier={carrier}");
                let depth = modulation / carrier;
                assert!((depth - 0.75).abs() < 1e-6, "AM depth={depth}");
            }
        }
    }

    #[test]
    fn matches_python_and_browser_rta_fixture() {
        let freq = [975e6, 985e6, 995e6, 1005e6, 1015e6, 1025e6];
        let spec = [-101.5, -98.25, -18.75, -19.5, -97.0, -103.25];
        let wf = [0, 17, 4096, 65535];
        let frame = encode_rta(3, &freq, &spec, &wf, 1234, 973_437_500.0, 1_026_562_500.0).unwrap();
        assert_eq!(
            frame,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../tests/fixtures/frames/rta.bin"
            ))
        );
        assert!(encode_rta(
            3,
            &freq,
            &spec[..5],
            &wf,
            1234,
            973_437_500.0,
            1_026_562_500.0
        )
        .is_err());
    }
}
