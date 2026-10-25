//! SSTV: slow-scan television.
//!
//! The decoder is the vendored `sstv` 0.3.1 tree (MIT) in `vendor/`. This module drives the
//! streaming front end (`vendor::decode::streaming`) from the channelized baseband:
//!
//! ```text
//!   baseband -> voice-band band pass -> real part -> 8 kHz i16 -> streaming decoder -> RGB rows
//! ```
//!
//! SSTV sends a picture as single-sideband audio: sync pulses at 1200 Hz, pixel tones from 1500 Hz to
//! 2300 Hz. The streaming decoder hands out each scanline as soon as its sequence pass completes, so
//! the image updates during reception. Ordered acquisition, row and completion events keep each
//! canvas tied to its transmission, even when one block spans multiple pictures.
//! The front end has a separate voice-band AGC.

/// A vendored copy of the reference decoder. Its own warnings (an unread field, an unused binding)
/// are upstream's, and editing the copy would make the next re-vendor a merge: silence them here.
#[allow(dead_code, unused_variables)]
pub mod vendor;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::mem;

use crate::ddc::agc::RmsAgc;
use crate::ddc::fir::ComplexBandFilter;
use crate::ddc::resampler::LinearResampler;
use crate::plugin::{DigitalDemodulator, ImageRef, ImageStatus};
use vendor::decode::streaming::{StreamingDecoder, StreamEvent};
use vendor::modes::Mode;

/// Rate the streaming decoder runs at. SSTV's highest tone is 2300 Hz, so 8 kHz has margin.
const DECODE_RATE_HZ: f64 = 8_000.0;
/// The voice band the audio is taken from, as the `usb` mode takes it.
/// The voice band the decoder reads, in Hz above the dial. The panadapter highlights this as
/// the SSTV passband. No separate filter runs at these frequencies any more (the complex
/// resampler to 8 kHz replaced it), but the band is still what the decoder responds to.
const AUDIO_LO_HZ: f64 = 300.0;
const AUDIO_HI_HZ: f64 = 2_700.0;
/// Full scale of the `i16` the decoder consumes.
const PCM_FULL_SCALE: f32 = 32_767.0;
/// The audio level the plugin's own AGC drives the decoded band to, as a fraction of full scale.
const AUDIO_TARGET: f64 = 0.3;

/// The mode table, with the names the UI shows. The vendored `Mode` has no name accessor. The
/// decode-window buttons and the mode pin read this list.
pub const SSTV_MODES: [(&str, Mode); 18] = [
    ("Scottie S1", vendor::modes::SCOTTIE_1),
    ("Scottie S2", vendor::modes::SCOTTIE_2),
    ("Scottie DX", vendor::modes::SCOTTIE_DX),
    ("Martin M1", vendor::modes::MARTIN_1),
    ("Martin M2", vendor::modes::MARTIN_2),
    ("Robot 36", vendor::modes::ROBOT_36),
    ("Robot 72", vendor::modes::ROBOT_72),
    ("Wrasse SC2-180", vendor::modes::WRASSE_SC2_180),
    ("Pasokon P3", vendor::modes::PASOKON_P3),
    ("Pasokon P5", vendor::modes::PASOKON_P5),
    ("Pasokon P7", vendor::modes::PASOKON_P7),
    ("PD-50", vendor::modes::PD_50),
    ("PD-90", vendor::modes::PD_90),
    ("PD-120", vendor::modes::PD_120),
    ("PD-160", vendor::modes::PD_160),
    ("PD-180", vendor::modes::PD_180),
    ("PD-240", vendor::modes::PD_240),
    ("PD-290", vendor::modes::PD_290),
];

/// The display name of a mode, or its resolution when the table does not know it.
pub fn mode_name(mode: Mode) -> String {
    SSTV_MODES
        .iter()
        .find(|(_, known)| *known == mode)
        .map_or_else(
            || {
                let (width, height) = mode.resolution();
                alloc::format!("{width}x{height}")
            },
            |(name, _)| (*name).to_string(),
        )
}

/// The index of a mode in [`SSTV_MODES`], which is what the UI's mode pin and highlight use.
pub fn mode_index(mode: Mode) -> Option<usize> {
    SSTV_MODES.iter().position(|(_, known)| *known == mode)
}

/// The mode's VIS code as the seven-bit number the calibration header carries.
pub fn mode_vis(mode: Mode) -> u8 {
    u8::from(mode.vis_code())
}

/// Exposed for the transmitter's table check: the duration of one pass of a mode's timing sequence, in
/// nanoseconds.
pub fn mode_sequence_ns(mode: Mode) -> u64 {
    mode.sequence_duration().ns()
}

/// The picture, and what is known about the transmission that carried it.
#[derive(Clone, PartialEq, Eq)]
pub struct SstvStatus {
    /// Bumped once per transmission: two reads with the same serial are the same picture, growing.
    pub serial: u64,
    /// Index into [`SSTV_MODES`] (and so the UI's button order).
    pub mode_index: usize,
    pub mode_name: &'static str,
    pub vis: u8,
    pub width: u32,
    pub height: u32,
    /// Scanlines decoded in this picture so far.
    pub lines: u32,
    /// `lines / height` as a whole percentage, 0..100.
    pub progress: u32,
    /// True once the picture's last scanline arrived (the transmission is over).
    pub complete: bool,
    /// Transmissions acquired since the plugin was created or reset (diagnostics).
    pub attempts: u64,
}

/// The decoded RGB888 picture. Row-major, `width * height * 3` bytes.
pub struct SstvImage {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct SstvPlugin {
    /// Resamples the real audio from the channel rate to the decoder's rate (the chain the
    /// original, hardware-proven plugin used: band pass, real part, this, AGC).
    resampler: LinearResampler,
    band: ComplexBandFilter,
    filtered: Vec<f64>,
    /// Levels the decoded band: the DDC's AGC targets the channel RMS, and a narrow SSTV band inside a
    /// wide channel can still be quiet. The readout reports the levelled audio.
    agc: RmsAgc,
    /// Scratch, reused per block so the hot path allocates nothing.
    mixed: Vec<f64>,
    resampled_f64: Vec<f32>,
    resampled: Vec<f32>,
    /// The streaming decoder, and the queue its samples are pushed through.
    decoder: StreamingDecoder,
    /// EMA of the decoded block level, for the readout.
    level: f32,
    /// 8 kHz samples seen since creation or reset.
    samples_seen: u64,
    status: Option<SstvStatus>,
    image: Option<SstvImage>,
    /// The mode the operator pinned, or `None` for VIS detection.
    pinned: Option<usize>,
    /// Consecutive quiet samples before the picture's last row (the transmission's own end is quiet).
    quiet_samples: u64,
    /// loudest block seen, for the quiet threshold (a fraction of the band's own peak).
    loud_peak: f32,
    /// Set once the picture's end has been declared quiet; rearmed by the next acquisition.
    finished: bool,
    /// Readout lines produced by the last block.
    readout: Vec<String>,
}

impl SstvPlugin {
    pub fn new(rate: f64) -> Self {
        Self {
            resampler: LinearResampler::new(rate, DECODE_RATE_HZ),
            band: {
                let (re, im) = crate::ddc::fir::design_complex_bandpass(
                    rate, AUDIO_LO_HZ, AUDIO_HI_HZ, 129);
                ComplexBandFilter::new(re, im)
            },
            filtered: Vec::new(),
            agc: RmsAgc::new(AUDIO_TARGET, 0.2, 0.05),
            mixed: Vec::new(),
            resampled_f64: Vec::new(),
            resampled: Vec::new(),
            decoder: StreamingDecoder::new(DECODE_RATE_HZ as u32),
            level: 0.0,
            samples_seen: 0,
            status: None,
            image: None,
            pinned: None,
            quiet_samples: 0,
            loud_peak: 0.0,
            finished: false,
            readout: Vec::new(),
        }
    }

    /// The current picture's state, or `None` before anything decoded.
    pub fn status(&self) -> Option<&SstvStatus> {
        self.status.as_ref()
    }

    /// The current picture, or `None` before anything decoded.
    pub fn image(&self) -> Option<&SstvImage> {
        self.image.as_ref()
    }

    /// The mode the operator pinned, or `None` when the VIS header decides it.
    pub fn mode_pin(&self) -> Option<usize> {
        self.pinned
    }

    /// Pin the decoder to a mode (index into [`SSTV_MODES`]) or clear the pin (`None` = auto).
    ///
    /// A pin makes the decoder lock on to line sync pulses instead of the header. The decoder restarts
    /// its search at the current position; the picture on screen stays.
    pub fn set_mode_pin(&mut self, index: Option<usize>) {
        let index = index.filter(|i| *i < SSTV_MODES.len());
        if index == self.pinned {
            return;
        }
        self.pinned = index;
        let pinned = index.map(|i| SSTV_MODES[i].1);
        self.decoder.set_pinned(pinned);
    }

    /// RMS of the decoded audio, 0..1 (the readout's level).
    pub fn audio_rms(&self) -> f32 {
        self.level
    }

    /// Seconds of decoded audio seen since creation or reset.
    pub fn seen_seconds(&self) -> f64 {
        self.samples_seen as f64 / DECODE_RATE_HZ
    }

    /// Start a canvas when the decoder acquires a transmission, before applying any of its rows.
    fn acquire(&mut self, mode: Mode, attempt: u32) {
        self.finished = false;
        self.quiet_samples = 0;
        let (width, height) = mode.resolution();
        let serial = self.status.as_ref().map_or(1, |status| status.serial + 1);
        let mode_index = mode_index(mode).unwrap_or(0);
        self.status = Some(SstvStatus {
            serial,
            mode_index,
            mode_name: SSTV_MODES[mode_index].0,
            vis: mode_vis(mode),
            width,
            height,
            lines: 0,
            progress: 0,
            complete: false,
            attempts: u64::from(attempt),
        });
        self.image = Some(SstvImage {
            rgb: alloc::vec![0u8; (width * height * 3) as usize],
            width,
            height,
        });
        let name = mode_name(mode);
        self.readout.push(alloc::format!(
            "SSTV {name} {width}x{height} VIS {}",
            mode_vis(mode)
        ));
    }

    fn absorb_events(&mut self, events: Vec<StreamEvent>) {
        for event in events {
            match event {
                StreamEvent::Acquired { mode, attempt } => self.acquire(mode, attempt),
                StreamEvent::Row(row) => {
                    let (Some(image), Some(status)) = (&mut self.image, &mut self.status) else {
                        continue;
                    };
                    let width = image.width as usize;
                    if row.index >= image.height || row.pixels.len() != width {
                        continue;
                    }
                    let base = row.index as usize * width * 3;
                    for (rgb, pixel) in image.rgb[base..base + width * 3]
                        .chunks_exact_mut(3).zip(&row.pixels)
                    {
                        rgb.copy_from_slice(&[pixel.red(), pixel.green(), pixel.blue()]);
                    }
                    status.lines = status.lines.max(row.index + 1);
                    status.progress = status.lines * 100 / status.height;
                }
                StreamEvent::Completed => {
                    self.finished = true;
                    if let Some(status) = self.status.as_mut() {
                        status.complete = true;
                        status.progress = 100;
                        self.readout.push(alloc::format!(
                            "SSTV {} {}x{} VIS {} 100%",
                            status.mode_name, status.width, status.height, status.vis
                        ));
                    }
                }
                StreamEvent::Abandoned => self.finished = true,
            }
        }
    }
}

impl DigitalDemodulator for SstvPlugin {
    fn id(&self) -> &'static str {
        "sstv"
    }

    fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
        let n = iq.len() & !1;
        if n == 0 {
            return Vec::new();
        }
        // This block's readout, if any; the previous block's is gone.
        self.readout.clear();
        // The original, hardware-proven front end, restored exactly: band pass at the channel
        // rate, real part, resample to 8 kHz, AGC, i16. The streaming rewrite had dropped the
        // AGC application (the field existed but nothing called it), which starved the decoder
        // of amplitude on real hardware while the full-scale fake fixture kept decoding.
        self.mixed.clear();
        self.mixed.extend(iq[..n].iter().map(|v| *v as f64));
        let iq_in = mem::take(&mut self.mixed);
        self.band.process_complex_into(&iq_in, &mut self.filtered);
        // The audio is the real part of the band-passed baseband, as in the `usb` mode.
        let audio: Vec<f64> = self.filtered.iter().step_by(2).copied().collect();
        self.resampler.process_into(&audio, &mut self.resampled_f64);
        self.mixed = audio;
        // Level the decimated audio to the decoder's working level. This is the plugin's own
        // AGC: without it a real-backend block (RMS ~45 in the i16 domain) reached the decoder
        // at i16 ~±45 — four hundred times below the level the header search needs — while the
        // tests, feeding full scale, passed.
        let decimated = mem::take(&mut self.resampled_f64);
        self.agc.process_into(&decimated, false, &mut self.resampled);
        self.resampled_f64 = decimated;
        let mut samples: Vec<i16> = Vec::with_capacity(self.resampled.len());
        let mut sum = 0.0;
        for value in &self.resampled {
            let sample = (*value * PCM_FULL_SCALE).clamp(-PCM_FULL_SCALE, PCM_FULL_SCALE) as i16;
            sum += f64::from(sample) * f64::from(sample);
            samples.push(sample);
        }
        if !samples.is_empty() {
            let rms = ((sum / samples.len() as f64).sqrt() / f64::from(PCM_FULL_SCALE)) as f32;
            self.level = self.level * 0.9 + rms * 0.1;
        }
        self.samples_seen += samples.len() as u64;

        let events = self.decoder.push(&samples);
        self.absorb_events(events);
        // A transmission ends quietly: the tones stop, and the last sequence is only cut when its end
        // is known. The band's own level against the loudest block seen tells the two apart; two
        // seconds of quiet end the picture.
        let raw = if self.resampled.is_empty() {
            0.0
        } else {
            let sum: f32 = self.resampled.iter().map(|value| value * value).sum::<f32>();
            (sum / self.resampled.len() as f32).sqrt()
        };
        if raw > self.loud_peak {
            self.loud_peak = raw;
        }
        let quiet = self.loud_peak > 0.005 && raw * 20.0 < self.loud_peak;
        self.quiet_samples = if quiet { self.quiet_samples + samples.len() as u64 } else { 0 };
        if !self.finished
            && self.decoder.mode().is_some()
            && self.quiet_samples >= (DECODE_RATE_HZ * 2.0) as u64
        {
            self.finished = true;
            self.quiet_samples = 0;
            let events = self.decoder.finish();
            self.absorb_events(events);
        }
        self.readout.clone()
    }

    fn buffered_input(&self) -> usize {
        self.samples_seen.min(usize::MAX as u64) as usize
    }

    fn attempts(&self) -> u64 {
        u64::from(self.decoder.acquisitions())
    }

    fn audio_level(&self) -> f32 {
        self.level
    }

    fn image(&self) -> Option<ImageRef<'_>> {
        self.image.as_ref().map(|image| ImageRef {
            rgb: &image.rgb,
            width: image.width,
            height: image.height,
        })
    }

    fn image_status(&self) -> Option<ImageStatus> {
        self.status.as_ref().map(|status| ImageStatus {
            width: status.width,
            height: status.height,
            lines: status.lines,
            progress: status.progress,
            vis: u32::from(status.vis),
            serial: status.serial,
            complete: status.complete,
            mode_index: status.mode_index as u32,
        })
    }

    fn set_mode_pin(&mut self, index: Option<usize>) -> bool {
        SstvPlugin::set_mode_pin(self, index);
        true
    }

    fn decoded(&self) -> Vec<(String, crate::plugin::DigitalReport)> {
        self.readout
            .iter()
            .map(|text| {
                (
                    text.clone(),
                    crate::plugin::DigitalReport {
                        frequency_hz: 0.0,
                        time_offset_s: 0.0,
                        snr_db: 0.0,
                    },
                )
            })
            .collect()
    }

    fn reset(&mut self) {
        self.resampler.reset();
        self.band.reset();
        self.agc.reset();
        self.decoder.reset();
        self.level = 0.0;
        self.samples_seen = 0;
        self.quiet_samples = 0;
        self.loud_peak = 0.0;
        self.finished = false;
        self.readout.clear();
        self.status = None;
        self.image = None;
    }

    fn retune(&mut self) {
        // Keep the decoder state: a backend flush on the same channel costs a fraction of a line, and
        // the decoder re-syncs on the next line pulse.
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use vendor::modes::ROBOT_36;
    use vendor::encode::{Encoder, Synthesizer};
    use vendor::RgbPixel;

    #[test]
    fn batched_pd290_then_robot36_preserves_picture_boundaries() {
        let pd = vendor::modes::PD_290;
        let encoder = Encoder::new(pd,
            core::iter::repeat(RgbPixel::new(220, 20, 20))
                .take((pd.resolution().0 * pd.resolution().1) as usize)).expect("PD encoder");
        let mut samples: Vec<i16> = Synthesizer::new(encoder, DECODE_RATE_HZ as u32).collect();
        samples.extend(core::iter::repeat(0).take(DECODE_RATE_HZ as usize * 2));
        let encoder = Encoder::new(ROBOT_36,
            core::iter::repeat(RgbPixel::new(20, 220, 20))
                .take(320 * 240)).expect("Robot encoder");
        samples.extend(Synthesizer::new(encoder, DECODE_RATE_HZ as u32)
            .take(DECODE_RATE_HZ as usize * 12));

        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        let events = plugin.decoder.push(&samples);
        let boundary = events.iter().position(|event| matches!(event,
            StreamEvent::Acquired { mode, attempt: 2 } if *mode == ROBOT_36))
            .expect("second acquisition in the same push");
        let mut pd_lines = 0;
        for event in &events[..boundary] {
            match event {
                StreamEvent::Row(row) => {
                    assert_eq!(row.index, pd_lines);
                    assert_eq!(row.pixels.len(), 800);
                    pd_lines += 1;
                }
                StreamEvent::Completed => assert_eq!(pd_lines, 616),
                _ => {}
            }
        }
        assert_eq!(pd_lines, 616, "all PD rows survive the batched boundary");
        assert!(matches!(events[boundary - 1], StreamEvent::Completed));
        plugin.absorb_events(events);
        let status = plugin.status().expect("second picture");
        assert_eq!(status.serial, 2);
        assert_eq!(status.attempts, 2);
        assert_eq!(status.mode_name, "Robot 36");
        assert_eq!((status.width, status.height), (320, 240));
        assert!(!status.complete, "PD completion must not complete Robot");
        assert!(status.lines > 0 && status.lines < 240);
        assert_eq!(status.progress, status.lines * 100 / 240);
        let image = plugin.image().unwrap();
        assert_eq!(image.rgb.len(), 320 * 240 * 3);
        let green = image.rgb[..status.lines as usize * 320 * 3]
            .chunks_exact(3).filter(|rgb| u16::from(rgb[1]) > u16::from(rgb[0]) + 50).count();
        assert!(green > status.lines as usize * 320 * 9 / 10);
        assert!(image.rgb[status.lines as usize * 320 * 3..].iter().all(|v| *v == 0));
    }

    #[test]
    fn repinning_incomplete_picture_replaces_canvas_on_acquisition() {
        for mode in [vendor::modes::MARTIN_1, vendor::modes::ROBOT_72] {
            let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
            let encoder = Encoder::new(ROBOT_36,
                core::iter::repeat(RgbPixel::new(220, 20, 20)).take(320 * 240))
                .expect("first encoder");
            let samples: Vec<i16> = Synthesizer::new(encoder, DECODE_RATE_HZ as u32)
                .take(DECODE_RATE_HZ as usize * 12).collect();
            let events = plugin.decoder.push(&samples);
            plugin.absorb_events(events);
            let first = plugin.status().expect("partial first picture").clone();
            assert!(!first.complete);
            assert!(first.lines > 0);

            plugin.set_mode_pin(mode_index(mode));
            assert!(plugin.status() == Some(&first), "retain the display during search");
            let (width, height) = mode.resolution();
            let encoder = Encoder::new(mode,
                core::iter::repeat(RgbPixel::new(20, 220, 20))
                    .take((width * height) as usize)).expect("second encoder");
            let samples: Vec<i16> = Synthesizer::new(encoder, DECODE_RATE_HZ as u32)
                .take(DECODE_RATE_HZ as usize * 15).collect();
            let events = plugin.decoder.push(&samples);
            plugin.absorb_events(events);
            let status = plugin.status().expect("new picture after re-pin");
            assert_eq!(status.serial, first.serial + 1);
            assert_eq!(status.mode_index, mode_index(mode).unwrap());
            assert_eq!((status.width, status.height), (width, height));
            assert!(status.lines > 0 && status.lines < height);
            assert!(!status.complete);
            assert_eq!(plugin.image().unwrap().rgb.len(), (width * height * 3) as usize);
        }
    }

    /// A picture to send: red brightens to the right, green downwards, blue diagonally. A wrong row
    /// order or channel swap shows up as error, not as a pass.
    pub(crate) fn gradient(width: usize, height: usize) -> Vec<RgbPixel> {
        (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| {
                    RgbPixel::new(
                        (x * 255 / width.max(1)) as u8,
                        (y * 255 / height.max(1)) as u8,
                        ((x + y) * 255 / (width + height).max(1)) as u8,
                    )
                })
            })
            .collect()
    }

    /// Encode a picture and turn it into complex baseband at the decoder's rate: the audio in I,
    /// nothing in Q, which is what the plugin's band pass and real part expect.
    pub(crate) fn baseband(mode: Mode, pixels: Vec<RgbPixel>) -> Vec<f32> {
        let encoder = Encoder::new(mode, pixels.into_iter()).expect("encoder");
        let samples: Vec<i16> = Synthesizer::new(encoder, DECODE_RATE_HZ as u32).collect();
        let mut iq = Vec::with_capacity(samples.len() * 2);
        for sample in &samples {
            iq.push(f32::from(*sample) / PCM_FULL_SCALE);
            iq.push(0.0);
        }
        iq
    }

    pub(crate) fn feed(plugin: &mut SstvPlugin, iq: &mut Vec<f32>) {
        let mut scratch = Vec::new();
        for chunk in iq.chunks(4096) {
            scratch.clear();
            scratch.extend_from_slice(chunk);
            plugin.process_iq(&scratch);
        }
        iq.clear();
    }

    fn mean_absolute_error(actual: &[u8], expected: &[u8]) -> f64 {
        let sum: u64 = actual
            .iter()
            .zip(expected)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum();
        sum as f64 / actual.len() as f64
    }

    #[test]
    fn decodes_a_robot_36_transmission_end_to_end() {
        let (width, height) = (ROBOT_36.resolution().0 as usize, ROBOT_36.resolution().1 as usize);
        let sent = gradient(width, height);
        let mut iq = baseband(ROBOT_36, sent.clone());
        // Trailing silence: a live stream always has audio after a transmission.
        iq.extend(core::iter::repeat(0.0f32).take(DECODE_RATE_HZ as usize * 5 * 2));

        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        feed(&mut plugin, &mut iq);

        let status = plugin.status().expect("a picture");
        assert_eq!(status.mode_name, "Robot 36");
        assert_eq!(status.vis, 8);
        assert_eq!((status.width, status.height), (width as u32, height as u32));
        assert!(status.complete, "{} of {} lines", status.lines, status.height);
        assert_eq!(status.progress, 100);
        assert!(status.serial >= 1);

        let image = plugin.image().expect("image");
        assert_eq!(image.rgb.len(), width * height * 3);
        let error = mean_absolute_error(&image.rgb, &sent.iter().flat_map(|p| [p.red(), p.green(), p.blue()]).collect::<Vec<u8>>());
        assert!(error < 24.0, "mean absolute error {error} per channel");
    }

    #[test]
    fn rows_arrive_while_the_transmission_is_still_running() {
        // Half a transmission: rows must already be on screen, with honest progress. The old batch
        // approach needed the whole transmission plus a decode attempt before anything showed.
        let (width, height) = (ROBOT_36.resolution().0 as usize, ROBOT_36.resolution().1 as usize);
        let encoder = Encoder::new(ROBOT_36, gradient(width, height).into_iter()).expect("encoder");
        let samples: Vec<i16> = Synthesizer::new(encoder, DECODE_RATE_HZ as u32).collect();
        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        let mut iq = Vec::new();
        for sample in &samples[..samples.len() / 2] {
            iq.push(f32::from(*sample) / PCM_FULL_SCALE);
            iq.push(0.0);
        }
        plugin.process_iq(&iq);
        let status = plugin.status().expect("a partial picture");
        assert!(!status.complete);
        assert_eq!(status.progress, status.lines * 100 / status.height);
        assert!(status.lines > 10, "only {} lines", status.lines);
        assert_eq!(status.serial, 1);
    }

    #[test]
    fn a_pinned_mode_decodes_without_the_vis_header() {
        let (width, height) = (ROBOT_36.resolution().0 as usize, ROBOT_36.resolution().1 as usize);
        let sent = gradient(width, height);
        let encoder = Encoder::new(ROBOT_36, sent.into_iter()).expect("encoder");
        let samples: Vec<i16> = Synthesizer::new(encoder, DECODE_RATE_HZ as u32).collect();
        // Skip the calibration header: only the line sync pulses are left to lock onto.
        let header = (DECODE_RATE_HZ * 3.0) as usize;
        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        plugin.set_mode_pin(mode_index(ROBOT_36));
        let mut iq = Vec::new();
        for sample in &samples[header..] {
            iq.push(f32::from(*sample) / PCM_FULL_SCALE);
            iq.push(0.0);
        }
        // Trailing silence: the picture completes when the transmission goes quiet (as on air).
        iq.extend(core::iter::repeat(0.0f32).take(DECODE_RATE_HZ as usize * 5 * 2));
        plugin.process_iq(&iq);
        let status = plugin.status().expect("a picture from sync pulses alone");
        assert_eq!(status.mode_name, "Robot 36");
        assert!(status.complete);
    }

    #[test]
    fn a_second_transmission_starts_a_new_serial() {
        let (width, height) = (ROBOT_36.resolution().0 as usize, ROBOT_36.resolution().1 as usize);
        let mut iq = baseband(ROBOT_36, gradient(width, height));
        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        feed(&mut plugin, &mut iq);
        let first = plugin.status().expect("first picture").serial;
        // Not `complete` yet: the last pass is cut when the transmission goes quiet (the gap below).

        // Silence, then a second transmission: a new serial, complete again.
        iq.extend(core::iter::repeat(0.0f32).take(DECODE_RATE_HZ as usize * 30 * 2));
        iq.extend(baseband(ROBOT_36, gradient(width, height)));
        iq.extend(core::iter::repeat(0.0f32).take(DECODE_RATE_HZ as usize * 5 * 2));
        feed(&mut plugin, &mut iq);
        let status = plugin.status().expect("second picture");
        assert!(status.complete);
        assert_eq!(status.serial, first + 1, "the second transmission is a new picture");
    }

    #[test]
    fn silence_between_transmissions_does_not_break_decoding() {
        // The bench failure this whole feature round started from: a long silent gap used to sit in a
        // buffer that every decode re-read, and a retune wiped it. Streaming has no buffer to wipe.
        let (width, height) = (ROBOT_36.resolution().0 as usize, ROBOT_36.resolution().1 as usize);
        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        let mut iq = baseband(ROBOT_36, gradient(width, height));
        feed(&mut plugin, &mut iq);
        // Not `complete` yet: the last pass is cut when the transmission goes quiet (the gap below).

        let gap: Vec<f32> = core::iter::repeat(0.0f32).take(DECODE_RATE_HZ as usize * 60 * 2).collect();
        feed(&mut plugin, &mut { let mut g = gap; g });
        let mut iq = baseband(ROBOT_36, gradient(width, height));
        iq.extend(core::iter::repeat(0.0f32).take(DECODE_RATE_HZ as usize * 8 * 2));
        feed(&mut plugin, &mut iq);
        let status = plugin.status().expect("second picture after a long gap");
        assert!(status.complete, "stopped at {} of {} lines", status.lines, status.height);
    }

    #[test]
    fn a_retune_keeps_the_stream_running() {
        let (width, height) = (ROBOT_36.resolution().0 as usize, ROBOT_36.resolution().1 as usize);
        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        let mut iq = baseband(ROBOT_36, gradient(width, height));
        let mut head = iq.split_off(iq.len() / 2);
        feed(&mut plugin, &mut iq);
        plugin.retune();              // a backend flush mid-transmission
        feed(&mut plugin, &mut head);
        let status = plugin.status().expect("a picture across a retune");
        assert!(status.lines > 10, "only {} lines across a retune", status.lines);
    }

    #[test]
    fn acquisitions_count_transmissions() {
        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        let mut iq = baseband(ROBOT_36, gradient(
            ROBOT_36.resolution().0 as usize,
            ROBOT_36.resolution().1 as usize,
        ));
        feed(&mut plugin, &mut iq);
        assert!(plugin.attempts() >= 1, "acquisitions={}", plugin.attempts());
        // Silence acquires nothing: the readout says so honestly.
        let before = plugin.attempts();
        let silence = vec![0.0f32; DECODE_RATE_HZ as usize * 10 * 2];
        plugin.process_iq(&silence);
        assert_eq!(plugin.attempts(), before);
    }

    #[test]
    fn the_level_readout_tracks_the_decoded_audio() {
        // The plugin's own AGC levels the band to AUDIO_TARGET; the readout must agree.
        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        let mut iq = baseband(ROBOT_36, gradient(
            ROBOT_36.resolution().0 as usize,
            ROBOT_36.resolution().1 as usize,
        ));
        feed(&mut plugin, &mut iq);
        let level = plugin.audio_rms();
        assert!(level > 0.1 && level < 0.9, "level {level}");
    }

    #[test]
    fn a_reset_clears_everything() {
        let (width, height) = (ROBOT_36.resolution().0 as usize, ROBOT_36.resolution().1 as usize);
        let mut plugin = SstvPlugin::new(DECODE_RATE_HZ);
        let mut iq = baseband(ROBOT_36, gradient(width, height));
        feed(&mut plugin, &mut iq);
        plugin.reset();
        assert!(plugin.status().is_none());
        assert!(plugin.image().is_none());
        assert_eq!(plugin.attempts(), 0);
        assert_eq!(plugin.seen_seconds(), 0.0);
    }
}

























#[cfg(test)]
mod test_backend_level {
    use super::tests::{gradient};
    use super::*;
    use vendor::encode::{Encoder, Synthesizer};
    use vendor::modes::ROBOT_36;

    /// The real backend's baseband AGC targets the channel RMS in the i16 domain (~45 measured on
    /// the bench), not normalized floats. The plugin's own AGC must level that to a decodable
    /// amplitude (this is the exact `src 4521% · dec 0% · 0 tries` bench failure).
    #[test]
    fn decodes_at_backend_i16_level() {
        let sent = gradient(320, 240);
        let enc = Encoder::new(ROBOT_36, sent.into_iter()).expect("enc");
        let rate = 48_225.308_641_975_31;
        let raw: Vec<i16> = Synthesizer::new(enc, rate as u32).collect();
        let mut iq: Vec<f32> = Vec::with_capacity(raw.len() * 2);
        for sample in &raw {
            iq.push(f32::from(*sample) / 32_767.0 * 5000.0); // RMS ~45 like the backend
            iq.push(0.0_f32);
        }
        iq.extend(core::iter::repeat(0.0f32).take(rate as usize * 5 * 2));
        let mut plugin = SstvPlugin::new(rate);
        let mut block = Vec::new();
        for chunk in iq.chunks(8192) {
            block.clear(); block.extend_from_slice(chunk);
            plugin.process_iq(&block);
            if plugin.status().is_some_and(|s| s.complete) { break; }
        }
        let ok = plugin.status().is_some_and(|s| s.complete);
        assert!(ok, "backend-level input failed: acq={} lvl={:.4}", plugin.attempts(), plugin.audio_rms());
    }
}

#[cfg(test)]
mod test_tiny_blocks {
    use super::tests::gradient;
    use super::*;
    use vendor::encode::{Encoder, Synthesizer};
    use vendor::modes::ROBOT_36;

    /// The browser feeds ~200-complex blocks (400 f32) of two-channel i16-domain baseband.
    /// The big-block test passes while the bench fails: reproduce the exact block size.
    #[test]
    fn decodes_in_tiny_blocks_at_backend_level() {
        let sent = gradient(320, 240);
        let enc = Encoder::new(ROBOT_36, sent.into_iter()).expect("enc");
        let rate = 48_223.992_f64;
        let raw: Vec<i16> = Synthesizer::new(enc, rate as u32).collect();
        let mut iq: Vec<f32> = Vec::with_capacity(raw.len() * 2);
        for sample in &raw {
            let v = f32::from(*sample) / 32_767.0 * 5000.0; // ~RMS 45 like the backend
            iq.push(v); iq.push(v * 0.3); // both channels carry signal
        }
        iq.extend(core::iter::repeat(0.0f32).take(rate as usize * 5 * 2));
        let mut plugin = SstvPlugin::new(rate);
        let mut block = Vec::new();
        for chunk in iq.chunks(400) { // 200 complex per block, like the browser
            block.clear(); block.extend_from_slice(chunk);
            plugin.process_iq(&block);
            if plugin.status().is_some_and(|s| s.complete) { break; }
        }
        let ok = plugin.status().is_some_and(|s| s.complete);
        assert!(ok, "tiny-block input failed: acq={} lvl={:.4} seen={:.0}s",
            plugin.attempts(), plugin.audio_rms(), plugin.seen_seconds());
    }
}

#[cfg(test)]
mod test_real_capture {
    use super::*;

    /// Replay REAL bench frames (SAN-90 DDC output while the Pluto transmits Robot 36) through
    /// the plugin. This is the ground-truth debug loop: whatever fails here fails on the bench.
    #[test]
    fn replays_real_bench_capture() {
        let Ok(bytes) = std::fs::read("/tmp/real_baseband_cont.f32") else {
            eprintln!("SKIP: /tmp/real_baseband.f32 not present");
            return;
        };
        let iq: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let rate = 48_226.550_f64; // updated below
        let mut plugin = SstvPlugin::new(rate);
        let mut block = Vec::new();
        for chunk in iq.chunks(400) {
            block.clear(); block.extend_from_slice(chunk);
            plugin.process_iq(&block);
        }
        eprintln!(
            "DBG-REAL acq={} lvl={:.4} seen={:.1}s mode={:?}",
            plugin.attempts(), plugin.audio_rms(), plugin.seen_seconds(),
            plugin.status().map(|s| s.mode_name)
        );
        eprintln!(
            "DBG-REAL acq={} lvl={:.4}",
            plugin.attempts(),
            plugin.audio_rms()
        );

    }
}