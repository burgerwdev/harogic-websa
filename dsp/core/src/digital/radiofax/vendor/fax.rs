// Copyright (c) 2026 boybook and Rasterwave contributors
// SPDX-License-Identifier: MIT

//! Streaming analog radiofax (WEFAX/HF fax) encoding and decoding.
//!
//! Radiofax has no VIS word. It is modeled separately from SSTV and uses APT
//! start/stop modulation plus phasing lines to establish IOC, line rate and
//! horizontal phase.

use std::collections::VecDeque;

use super::decoder::{FrequencyDemodulator, LinearResampler, WORK_SAMPLE_RATE};
#[cfg(test)]
use super::oscillator::Oscillator;
use super::{Error, Result};
#[cfg(test)]
use super::GrayImage;

const START_IOC_576_HZ: f64 = 300.0;
const START_IOC_288_HZ: f64 = 675.0;
const STOP_HZ: f64 = 450.0;
const MODULATION_WINDOW_SECONDS: f64 = 0.2;
const MODULATION_EVALUATION_SECONDS: f64 = 0.01;
const MODULATION_DROPOUT_SECONDS: f64 = 0.25;
const MAX_PHASING_STARTS: usize = 64;
const MAX_EOF_FILTER_DELAY_SAMPLES: usize = 16;

/// Radiofax index of cooperation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaxIoc {
    /// IOC 288, 905 pixels per line (`round(pi * IOC)`).
    Ioc288,
    /// IOC 576, 1810 pixels per line (`round(pi * IOC)`).
    Ioc576,
}

impl FaxIoc {
    /// Numeric IOC value.
    pub const fn value(self) -> u16 {
        match self {
            Self::Ioc288 => 288,
            Self::Ioc576 => 576,
        }
    }

    /// Conventional raster width defined by `round(pi * IOC)`.
    pub const fn width(self) -> u32 {
        match self {
            Self::Ioc288 => 905,
            Self::Ioc576 => 1810,
        }
    }

    const fn apt_start_hz(self) -> f64 {
        match self {
            Self::Ioc288 => START_IOC_288_HZ,
            Self::Ioc576 => START_IOC_576_HZ,
        }
    }
}

/// Radiofax scan rate in lines per minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaxLpm(u16);

impl FaxLpm {
    /// Common 60 LPM rate.
    pub const LPM_60: Self = Self(60);
    /// Common 90 LPM rate.
    pub const LPM_90: Self = Self(90);
    /// Common 120 LPM rate.
    pub const LPM_120: Self = Self(120);
    /// Common interoperability extension; WMO-No.386 does not list 180 LPM.
    pub const LPM_180: Self = Self(180);
    /// Common 240 LPM rate.
    pub const LPM_240: Self = Self(240);

    /// Validate a custom line rate.
    pub fn new(lines_per_minute: u16) -> Result<Self> {
        if !(30..=480).contains(&lines_per_minute) {
            return Err(Error::InvalidConfiguration(
                "radiofax LPM must be in 30..=480",
            ));
        }
        Ok(Self(lines_per_minute))
    }

    /// Numeric line rate.
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Whether this rate is one of the WMO-No.386 values.
    pub const fn is_wmo(self) -> bool {
        matches!(self.0, 60 | 90 | 120 | 240)
    }

    /// Duration of one scan line.
    pub fn line_seconds(self) -> f64 {
        60.0 / f64::from(self.0)
    }
}

/// Mapping direction between modulation and luminance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaxPolarity {
    /// Low frequency/level is black and high frequency/level is white.
    Normal,
    /// High frequency/level is black and low frequency/level is white.
    Inverted,
}

/// Radiofax image modulation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FaxModulation {
    /// Frequency-modulated audio subcarrier.
    FmSubcarrier {
        /// Mid-gray carrier frequency.
        center_hz: f32,
        /// One-sided black/white deviation.
        deviation_hz: f32,
        /// Luminance mapping direction.
        polarity: FaxPolarity,
    },
    /// Amplitude-modulated subcarrier with explicit black and white levels.
    AmSubcarrier {
        /// Audio subcarrier frequency.
        carrier_hz: f32,
        /// Relative amplitude for black pixels.
        black_level: f32,
        /// Relative amplitude for white pixels.
        white_level: f32,
    },
}

impl FaxModulation {
    /// WMO FM audio subcarrier: 1900 Hz center and +/-400 Hz deviation.
    pub const WMO_FM: Self = Self::FmSubcarrier {
        center_hz: 1900.0,
        deviation_hz: 400.0,
        polarity: FaxPolarity::Normal,
    };
}

/// Complete radiofax transmission parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaxSpec {
    /// Index of cooperation.
    pub ioc: FaxIoc,
    /// Scan rate.
    pub lpm: FaxLpm,
    /// Image modulation.
    pub modulation: FaxModulation,
    /// Duration of the phasing pattern. WMO transmissions use about 30 s.
    pub phasing_seconds: f32,
    /// APT start duration.
    pub start_seconds: f32,
    /// APT stop duration.
    pub stop_seconds: f32,
    /// Continuous black tail following the stop pattern.
    pub trailing_black_seconds: f32,
}

/// Automatic radiofax clock-recovery policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FaxClockRecoveryMode {
    /// Keep the nominal LPM clock and do not publish automatic corrections.
    Off,
    /// Recover line period and horizontal phase from phasing and image timing.
    #[default]
    Auto,
}

/// Evidence used for a radiofax clock estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaxClockSource {
    /// Nominal LPM timing is active.
    Nominal,
    /// APT phasing lines established timing.
    Phasing,
    /// Conservative timing inferred from stable image margins or content.
    ImageContent,
    /// A caller supplied an additive adjustment.
    Manual,
}

/// Radiofax clock-recovery lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaxClockStatus {
    /// No trusted timing evidence is available.
    Nominal,
    /// Phasing or image timing is being accumulated.
    Acquiring,
    /// A trusted initial clock is active.
    Locked,
    /// Image timing is refining an existing clock.
    Tracking,
    /// Timing evidence weakened; the last trusted model is frozen.
    Degraded,
}

/// One affine control point for correcting a nominal radiofax paper raster.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaxClockCalibrationPoint {
    /// Monotonic calibration revision.
    pub revision: u32,
    /// Paper line at which `phase_pixels` is defined.
    pub reference_line: u64,
    /// Horizontal correction at the reference line. Positive moves content right.
    pub phase_pixels: f32,
    /// Estimated source clock correction in parts per million.
    pub clock_ppm: f32,
    /// Normalized confidence in `0.0..=1.0`.
    pub confidence: f32,
    /// Evidence source.
    pub source: FaxClockSource,
    /// Recovery lifecycle at this point.
    pub status: FaxClockStatus,
}

/// Additive operator adjustment applied while correcting a paper segment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FaxPaperCorrection {
    /// Constant horizontal adjustment. Positive moves content right.
    pub phase_pixels: f32,
    /// Additional linear slant correction in parts per million.
    pub clock_ppm: f32,
}

impl Default for FaxClockCalibrationPoint {
    fn default() -> Self {
        Self {
            revision: 0,
            reference_line: 0,
            phase_pixels: 0.0,
            clock_ppm: 0.0,
            confidence: 0.0,
            source: FaxClockSource::Nominal,
            status: FaxClockStatus::Nominal,
        }
    }
}

/// Current radiofax clock snapshot.
pub type FaxClockState = FaxClockCalibrationPoint;

/// Coordinate basis used by a decoded radiofax row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaxRasterBasis {
    /// The row has already been cut with the recovered clock.
    Calibrated,
    /// The row uses the stable nominal LPM paper grid and needs calibration metadata.
    NominalPaper,
}

impl FaxSpec {
    /// WMO-style framing with a caller-selected line rate.
    ///
    /// Use [`FaxLpm::is_wmo`] when strict WMO rate compatibility is required.
    pub const fn standard(ioc: FaxIoc, lpm: FaxLpm) -> Self {
        Self {
            ioc,
            lpm,
            modulation: FaxModulation::WMO_FM,
            phasing_seconds: 30.0,
            start_seconds: 5.0,
            stop_seconds: 5.0,
            trailing_black_seconds: 10.0,
        }
    }

    /// Full square-sampling raster width.
    pub const fn width(self) -> u32 {
        self.ioc.width()
    }

    /// Number of phasing lines implied by duration and LPM.
    pub fn phasing_line_count(self) -> u16 {
        (f32::from(self.lpm.get()) * self.phasing_seconds / 60.0).round() as u16
    }
}

/// Radiofax encoder controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaxEncodeOptions {
    /// Peak output amplitude.
    pub amplitude: f32,
    /// Include APT selection and stop patterns. This requires phasing.
    pub include_apt: bool,
    /// Include standard phasing lines. Disabling both framing options produces
    /// image-only audio that requires out-of-band timing at the receiver.
    pub include_phasing: bool,
}

#[cfg(test)]
impl Default for FaxEncodeOptions {
    fn default() -> Self {
        Self {
            amplitude: 0.5,
            include_apt: true,
            include_phasing: true,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum FaxEncodeStage {
    AptStart { half_cycle: u64, total: u64 },
    Phasing { line: u16, part: u8 },
    Image { row: u32, pixel: u32 },
    AptStop { half_cycle: u64, total: u64 },
    BlackTail,
    Done,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct FaxSegment {
    frequency_hz: f64,
    amplitude: f32,
    remaining: usize,
}

/// Incremental radiofax encoder.
#[cfg(test)]
#[derive(Debug)]
pub struct FaxEncoder {
    image: GrayImage,
    spec: FaxSpec,
    sample_rate: u32,
    options: FaxEncodeOptions,
    oscillator: Oscillator,
    stage: FaxEncodeStage,
    active: Option<FaxSegment>,
    exact_sample_deadline: f64,
    scheduled_samples: u64,
    finished: bool,
    emitted: u64,
}

#[cfg(test)]
impl FaxEncoder {
    /// Construct a streaming radiofax encoder.
    ///
    /// The image width must equal [`FaxSpec::width`].
    pub fn new(
        image: GrayImage,
        spec: FaxSpec,
        sample_rate: u32,
        options: FaxEncodeOptions,
    ) -> Result<Self> {
        validate_rate(sample_rate)?;
        validate_spec(spec, sample_rate.min(WORK_SAMPLE_RATE))?;
        if image.width() != spec.width() {
            return Err(Error::InvalidConfiguration(
                "radiofax image width must equal the full IOC width",
            ));
        }
        if !(0.0..=1.0).contains(&options.amplitude) {
            return Err(Error::InvalidConfiguration(
                "radiofax amplitude must be in 0.0..=1.0",
            ));
        }
        if options.include_apt && !options.include_phasing {
            return Err(Error::InvalidConfiguration(
                "APT framing requires phasing; use image-only output when both are disabled",
            ));
        }
        let start_total =
            (f64::from(spec.start_seconds) * spec.ioc.apt_start_hz() * 2.0).round() as u64;
        let stage = if options.include_apt {
            FaxEncodeStage::AptStart {
                half_cycle: 0,
                total: start_total,
            }
        } else if options.include_phasing {
            FaxEncodeStage::Phasing { line: 0, part: 0 }
        } else {
            FaxEncodeStage::Image { row: 0, pixel: 0 }
        };
        Ok(Self {
            image,
            spec,
            sample_rate,
            options,
            oscillator: Oscillator::default(),
            stage,
            active: None,
            exact_sample_deadline: 0.0,
            scheduled_samples: 0,
            finished: false,
            emitted: 0,
        })
    }

    /// Fill an arbitrary-sized mono PCM output buffer.
    pub fn read_samples(&mut self, output: &mut [f32]) -> usize {
        if self.finished || output.is_empty() {
            return 0;
        }
        let mut written = 0;
        while written < output.len() {
            if self.active.is_none() {
                self.active = self.next_segment();
                if self.active.is_none() {
                    self.finished = true;
                    break;
                }
            }
            let active = self.active.as_mut().expect("fax segment exists");
            let count = active.remaining.min(output.len() - written);
            self.oscillator.fill(
                &mut output[written..written + count],
                active.frequency_hz,
                self.sample_rate,
                active.amplitude,
            );
            active.remaining -= count;
            written += count;
            self.emitted += count as u64;
            if active.remaining == 0 {
                self.active = None;
            }
        }
        written
    }

    /// Whether the stop pattern has completed.
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Number of samples emitted so far.
    pub const fn samples_emitted(&self) -> u64 {
        self.emitted
    }

    fn next_segment(&mut self) -> Option<FaxSegment> {
        match self.stage {
            FaxEncodeStage::AptStart { half_cycle, total } => {
                if half_cycle >= total {
                    self.stage = if self.options.include_phasing {
                        FaxEncodeStage::Phasing { line: 0, part: 0 }
                    } else {
                        FaxEncodeStage::Image { row: 0, pixel: 0 }
                    };
                    return self.next_segment();
                }
                self.stage = FaxEncodeStage::AptStart {
                    half_cycle: half_cycle + 1,
                    total,
                };
                Some(self.level_segment(
                    if half_cycle % 2 == 0 { 0 } else { 255 },
                    1.0 / (2.0 * self.spec.ioc.apt_start_hz()),
                ))
            }
            FaxEncodeStage::Phasing { line, part } => {
                if line >= self.spec.phasing_line_count() {
                    self.stage = FaxEncodeStage::Image { row: 0, pixel: 0 };
                    return self.next_segment();
                }
                let line_seconds = self.spec.lpm.line_seconds();
                if part == 0 {
                    self.stage = FaxEncodeStage::Phasing { line, part: 1 };
                    Some(self.level_segment(0, line_seconds * 0.95))
                } else {
                    self.stage = FaxEncodeStage::Phasing {
                        line: line + 1,
                        part: 0,
                    };
                    Some(self.level_segment(255, line_seconds * 0.05))
                }
            }
            FaxEncodeStage::Image { row, pixel } => {
                if row >= self.image.height() {
                    if self.options.include_apt {
                        let total =
                            (f64::from(self.spec.stop_seconds) * STOP_HZ * 2.0).round() as u64;
                        self.stage = FaxEncodeStage::AptStop {
                            half_cycle: 0,
                            total,
                        };
                    } else {
                        self.stage = FaxEncodeStage::Done;
                    }
                    return self.next_segment();
                }
                let width = self.spec.width();
                let value = self.image.pixels()
                    [row as usize * self.image.width() as usize + pixel as usize];
                let next_pixel = pixel + 1;
                self.stage = if next_pixel >= width {
                    FaxEncodeStage::Image {
                        row: row + 1,
                        pixel: 0,
                    }
                } else {
                    FaxEncodeStage::Image {
                        row,
                        pixel: next_pixel,
                    }
                };
                let mut segment = self.level_segment(value, 0.0);
                segment.remaining = self
                    .schedule_duration(self.spec.lpm.line_seconds() / f64::from(width))
                    as usize;
                Some(segment)
            }
            FaxEncodeStage::AptStop { half_cycle, total } => {
                if half_cycle >= total {
                    self.stage = FaxEncodeStage::BlackTail;
                    return self.next_segment();
                }
                self.stage = FaxEncodeStage::AptStop {
                    half_cycle: half_cycle + 1,
                    total,
                };
                Some(self.level_segment(
                    if half_cycle % 2 == 0 { 0 } else { 255 },
                    1.0 / (2.0 * STOP_HZ),
                ))
            }
            FaxEncodeStage::BlackTail => {
                self.stage = FaxEncodeStage::Done;
                (self.spec.trailing_black_seconds > 0.0)
                    .then(|| self.level_segment(0, f64::from(self.spec.trailing_black_seconds)))
            }
            FaxEncodeStage::Done => None,
        }
    }

    fn level_segment(&mut self, value: u8, seconds: f64) -> FaxSegment {
        let normalized = f32::from(value) / 255.0;
        let remaining = if seconds > 0.0 {
            self.schedule_duration(seconds) as usize
        } else {
            0
        };
        match self.spec.modulation {
            FaxModulation::FmSubcarrier {
                center_hz,
                deviation_hz,
                polarity,
            } => {
                let normalized = match polarity {
                    FaxPolarity::Normal => normalized,
                    FaxPolarity::Inverted => 1.0 - normalized,
                };
                FaxSegment {
                    frequency_hz: f64::from(center_hz - deviation_hz)
                        + f64::from(2.0 * deviation_hz * normalized),
                    amplitude: self.options.amplitude,
                    remaining,
                }
            }
            FaxModulation::AmSubcarrier {
                carrier_hz,
                black_level,
                white_level,
            } => FaxSegment {
                frequency_hz: f64::from(carrier_hz),
                amplitude: self.options.amplitude
                    * (black_level + (white_level - black_level) * normalized),
                remaining,
            },
        }
    }

    fn schedule_duration(&mut self, seconds: f64) -> u64 {
        self.exact_sample_deadline += seconds * f64::from(self.sample_rate);
        let deadline = self.exact_sample_deadline.round() as u64;
        let duration = deadline.saturating_sub(self.scheduled_samples);
        self.scheduled_samples = deadline;
        duration
    }
}

/// Encode a complete page through the streaming fax encoder.
#[cfg(test)]
pub fn encode_fax(image: GrayImage, spec: FaxSpec, sample_rate: u32) -> Result<Vec<f32>> {
    let mut encoder = FaxEncoder::new(image, spec, sample_rate, FaxEncodeOptions::default())?;
    let mut output = Vec::new();
    let mut chunk = [0.0_f32; 4096];
    while !encoder.is_finished() {
        let count = encoder.read_samples(&mut chunk);
        output.extend_from_slice(&chunk[..count]);
    }
    Ok(output)
}

/// Radiofax decoder policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaxDecoderConfig {
    /// Start raster decoding immediately with the configured IOC and line
    /// rate. This bypasses APT/phasing acquisition and keeps decoding through
    /// low signal levels so receivers can join an in-progress transmission.
    /// Both `ioc` and `lpm` must be configured when enabled.
    pub immediate_decode: bool,
    /// Automatic line-clock and horizontal-phase recovery.
    pub clock_recovery: FaxClockRecoveryMode,
    /// Coordinate basis used for emitted rows.
    pub raster_basis: FaxRasterBasis,
    /// Force IOC, or detect it from APT start modulation.
    pub ioc: Option<FaxIoc>,
    /// Force line rate, or infer a common rate from phasing lines.
    pub lpm: Option<FaxLpm>,
    /// Expected modulation.
    pub modulation: FaxModulation,
    /// Stop automatically after this many image lines. `None` accepts an
    /// unbounded page until APT stop or EOF.
    pub max_lines: Option<u32>,
    /// Full-scale amplitude used by AM decoding.
    pub am_full_scale: f32,
    /// Expected phasing duration. `Some(30.0)` follows WMO. Set `None` for
    /// non-standard senders and rely on pattern-end detection.
    pub expected_phasing_seconds: Option<f32>,
    /// Stable APT evidence required before announcing an IOC selection.
    pub apt_confirm_seconds: f32,
    /// Maximum time spent in one APT/phasing acquisition before returning to
    /// search.
    pub acquisition_timeout_seconds: f32,
    /// Continuous 450 Hz stop-envelope evidence required to close a page.
    pub stop_confirm_seconds: f32,
    /// Maximum continuous low-level interval while receiving a page.
    pub signal_loss_seconds: f32,
    /// Minimum exponentially averaged absolute PCM level treated as a signal.
    pub minimum_signal_level: f32,
    /// Minimum target-subcarrier energy divided by broadband absolute level.
    /// Set to zero only when an upstream squelch supplies loss decisions.
    pub minimum_carrier_coherence: f32,
}

impl Default for FaxDecoderConfig {
    fn default() -> Self {
        Self {
            immediate_decode: false,
            clock_recovery: FaxClockRecoveryMode::Auto,
            raster_basis: FaxRasterBasis::Calibrated,
            ioc: None,
            lpm: None,
            modulation: FaxModulation::WMO_FM,
            max_lines: None,
            am_full_scale: 0.5,
            expected_phasing_seconds: Some(30.0),
            apt_confirm_seconds: 4.0,
            acquisition_timeout_seconds: 45.0,
            stop_confirm_seconds: 4.0,
            signal_loss_seconds: 2.5,
            minimum_signal_level: 0.002,
            minimum_carrier_coherence: 1.2,
        }
    }
}

/// Borrowed radiofax decode event.
#[derive(Debug)]
#[non_exhaustive]
pub enum FaxDecodeEventRef<'a> {
    /// APT start modulation identified the IOC.
    AptDetected {
        /// Detected IOC.
        ioc: FaxIoc,
    },
    /// Phasing established raster timing.
    PhasingLocked {
        /// Detected or configured IOC.
        ioc: FaxIoc,
        /// Detected or configured line rate.
        lpm: FaxLpm,
        /// Output width.
        width: u32,
        /// Recovered timing model.
        clock: FaxClockCalibrationPoint,
    },
    /// Image pixels begin after phasing.
    PageStarted {
        /// Decoder-local page identifier.
        page_id: u64,
        /// Active fax specification.
        spec: FaxSpec,
        /// Timing model used to start this raster.
        clock: FaxClockCalibrationPoint,
    },
    /// One grayscale raster line is ready.
    LineReady {
        /// Decoder-local page identifier.
        page_id: u64,
        /// Zero-based row.
        line_index: u32,
        /// Grayscale pixels, valid until the callback returns.
        pixels: &'a [u8],
        /// Coordinate basis of `pixels`.
        basis: FaxRasterBasis,
    },
    /// Page reception ended.
    PageCompleted {
        /// Decoder-local page identifier.
        page_id: u64,
        /// Number of emitted lines.
        lines: u32,
        /// `true` when EOF ended the page rather than APT stop/max-lines.
        partial: bool,
    },
    /// APT or phasing acquisition was rejected and search resumed.
    SignalRejected {
        /// Stable machine-readable reason.
        reason: &'static str,
    },
}

/// Owned radiofax event.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum FaxDecodeEvent {
    /// APT start identified IOC.
    AptDetected {
        /// Detected IOC.
        ioc: FaxIoc,
    },
    /// Phasing established raster timing.
    PhasingLocked {
        /// IOC.
        ioc: FaxIoc,
        /// Line rate.
        lpm: FaxLpm,
        /// Width.
        width: u32,
        /// Recovered timing model.
        clock: FaxClockCalibrationPoint,
    },
    /// Page image started.
    PageStarted {
        /// Page identifier.
        page_id: u64,
        /// Active specification.
        spec: FaxSpec,
        /// Timing model used to start this raster.
        clock: FaxClockCalibrationPoint,
    },
    /// One owned grayscale row.
    LineReady {
        /// Page identifier.
        page_id: u64,
        /// Row index.
        line_index: u32,
        /// Grayscale pixels.
        pixels: Vec<u8>,
        /// Coordinate basis of `pixels`.
        basis: FaxRasterBasis,
    },
    /// Page ended.
    PageCompleted {
        /// Page identifier.
        page_id: u64,
        /// Number of rows.
        lines: u32,
        /// Whether EOF ended the page.
        partial: bool,
    },
    /// Acquisition was rejected.
    SignalRejected {
        /// Stable reason.
        reason: &'static str,
    },
}

impl FaxDecodeEventRef<'_> {
    /// Copy this event for storage or cross-thread delivery.
    pub fn to_owned(&self) -> FaxDecodeEvent {
        match self {
            Self::AptDetected { ioc } => FaxDecodeEvent::AptDetected { ioc: *ioc },
            Self::PhasingLocked {
                ioc,
                lpm,
                width,
                clock,
            } => FaxDecodeEvent::PhasingLocked {
                ioc: *ioc,
                lpm: *lpm,
                width: *width,
                clock: *clock,
            },
            Self::PageStarted {
                page_id,
                spec,
                clock,
            } => FaxDecodeEvent::PageStarted {
                page_id: *page_id,
                spec: *spec,
                clock: *clock,
            },
            Self::LineReady {
                page_id,
                line_index,
                pixels,
                basis,
            } => FaxDecodeEvent::LineReady {
                page_id: *page_id,
                line_index: *line_index,
                pixels: pixels.to_vec(),
                basis: *basis,
            },
            Self::PageCompleted {
                page_id,
                lines,
                partial,
            } => FaxDecodeEvent::PageCompleted {
                page_id: *page_id,
                lines: *lines,
                partial: *partial,
            },
            Self::SignalRejected { reason } => FaxDecodeEvent::SignalRejected { reason },
        }
    }
}

/// Synchronous sink for borrowed radiofax events.
pub trait FaxDecodeSink {
    /// Handle one event before borrowed buffers are reused.
    fn on_event(&mut self, event: FaxDecodeEventRef<'_>);
}

impl<F> FaxDecodeSink for F
where
    F: for<'a> FnMut(FaxDecodeEventRef<'a>),
{
    fn on_event(&mut self, event: FaxDecodeEventRef<'_>) {
        self(event);
    }
}

#[derive(Debug)]
enum FaxDecodeState {
    Searching,
    AwaitingAptEnd {
        ioc: FaxIoc,
        last_transition: u64,
        started_at: u64,
    },
    Phasing {
        ioc: FaxIoc,
        tracker: PhasingTracker,
        lpm: Option<FaxLpm>,
        clock_fit: Option<PhasingClockFit>,
        lock_reported: bool,
        phasing_started_at: u64,
    },
    Receiving {
        spec: FaxSpec,
        page_id: u64,
        raster: FaxRasterState,
        stop_started_at: Option<u64>,
        stop_hold: VecDeque<u8>,
    },
}

#[derive(Debug)]
struct FaxRasterState {
    line_index: u32,
    line_period_samples: f64,
    next_line_deadline: f64,
    scheduled_line_samples: u64,
    levels: Vec<u8>,
    line: Vec<u8>,
    basis: FaxRasterBasis,
}

#[derive(Clone, Copy, Debug)]
struct PhasingClockFit {
    period_samples: f64,
    phase_sample: f64,
    confidence: f32,
}

impl PhasingClockFit {
    fn nearest_grid_sample(self, target: u64) -> u64 {
        let index = ((target as f64 - self.phase_sample) / self.period_samples).round();
        (self.phase_sample + index * self.period_samples)
            .round()
            .max(0.0) as u64
    }

    fn calibration(self, lpm: FaxLpm) -> FaxClockCalibrationPoint {
        let nominal = lpm.line_seconds() * f64::from(WORK_SAMPLE_RATE);
        FaxClockCalibrationPoint {
            revision: 1,
            reference_line: 0,
            phase_pixels: 0.0,
            clock_ppm: (((self.period_samples / nominal) - 1.0) * 1_000_000.0) as f32,
            confidence: self.confidence,
            source: FaxClockSource::Phasing,
            status: FaxClockStatus::Locked,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct AptCandidate {
    ioc: FaxIoc,
    first_seen: u64,
    last_seen: u64,
}

#[derive(Debug, Default)]
struct PhasingTracker {
    black_starts: VecDeque<u64>,
    previous_black: Option<bool>,
    white_started_at: Option<u64>,
    valid_cycles: u8,
    invalid_cycles: u8,
    invalidated: bool,
}

impl PhasingTracker {
    fn new(level: u8, sample: u64) -> Self {
        let black = level < 128;
        let mut tracker = Self {
            previous_black: Some(black),
            ..Self::default()
        };
        if black {
            tracker.black_starts.push_back(sample);
        }
        tracker
    }

    fn process(
        &mut self,
        level: u8,
        sample: u64,
        expected_lpm: Option<FaxLpm>,
    ) -> Option<(FaxLpm, PhasingClockFit)> {
        let black = match self.previous_black {
            Some(true) => level < 160,
            Some(false) => level <= 96,
            None => level < 128,
        };
        let Some(previous_black) = self.previous_black.replace(black) else {
            if black {
                self.black_starts.push_back(sample);
            }
            return None;
        };
        if previous_black == black {
            return self.locked(expected_lpm);
        }
        if previous_black {
            self.white_started_at = Some(sample);
            return self.locked(expected_lpm);
        }

        let Some(previous_start) = self.black_starts.back().copied() else {
            self.black_starts.push_back(sample);
            return None;
        };
        let interval = sample.saturating_sub(previous_start) as f64;
        let white = self
            .white_started_at
            .map_or(0.0, |start| sample.saturating_sub(start) as f64);
        let rate = expected_lpm.or_else(|| infer_lpm_from_period(interval));
        let valid = rate.is_some_and(|rate| {
            let expected = rate.line_seconds() * f64::from(WORK_SAMPLE_RATE);
            let white_fraction = white / interval.max(1.0);
            (interval - expected).abs() <= expected * 0.04
                && ((0.02..=0.10).contains(&white_fraction)
                    || (0.45..=0.55).contains(&white_fraction))
        });
        if valid {
            self.black_starts.push_back(sample);
            while self.black_starts.len() > MAX_PHASING_STARTS {
                self.black_starts.pop_front();
            }
            self.valid_cycles = self.valid_cycles.saturating_add(1);
            self.invalid_cycles = 0;
        } else {
            self.invalid_cycles = self.invalid_cycles.saturating_add(1);
            if self.invalid_cycles >= 3 {
                self.black_starts.clear();
                self.black_starts.push_back(sample);
                self.valid_cycles = 0;
                self.invalid_cycles = 0;
                self.invalidated = true;
            }
        }
        self.white_started_at = None;
        self.locked(expected_lpm)
    }

    fn locked(&self, expected_lpm: Option<FaxLpm>) -> Option<(FaxLpm, PhasingClockFit)> {
        if self.valid_cycles < 4 || self.black_starts.len() < 5 {
            return None;
        }
        let fit = fitted_phasing_clock(&self.black_starts)?;
        let rate = expected_lpm.or_else(|| infer_lpm_from_period(fit.period_samples))?;
        (fit.confidence >= 0.45).then_some((rate, fit))
    }

    fn first_start(&self) -> Option<u64> {
        self.black_starts.front().copied()
    }

    fn last_start(&self) -> Option<u64> {
        self.black_starts.back().copied()
    }

    fn take_invalidated(&mut self) -> bool {
        std::mem::take(&mut self.invalidated)
    }
}

#[derive(Debug)]
struct ModulationDetector {
    window: VecDeque<f32>,
    capacity: usize,
    samples_since_evaluation: usize,
    last_estimate: Option<f32>,
    last_detection_sample: Option<u64>,
    previous_high: Option<bool>,
    level_transitions: VecDeque<u64>,
}

impl Default for ModulationDetector {
    fn default() -> Self {
        let capacity = work_samples(MODULATION_WINDOW_SECONDS);
        Self {
            window: VecDeque::with_capacity(capacity),
            capacity,
            samples_since_evaluation: 0,
            last_estimate: None,
            last_detection_sample: None,
            previous_high: None,
            level_transitions: VecDeque::with_capacity(512),
        }
    }
}

impl ModulationDetector {
    fn process(&mut self, level: u8, sample: u64) -> Option<f32> {
        let high = level >= 128;
        if self
            .previous_high
            .replace(high)
            .is_some_and(|previous| previous != high)
        {
            self.level_transitions.push_back(sample);
            if self.level_transitions.len() > 512 {
                self.level_transitions.pop_front();
            }
        }
        if self.window.len() == self.capacity {
            self.window.pop_front();
        }
        self.window.push_back(f32::from(level) - 127.5);
        self.samples_since_evaluation += 1;
        if self.window.len() < self.capacity
            || self.samples_since_evaluation < work_samples(MODULATION_EVALUATION_SECONDS)
        {
            return self.last_estimate;
        }
        self.samples_since_evaluation = 0;

        let energy = self
            .window
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>();
        if energy <= f64::EPSILON {
            self.last_estimate = None;
            return None;
        }

        let mut best = (0.0_f64, 0.0_f32);
        for frequency in [300.0_f32, 450.0, 675.0] {
            let power = goertzel_power(&self.window, frequency);
            if power > best.0 {
                best = (power, frequency);
            }
        }
        let normalized = best.0 / (energy * self.window.len() as f64);
        self.last_estimate = (normalized >= 0.01).then_some(best.1);
        if self.last_estimate.is_some() {
            self.last_detection_sample = Some(sample);
        }
        self.last_estimate
    }

    fn last_transition(&self) -> Option<u64> {
        self.level_transitions.back().copied()
    }

    fn control_run_start(&self, frequency_hz: f64, minimum_intervals: usize) -> Option<u64> {
        let expected = f64::from(WORK_SAMPLE_RATE) / (2.0 * frequency_hz);
        let tolerance = (expected * 0.2).max(2.0);
        let transitions: Vec<_> = self.level_transitions.iter().copied().collect();
        let mut start = *transitions.last()?;
        let mut intervals = 0;
        for pair in transitions.windows(2).rev() {
            let interval = pair[1].saturating_sub(pair[0]) as f64;
            if (interval - expected).abs() > tolerance {
                break;
            }
            start = pair[0];
            intervals += 1;
        }
        (intervals >= minimum_intervals).then_some(start)
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug)]
struct AmEnvelopeDetector {
    oscillator_sin: f64,
    oscillator_cos: f64,
    oscillator_sin_step: f64,
    oscillator_cos_step: f64,
    filtered_i: f64,
    filtered_q: f64,
    carrier_hz: f32,
    until_normalize: u16,
}

impl AmEnvelopeDetector {
    fn new(carrier_hz: f32) -> Self {
        let step = std::f64::consts::TAU * f64::from(carrier_hz) / f64::from(WORK_SAMPLE_RATE);
        let (oscillator_sin_step, oscillator_cos_step) = step.sin_cos();
        Self {
            oscillator_sin: 0.0,
            oscillator_cos: 1.0,
            oscillator_sin_step,
            oscillator_cos_step,
            filtered_i: 0.0,
            filtered_q: 0.0,
            carrier_hz,
            until_normalize: 4096,
        }
    }

    fn process(&mut self, sample: f32) -> f32 {
        let sample = f64::from(sample);
        let mixed_i = sample * self.oscillator_cos;
        let mixed_q = -sample * self.oscillator_sin;
        let next_sin = self.oscillator_sin * self.oscillator_cos_step
            + self.oscillator_cos * self.oscillator_sin_step;
        let next_cos = self.oscillator_cos * self.oscillator_cos_step
            - self.oscillator_sin * self.oscillator_sin_step;
        self.oscillator_sin = next_sin;
        self.oscillator_cos = next_cos;
        self.until_normalize -= 1;
        if self.until_normalize == 0 {
            let magnitude = self.oscillator_sin.hypot(self.oscillator_cos);
            if magnitude > f64::EPSILON {
                self.oscillator_sin /= magnitude;
                self.oscillator_cos /= magnitude;
            }
            self.until_normalize = 4096;
        }

        const ALPHA: f64 = 0.55;
        self.filtered_i += ALPHA * (mixed_i - self.filtered_i);
        self.filtered_q += ALPHA * (mixed_q - self.filtered_q);
        (2.0 * self.filtered_i.hypot(self.filtered_q)) as f32
    }

    fn reset(&mut self) {
        *self = Self::new(self.carrier_hz);
    }
}

fn goertzel_power(window: &VecDeque<f32>, frequency_hz: f32) -> f64 {
    let omega = std::f64::consts::TAU * f64::from(frequency_hz) / f64::from(WORK_SAMPLE_RATE);
    let coefficient = 2.0 * omega.cos();
    let mut previous = 0.0_f64;
    let mut previous2 = 0.0_f64;
    for sample in window {
        let current = f64::from(*sample) + coefficient * previous - previous2;
        previous2 = previous;
        previous = current;
    }
    previous2.mul_add(
        previous2,
        previous * previous - coefficient * previous * previous2,
    )
}

#[derive(Debug)]
struct LevelHistory {
    values: VecDeque<(u64, u8)>,
    capacity: usize,
}

impl LevelHistory {
    fn new(capacity: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push(&mut self, sample: u64, value: u8) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back((sample, value));
    }

    fn copy_from(&self, start: u64, output: &mut Vec<u8>) {
        output.extend(
            self.values
                .iter()
                .filter(|(sample, _)| *sample >= start)
                .map(|(_, value)| *value),
        );
    }

    fn clear(&mut self) {
        self.values.clear();
    }
}

/// Streaming APT/phasing radiofax decoder.
#[derive(Debug)]
pub struct FaxDecoder {
    config: FaxDecoderConfig,
    resampler: LinearResampler,
    demodulator: FrequencyDemodulator,
    am_detector: AmEnvelopeDetector,
    modulation_detector: ModulationDetector,
    level_history: LevelHistory,
    state: FaxDecodeState,
    working_sample: u64,
    next_page_id: u64,
    signal_level_ema: f32,
    carrier_coherence_ema: f32,
    last_signal_sample: u64,
    apt_candidate: Option<AptCandidate>,
    search_phasing: PhasingTracker,
    finished: bool,
}

impl FaxDecoder {
    /// Construct a radiofax decoder.
    pub fn new(input_sample_rate: u32, config: FaxDecoderConfig) -> Result<Self> {
        validate_rate(input_sample_rate)?;
        validate_modulation(config.modulation, input_sample_rate.min(WORK_SAMPLE_RATE))?;
        if config.immediate_decode && (config.ioc.is_none() || config.lpm.is_none()) {
            return Err(Error::InvalidConfiguration(
                "immediate_decode requires configured ioc and lpm",
            ));
        }
        if config.am_full_scale <= 0.0 || !config.am_full_scale.is_finite() {
            return Err(Error::InvalidConfiguration(
                "am_full_scale must be finite and positive",
            ));
        }
        if config
            .expected_phasing_seconds
            .is_some_and(|seconds| !valid_bounded_seconds(seconds, 0.1, 120.0))
        {
            return Err(Error::InvalidConfiguration(
                "expected_phasing_seconds must be finite and positive",
            ));
        }
        if !valid_bounded_seconds(config.apt_confirm_seconds, 0.2, 30.0)
            || !valid_bounded_seconds(config.acquisition_timeout_seconds, 1.0, 120.0)
            || !valid_bounded_seconds(config.stop_confirm_seconds, 0.2, 30.0)
            || !valid_bounded_seconds(config.signal_loss_seconds, 0.2, 30.0)
        {
            return Err(Error::InvalidConfiguration(
                "fax decoder timeouts are outside their supported finite ranges",
            ));
        }
        if !config.minimum_signal_level.is_finite()
            || !(0.0..=1.0).contains(&config.minimum_signal_level)
            || !config.minimum_carrier_coherence.is_finite()
            || !(0.0..=2.0).contains(&config.minimum_carrier_coherence)
        {
            return Err(Error::InvalidConfiguration(
                "fax signal level/coherence thresholds are outside their finite ranges",
            ));
        }
        let (demodulator_center, am_carrier) = match config.modulation {
            FaxModulation::AmSubcarrier { carrier_hz, .. } => (carrier_hz, carrier_hz),
            FaxModulation::FmSubcarrier { center_hz, .. } => (center_hz, center_hz),
        };
        Ok(Self {
            config,
            resampler: LinearResampler::new(input_sample_rate),
            demodulator: FrequencyDemodulator::new(demodulator_center),
            am_detector: AmEnvelopeDetector::new(am_carrier),
            modulation_detector: ModulationDetector::default(),
            level_history: LevelHistory::new(WORK_SAMPLE_RATE as usize * 3),
            state: FaxDecodeState::Searching,
            working_sample: 0,
            next_page_id: 1,
            signal_level_ema: 0.0,
            carrier_coherence_ema: 0.0,
            last_signal_sample: 0,
            apt_candidate: None,
            search_phasing: PhasingTracker::default(),
            finished: false,
        })
    }

    /// Push arbitrary-sized mono PCM.
    pub fn push_f32(&mut self, input: &[f32], sink: &mut impl FaxDecodeSink) -> Result<usize> {
        if self.finished {
            return Err(Error::DecoderFinished);
        }
        if input.iter().any(|sample| !sample.is_finite()) {
            return Err(Error::NonFiniteSample);
        }
        let mut events = 0;
        for &sample in input {
            let mut resampled = [0.0_f32; 4];
            let mut count = 0;
            self.resampler.push(sample, |value| {
                if count < resampled.len() {
                    resampled[count] = value;
                    count += 1;
                }
            });
            for value in &resampled[..count] {
                events += self.process_working(*value, sink);
            }
        }
        Ok(events)
    }

    /// Collect owned events for one PCM chunk.
    pub fn process_into(
        &mut self,
        input: &[f32],
        output: &mut Vec<FaxDecodeEvent>,
    ) -> Result<usize> {
        let mut sink = |event: FaxDecodeEventRef<'_>| output.push(event.to_owned());
        self.push_f32(input, &mut sink)
    }

    /// Finalize an active page.
    pub fn finish(&mut self, sink: &mut impl FaxDecodeSink) -> Result<usize> {
        if self.finished {
            return Err(Error::DecoderFinished);
        }
        let state = std::mem::replace(&mut self.state, FaxDecodeState::Searching);
        let mut events = 0;
        if let FaxDecodeState::Receiving {
            page_id,
            mut raster,
            stop_started_at,
            stop_hold,
            ..
        } = state
        {
            if self.config.immediate_decode
                || (stop_started_at.is_none()
                    && self.signal_level_ema >= self.config.minimum_signal_level)
            {
                raster.levels.extend(stop_hold);
                events += emit_available_fax_lines(
                    page_id,
                    &mut raster,
                    self.config.max_lines,
                    true,
                    false,
                    sink,
                );
            }
            sink.on_event(FaxDecodeEventRef::PageCompleted {
                page_id,
                lines: raster.line_index,
                partial: true,
            });
            events += 1;
        }
        self.finished = true;
        Ok(events)
    }

    /// End an active page from receiver-specific squelch or carrier evidence.
    ///
    /// This complements the built-in PCM level/coherence gate when an SDR or
    /// radio supplies a more authoritative signal-present decision.
    pub fn mark_signal_lost(&mut self, sink: &mut impl FaxDecodeSink) -> Result<usize> {
        if self.finished {
            return Err(Error::DecoderFinished);
        }
        let active = match &self.state {
            FaxDecodeState::Receiving {
                page_id, raster, ..
            } => Some((*page_id, raster.line_index)),
            _ => None,
        };
        let Some((page_id, lines)) = active else {
            return Ok(0);
        };
        sink.on_event(FaxDecodeEventRef::PageCompleted {
            page_id,
            lines,
            partial: true,
        });
        self.state = FaxDecodeState::Searching;
        self.reset_fax_acquisition();
        Ok(1)
    }

    /// Reset and accept a new stream.
    pub fn reset(&mut self) {
        self.resampler.reset();
        self.demodulator.reset();
        self.am_detector.reset();
        self.modulation_detector.reset();
        self.level_history.clear();
        self.state = FaxDecodeState::Searching;
        self.working_sample = 0;
        self.signal_level_ema = 0.0;
        self.carrier_coherence_ema = 0.0;
        self.last_signal_sample = 0;
        self.apt_candidate = None;
        self.search_phasing = PhasingTracker::default();
        self.finished = false;
    }

    fn process_working(&mut self, sample: f32, sink: &mut impl FaxDecodeSink) -> usize {
        let frequency = self.demodulator.process(sample, self.working_sample);
        let am_envelope = self.am_detector.process(sample);
        self.signal_level_ema += (sample.abs() - self.signal_level_ema) * 0.02;
        let carrier_level = match self.config.modulation {
            FaxModulation::FmSubcarrier { .. } => self.demodulator.carrier_level(),
            FaxModulation::AmSubcarrier { .. } => am_envelope,
        };
        let coherence = carrier_level / self.signal_level_ema.max(1.0e-9);
        self.carrier_coherence_ema += (coherence - self.carrier_coherence_ema) * 0.001;
        let signal_present = self.signal_level_ema >= self.config.minimum_signal_level
            && (self.config.minimum_carrier_coherence == 0.0
                || self.carrier_coherence_ema >= self.config.minimum_carrier_coherence);
        if signal_present {
            self.last_signal_sample = self.working_sample;
        }
        let level = match self.config.modulation {
            modulation @ FaxModulation::FmSubcarrier { .. } => {
                frequency_to_gray(frequency, modulation)
            }
            FaxModulation::AmSubcarrier {
                black_level,
                white_level,
                ..
            } => {
                let relative = am_envelope / self.config.am_full_scale;
                (((relative - black_level) / (white_level - black_level)) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8
            }
        };
        let modulation_hz = self.modulation_detector.process(level, self.working_sample);
        self.level_history.push(self.working_sample, level);
        let mut events = 0;

        let mut state = std::mem::replace(&mut self.state, FaxDecodeState::Searching);
        if self.config.immediate_decode && matches!(state, FaxDecodeState::Searching) {
            let ioc = self
                .config
                .ioc
                .expect("immediate_decode IOC validated at construction");
            let lpm = self
                .config
                .lpm
                .expect("immediate_decode LPM validated at construction");
            let mut spec = FaxSpec::standard(ioc, lpm);
            spec.modulation = self.config.modulation;
            let page_id = self.next_page_id;
            self.next_page_id = self.next_page_id.wrapping_add(1).max(1);
            let clock = FaxClockCalibrationPoint::default();
            sink.on_event(FaxDecodeEventRef::PageStarted {
                page_id,
                spec,
                clock,
            });
            events += 1;
            let line_period_samples = lpm.line_seconds() * f64::from(WORK_SAMPLE_RATE);
            state = FaxDecodeState::Receiving {
                spec,
                page_id,
                raster: FaxRasterState {
                    line_index: 0,
                    line_period_samples,
                    next_line_deadline: line_period_samples,
                    scheduled_line_samples: 0,
                    levels: Vec::with_capacity(line_period_samples.ceil() as usize),
                    line: vec![0; ioc.width() as usize],
                    basis: self.config.raster_basis,
                },
                stop_started_at: None,
                stop_hold: VecDeque::new(),
            };
        }
        self.state = match state {
            FaxDecodeState::Searching => {
                let detected_ioc = signal_present
                    .then_some(modulation_hz)
                    .flatten()
                    .and_then(detect_ioc)
                    .filter(|ioc| self.config.ioc.is_none_or(|expected| expected == *ioc));
                if let Some(ioc) = detected_ioc {
                    let dropout = work_samples(MODULATION_DROPOUT_SECONDS) as u64;
                    self.apt_candidate = Some(match self.apt_candidate {
                        Some(candidate)
                            if candidate.ioc == ioc
                                && self.working_sample.saturating_sub(candidate.last_seen)
                                    <= dropout =>
                        {
                            AptCandidate {
                                last_seen: self.working_sample,
                                ..candidate
                            }
                        }
                        _ => AptCandidate {
                            ioc,
                            first_seen: self.working_sample,
                            last_seen: self.working_sample,
                        },
                    });
                } else if self.apt_candidate.is_some_and(|candidate| {
                    self.working_sample.saturating_sub(candidate.last_seen)
                        > work_samples(MODULATION_DROPOUT_SECONDS) as u64
                }) {
                    self.apt_candidate = None;
                }

                let apt_confirmed = self.apt_candidate.filter(|candidate| {
                    self.working_sample.saturating_sub(candidate.first_seen)
                        >= work_samples(f64::from(self.config.apt_confirm_seconds)) as u64
                });
                if let Some(candidate) = apt_confirmed {
                    sink.on_event(FaxDecodeEventRef::AptDetected { ioc: candidate.ioc });
                    events += 1;
                    self.search_phasing = PhasingTracker::default();
                    FaxDecodeState::AwaitingAptEnd {
                        ioc: candidate.ioc,
                        last_transition: self
                            .modulation_detector
                            .last_transition()
                            .unwrap_or(self.working_sample),
                        started_at: self.working_sample,
                    }
                } else if let Some(ioc) = self.config.ioc {
                    if let Some((rate, fit)) =
                        self.search_phasing
                            .process(level, self.working_sample, self.config.lpm)
                    {
                        let tracker = std::mem::take(&mut self.search_phasing);
                        let started_at = tracker.first_start().unwrap_or(self.working_sample);
                        self.apt_candidate = None;
                        FaxDecodeState::Phasing {
                            ioc,
                            tracker,
                            lpm: Some(rate),
                            clock_fit: Some(fit),
                            lock_reported: false,
                            phasing_started_at: started_at,
                        }
                    } else {
                        FaxDecodeState::Searching
                    }
                } else {
                    FaxDecodeState::Searching
                }
            }
            FaxDecodeState::AwaitingAptEnd {
                ioc,
                mut last_transition,
                started_at,
            } => {
                if self.working_sample.saturating_sub(started_at)
                    > work_samples(f64::from(self.config.acquisition_timeout_seconds)) as u64
                {
                    sink.on_event(FaxDecodeEventRef::SignalRejected {
                        reason: "apt-end-timeout",
                    });
                    events += 1;
                    self.reset_fax_acquisition();
                    FaxDecodeState::Searching
                } else {
                    if let Some(transition) = self.modulation_detector.last_transition() {
                        last_transition = transition;
                    }
                    if self.working_sample.saturating_sub(last_transition)
                        >= work_samples(0.008) as u64
                    {
                        self.modulation_detector.reset();
                        self.apt_candidate = None;
                        FaxDecodeState::Phasing {
                            ioc,
                            tracker: PhasingTracker::new(level, self.working_sample),
                            lpm: self.config.lpm,
                            clock_fit: None,
                            lock_reported: false,
                            phasing_started_at: last_transition,
                        }
                    } else {
                        FaxDecodeState::AwaitingAptEnd {
                            ioc,
                            last_transition,
                            started_at,
                        }
                    }
                }
            }
            FaxDecodeState::Phasing {
                ioc,
                mut tracker,
                mut lpm,
                mut clock_fit,
                mut lock_reported,
                phasing_started_at,
            } => {
                let acquisition_expired = self.working_sample.saturating_sub(phasing_started_at)
                    > work_samples(f64::from(self.config.acquisition_timeout_seconds)) as u64;
                let signal_lost = self.working_sample.saturating_sub(self.last_signal_sample)
                    > work_samples(f64::from(self.config.signal_loss_seconds)) as u64;
                if acquisition_expired || signal_lost {
                    sink.on_event(FaxDecodeEventRef::SignalRejected {
                        reason: if signal_lost {
                            "phasing-signal-lost"
                        } else {
                            "phasing-timeout"
                        },
                    });
                    events += 1;
                    self.reset_fax_acquisition();
                    FaxDecodeState::Searching
                } else {
                    if let Some((rate, fit)) =
                        tracker.process(level, self.working_sample, self.config.lpm)
                    {
                        lpm = Some(rate);
                        clock_fit = Some(fit);
                    }
                    if tracker.take_invalidated() {
                        clock_fit = None;
                        lock_reported = false;
                    }
                    if let (Some(rate), Some(fit)) = (lpm, clock_fit) {
                        let nominal_period = rate.line_seconds() * f64::from(WORK_SAMPLE_RATE);
                        let clock = if self.config.clock_recovery == FaxClockRecoveryMode::Auto {
                            fit.calibration(rate)
                        } else {
                            FaxClockCalibrationPoint::default()
                        };
                        if !lock_reported {
                            sink.on_event(FaxDecodeEventRef::PhasingLocked {
                                ioc,
                                lpm: rate,
                                width: ioc.width(),
                                clock,
                            });
                            events += 1;
                            lock_reported = true;
                        }
                        let standard_page_start =
                            self.config.expected_phasing_seconds.map(|seconds| {
                                phasing_started_at
                                    + (f64::from(seconds) * f64::from(WORK_SAMPLE_RATE)).round()
                                        as u64
                            });
                        let pattern_ended = standard_page_start.is_none()
                            && tracker.last_start().is_some_and(|last| {
                                self.working_sample.saturating_sub(last) as f64
                                    > fit.period_samples * 1.2
                            });
                        let timed_end =
                            standard_page_start.is_some_and(|start| self.working_sample >= start);
                        if timed_end || pattern_ended {
                            let nominal_page_start = standard_page_start.unwrap_or_else(|| {
                                tracker.last_start().map_or(self.working_sample, |last| {
                                    last.saturating_add(fit.period_samples.round() as u64)
                                })
                            });
                            let page_start =
                                if self.config.clock_recovery == FaxClockRecoveryMode::Auto {
                                    fit.nearest_grid_sample(nominal_page_start)
                                } else {
                                    nominal_page_start
                                };
                            let mut spec = FaxSpec::standard(ioc, rate);
                            spec.modulation = self.config.modulation;
                            spec.phasing_seconds =
                                self.config.expected_phasing_seconds.unwrap_or_else(|| {
                                    self.working_sample.saturating_sub(phasing_started_at) as f32
                                        / WORK_SAMPLE_RATE as f32
                                });
                            let page_id = self.next_page_id;
                            self.next_page_id = self.next_page_id.wrapping_add(1).max(1);
                            sink.on_event(FaxDecodeEventRef::PageStarted {
                                page_id,
                                spec,
                                clock,
                            });
                            events += 1;
                            let line_period_samples = if self.config.raster_basis
                                == FaxRasterBasis::NominalPaper
                                || self.config.clock_recovery == FaxClockRecoveryMode::Off
                            {
                                nominal_period
                            } else {
                                fit.period_samples
                            };
                            let mut levels =
                                Vec::with_capacity(line_period_samples.ceil() as usize);
                            self.level_history.copy_from(page_start, &mut levels);
                            FaxDecodeState::Receiving {
                                spec,
                                page_id,
                                raster: FaxRasterState {
                                    line_index: 0,
                                    line_period_samples,
                                    next_line_deadline: line_period_samples,
                                    scheduled_line_samples: 0,
                                    levels,
                                    line: vec![0; ioc.width() as usize],
                                    basis: self.config.raster_basis,
                                },
                                stop_started_at: None,
                                stop_hold: VecDeque::with_capacity(work_samples(
                                    MODULATION_WINDOW_SECONDS + 0.05,
                                )),
                            }
                        } else {
                            FaxDecodeState::Phasing {
                                ioc,
                                tracker,
                                lpm,
                                clock_fit,
                                lock_reported,
                                phasing_started_at,
                            }
                        }
                    } else {
                        FaxDecodeState::Phasing {
                            ioc,
                            tracker,
                            lpm,
                            clock_fit,
                            lock_reported,
                            phasing_started_at,
                        }
                    }
                }
            }
            FaxDecodeState::Receiving {
                spec,
                page_id,
                mut raster,
                mut stop_started_at,
                mut stop_hold,
            } => {
                if self.config.immediate_decode {
                    raster.levels.push(level);
                    events += emit_available_fax_lines(
                        page_id,
                        &mut raster,
                        self.config.max_lines,
                        false,
                        false,
                        sink,
                    );
                    let maxed = self
                        .config
                        .max_lines
                        .is_some_and(|max| raster.line_index >= max);
                    if maxed {
                        sink.on_event(FaxDecodeEventRef::PageCompleted {
                            page_id,
                            lines: raster.line_index,
                            partial: false,
                        });
                        events += 1;
                        self.reset_fax_acquisition();
                        FaxDecodeState::Searching
                    } else {
                        FaxDecodeState::Receiving {
                            spec,
                            page_id,
                            raster,
                            stop_started_at,
                            stop_hold,
                        }
                    }
                } else {
                    let signal_lost = self.working_sample.saturating_sub(self.last_signal_sample)
                        > work_samples(f64::from(self.config.signal_loss_seconds)) as u64;
                    if signal_lost {
                        sink.on_event(FaxDecodeEventRef::PageCompleted {
                            page_id,
                            lines: raster.line_index,
                            partial: true,
                        });
                        events += 1;
                        self.reset_fax_acquisition();
                        FaxDecodeState::Searching
                    } else {
                        stop_hold.push_back(level);
                        let stop_detected = signal_present
                            && modulation_hz
                                .is_some_and(|frequency| (frequency - STOP_HZ as f32).abs() < 25.0);
                        if stop_detected {
                            let evidence_start = self
                                .modulation_detector
                                .control_run_start(STOP_HZ, 8)
                                .unwrap_or(self.working_sample);
                            stop_started_at.get_or_insert(evidence_start);
                        } else {
                            stop_started_at = None;
                        }
                        let stop_confirmed = stop_started_at.is_some_and(|started| {
                            self.working_sample.saturating_sub(started)
                                >= work_samples(f64::from(self.config.stop_confirm_seconds)) as u64
                        });
                        if stop_confirmed {
                            if let Some(stop_start) = stop_started_at {
                                let oldest = self
                                    .working_sample
                                    .saturating_add(1)
                                    .saturating_sub(stop_hold.len() as u64);
                                let image_prefix = stop_start.saturating_sub(oldest) as usize;
                                for _ in 0..image_prefix.min(stop_hold.len()) {
                                    if let Some(value) = stop_hold.pop_front() {
                                        raster.levels.push(value);
                                    }
                                }
                                events += emit_available_fax_lines(
                                    page_id,
                                    &mut raster,
                                    self.config.max_lines,
                                    false,
                                    true,
                                    sink,
                                );
                            }
                            sink.on_event(FaxDecodeEventRef::PageCompleted {
                                page_id,
                                lines: raster.line_index,
                                partial: false,
                            });
                            events += 1;
                            self.reset_fax_acquisition();
                            FaxDecodeState::Searching
                        } else {
                            if stop_started_at.is_none() && signal_present {
                                let guard = work_samples(MODULATION_WINDOW_SECONDS + 0.05);
                                while stop_hold.len() > guard {
                                    if let Some(value) = stop_hold.pop_front() {
                                        raster.levels.push(value);
                                    }
                                }
                                events += emit_available_fax_lines(
                                    page_id,
                                    &mut raster,
                                    self.config.max_lines,
                                    false,
                                    false,
                                    sink,
                                );
                            }
                            let maxed = self
                                .config
                                .max_lines
                                .is_some_and(|max| raster.line_index >= max);
                            if maxed {
                                sink.on_event(FaxDecodeEventRef::PageCompleted {
                                    page_id,
                                    lines: raster.line_index,
                                    partial: false,
                                });
                                events += 1;
                                self.reset_fax_acquisition();
                                FaxDecodeState::Searching
                            } else {
                                FaxDecodeState::Receiving {
                                    spec,
                                    page_id,
                                    raster,
                                    stop_started_at,
                                    stop_hold,
                                }
                            }
                        }
                    }
                }
            }
        };

        self.working_sample += 1;
        events
    }

    fn reset_fax_acquisition(&mut self) {
        self.modulation_detector.reset();
        self.apt_candidate = None;
        self.search_phasing = PhasingTracker::default();
    }
}

/// Decode a complete radiofax recording through the streaming decoder.
pub fn decode_fax(
    input: &[f32],
    input_sample_rate: u32,
    config: FaxDecoderConfig,
) -> Result<Vec<FaxDecodeEvent>> {
    let mut decoder = FaxDecoder::new(input_sample_rate, config)?;
    let mut output = Vec::new();
    decoder.process_into(input, &mut output)?;
    let mut sink = |event: FaxDecodeEventRef<'_>| output.push(event.to_owned());
    decoder.finish(&mut sink)?;
    Ok(output)
}

/// Correct a nominal-grid grayscale fax paper using clock calibration points.
///
/// The paper is treated as one continuous one-dimensional raster so a shear
/// may sample across adjacent row boundaries without introducing a seam.
pub fn correct_fax_paper(
    pixels: &[u8],
    width: u32,
    height: u32,
    start_line: u64,
    calibration: &[FaxClockCalibrationPoint],
    adjustment: FaxPaperCorrection,
) -> Result<Vec<u8>> {
    let expected = width as usize * height as usize;
    if width == 0 || height == 0 || pixels.len() != expected {
        return Err(Error::InvalidConfiguration(
            "fax paper dimensions do not match the grayscale buffer",
        ));
    }
    if !adjustment.phase_pixels.is_finite() || !adjustment.clock_ppm.is_finite() {
        return Err(Error::InvalidConfiguration(
            "fax paper correction must be finite",
        ));
    }
    if calibration.iter().any(|point| {
        !point.phase_pixels.is_finite()
            || !point.clock_ppm.is_finite()
            || !point.confidence.is_finite()
    }) {
        return Err(Error::InvalidConfiguration(
            "fax clock calibration must be finite",
        ));
    }
    let mut points = calibration.to_vec();
    points.sort_by_key(|point| (point.reference_line, point.revision));
    let mut output = vec![255; expected];
    let row_width = f64::from(width);
    for row in 0..height as usize {
        let line = start_line + row as u64;
        let mut phase = calibration_phase(&points, line, row_width);
        phase += f64::from(adjustment.phase_pixels)
            + (line.saturating_sub(start_line) as f64
                * f64::from(adjustment.clock_ppm)
                * row_width
                / 1_000_000.0);
        for column in 0..width as usize {
            let target = row * width as usize + column;
            let source = target as f64 - phase;
            if source < 0.0 || source > (pixels.len() - 1) as f64 {
                continue;
            }
            output[target] = sample_fax_gray(pixels, source);
        }
    }
    Ok(output)
}

fn calibration_phase(points: &[FaxClockCalibrationPoint], line: u64, width: f64) -> f64 {
    let Some(first) = points.first() else {
        return 0.0;
    };
    if line < first.reference_line {
        return f64::from(first.phase_pixels);
    }
    let index = points.partition_point(|point| point.reference_line <= line);
    let left = points[index.saturating_sub(1)];
    if let Some(right) = points.get(index).copied() {
        let span = right
            .reference_line
            .saturating_sub(left.reference_line)
            .max(1) as f64;
        let progress = line.saturating_sub(left.reference_line) as f64 / span;
        let progress2 = progress * progress;
        let progress3 = progress2 * progress;
        let left_slope = f64::from(left.clock_ppm) * width / 1_000_000.0;
        let right_slope = f64::from(right.clock_ppm) * width / 1_000_000.0;
        return (2.0 * progress3 - 3.0 * progress2 + 1.0) * f64::from(left.phase_pixels)
            + (progress3 - 2.0 * progress2 + progress) * span * left_slope
            + (-2.0 * progress3 + 3.0 * progress2) * f64::from(right.phase_pixels)
            + (progress3 - progress2) * span * right_slope;
    }
    f64::from(left.phase_pixels)
        + line.saturating_sub(left.reference_line) as f64 * f64::from(left.clock_ppm) * width
            / 1_000_000.0
}

fn sample_fax_gray(pixels: &[u8], source: f64) -> u8 {
    let center = source.floor() as isize;
    let fraction = source - center as f64;
    if fraction <= 1.0e-9 {
        return pixels[center as usize];
    }
    let sample = |index: isize| -> f64 {
        if index < 0 || index >= pixels.len() as isize {
            255.0
        } else {
            f64::from(pixels[index as usize])
        }
    };
    let before = sample(center - 1);
    let left = sample(center);
    let right = sample(center + 1);
    let after = sample(center + 2);
    let fraction2 = fraction * fraction;
    let fraction3 = fraction2 * fraction;
    (0.5 * (2.0 * left
        + (-before + right) * fraction
        + (2.0 * before - 5.0 * left + 4.0 * right - after) * fraction2
        + (-before + 3.0 * left - 3.0 * right + after) * fraction3))
        .round()
        .clamp(0.0, 255.0) as u8
}

fn emit_available_fax_lines(
    page_id: u64,
    raster: &mut FaxRasterState,
    max_lines: Option<u32>,
    at_end_of_input: bool,
    known_page_boundary: bool,
    sink: &mut impl FaxDecodeSink,
) -> usize {
    let mut events = 0;
    loop {
        if max_lines.is_some_and(|max| raster.line_index >= max) {
            break;
        }
        let deadline = raster
            .next_line_deadline
            .round()
            .max(raster.scheduled_line_samples as f64) as u64;
        let mut line_samples = deadline.saturating_sub(raster.scheduled_line_samples) as usize;
        if line_samples == 0 {
            break;
        }
        if raster.levels.len() < line_samples {
            let shortfall = line_samples - raster.levels.len();
            let final_bounded_line = max_lines.is_some_and(|max| raster.line_index + 1 == max);
            if (known_page_boundary || (at_end_of_input && final_bounded_line))
                && shortfall <= MAX_EOF_FILTER_DELAY_SAMPLES
            {
                line_samples = raster.levels.len();
            } else {
                break;
            }
        }
        resample_gray_line(&raster.levels[..line_samples], &mut raster.line);
        sink.on_event(FaxDecodeEventRef::LineReady {
            page_id,
            line_index: raster.line_index,
            pixels: &raster.line,
            basis: raster.basis,
        });
        events += 1;
        raster.line_index += 1;
        raster.levels.drain(..line_samples);
        raster.scheduled_line_samples = deadline;
        raster.next_line_deadline += raster.line_period_samples;
    }
    events
}

fn fitted_phasing_clock(starts: &VecDeque<u64>) -> Option<PhasingClockFit> {
    if starts.len() < 2 {
        return None;
    }
    let mut intervals: Vec<f64> = starts
        .iter()
        .zip(starts.iter().skip(1))
        .map(|(left, right)| right.saturating_sub(*left) as f64)
        .collect();
    let period = median(&mut intervals)?;
    let mut phases: Vec<f64> = starts
        .iter()
        .enumerate()
        .map(|(index, start)| *start as f64 - index as f64 * period)
        .collect();
    let phase = median(&mut phases)?;
    let mut residuals: Vec<f64> = starts
        .iter()
        .enumerate()
        .map(|(index, start)| (*start as f64 - (phase + index as f64 * period)).abs())
        .collect();
    let mad = median(&mut residuals).unwrap_or(period);
    let inlier_limit = (mad * 3.0).max(4.0);
    let inliers = starts
        .iter()
        .enumerate()
        .filter(|(index, start)| {
            (**start as f64 - (phase + *index as f64 * period)).abs() <= inlier_limit
        })
        .count();
    let span_score = ((starts.len().saturating_sub(1)) as f32 / 12.0).clamp(0.0, 1.0);
    let inlier_score = inliers as f32 / starts.len() as f32;
    let residual_score = (1.0 - (mad / period.max(1.0) * 500.0) as f32).clamp(0.0, 1.0);
    Some(PhasingClockFit {
        period_samples: period,
        phase_sample: phase,
        confidence: (span_score * 0.35 + inlier_score * 0.45 + residual_score * 0.20)
            .clamp(0.0, 1.0),
    })
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    })
}

fn infer_lpm_from_period(period_samples: f64) -> Option<FaxLpm> {
    let candidates = [
        FaxLpm::LPM_60,
        FaxLpm::LPM_90,
        FaxLpm::LPM_120,
        FaxLpm::LPM_240,
    ];
    candidates
        .into_iter()
        .min_by(|a, b| {
            let a_error = (a.line_seconds() * f64::from(WORK_SAMPLE_RATE) - period_samples).abs();
            let b_error = (b.line_seconds() * f64::from(WORK_SAMPLE_RATE) - period_samples).abs();
            a_error.total_cmp(&b_error)
        })
        .filter(|candidate| {
            let expected = candidate.line_seconds() * f64::from(WORK_SAMPLE_RATE);
            (expected - period_samples).abs() <= expected * 0.04
        })
}

fn detect_ioc(frequency_hz: f32) -> Option<FaxIoc> {
    if (frequency_hz - START_IOC_576_HZ as f32).abs() <= 25.0 {
        Some(FaxIoc::Ioc576)
    } else if (frequency_hz - START_IOC_288_HZ as f32).abs() <= 40.0 {
        Some(FaxIoc::Ioc288)
    } else {
        None
    }
}

fn resample_gray_line(input: &[u8], output: &mut [u8]) {
    let output_len = output.len();
    for (index, target) in output.iter_mut().enumerate() {
        let source = index * input.len() / output_len;
        *target = input[source.min(input.len() - 1)];
    }
}

#[inline]
fn frequency_to_gray(frequency_hz: f32, modulation: FaxModulation) -> u8 {
    let FaxModulation::FmSubcarrier {
        center_hz,
        deviation_hz,
        polarity,
    } = modulation
    else {
        return 0;
    };
    let normalized =
        ((frequency_hz - (center_hz - deviation_hz)) / (2.0 * deviation_hz)).clamp(0.0, 1.0);
    let normalized = match polarity {
        FaxPolarity::Normal => normalized,
        FaxPolarity::Inverted => 1.0 - normalized,
    };
    (normalized * 255.0).round() as u8
}

fn validate_rate(sample_rate: u32) -> Result<()> {
    if !(8_000..=384_000).contains(&sample_rate) {
        return Err(Error::InvalidSampleRate(sample_rate));
    }
    Ok(())
}

#[cfg(test)]
fn validate_spec(spec: FaxSpec, sample_rate: u32) -> Result<()> {
    if !valid_bounded_seconds(spec.phasing_seconds, 0.001, 600.0) {
        return Err(Error::InvalidConfiguration(
            "radiofax phasing_seconds must be finite and in 0.001..=600",
        ));
    }
    if !valid_nonnegative_seconds(spec.start_seconds, 600.0)
        || !valid_nonnegative_seconds(spec.stop_seconds, 600.0)
        || !valid_nonnegative_seconds(spec.trailing_black_seconds, 600.0)
    {
        return Err(Error::InvalidConfiguration(
            "radiofax APT durations must be finite and in 0.0..=600",
        ));
    }
    validate_modulation(spec.modulation, sample_rate)
}

fn validate_modulation(modulation: FaxModulation, sample_rate: u32) -> Result<()> {
    let nyquist = sample_rate as f32 * 0.5;
    match modulation {
        FaxModulation::FmSubcarrier {
            center_hz,
            deviation_hz,
            ..
        } if !center_hz.is_finite()
            || !deviation_hz.is_finite()
            || center_hz <= deviation_hz
            || deviation_hz <= 0.0
            || center_hz + deviation_hz >= nyquist =>
        {
            return Err(Error::InvalidConfiguration(
                "FM center/deviation must be finite, positive and below Nyquist",
            ));
        }
        FaxModulation::AmSubcarrier {
            carrier_hz,
            black_level,
            white_level,
        } if !carrier_hz.is_finite()
            || carrier_hz <= 0.0
            || carrier_hz >= nyquist
            || !black_level.is_finite()
            || !white_level.is_finite()
            || !(0.0..=1.0).contains(&black_level)
            || !(0.0..=1.0).contains(&white_level)
            || (black_level - white_level).abs() < f32::EPSILON =>
        {
            return Err(Error::InvalidConfiguration(
                "AM carrier/black/white levels are invalid",
            ));
        }
        _ => {}
    }
    Ok(())
}

fn valid_bounded_seconds(value: f32, minimum: f32, maximum: f32) -> bool {
    value.is_finite() && (minimum..=maximum).contains(&value)
}

fn valid_nonnegative_seconds(value: f32, maximum: f32) -> bool {
    value.is_finite() && (0.0..=maximum).contains(&value)
}

#[inline]
fn seconds_to_samples(seconds: f64, sample_rate: u32) -> usize {
    (seconds * f64::from(sample_rate)).round() as usize
}

#[inline]
fn work_samples(seconds: f64) -> usize {
    seconds_to_samples(seconds, WORK_SAMPLE_RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fax_types_are_thread_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_copy<T: Copy>() {}
        assert_send_sync::<FaxEncoder>();
        assert_send_sync::<FaxDecoder>();
        assert_send_sync::<FaxSpec>();
        assert_copy::<FaxSpec>();
    }

    #[test]
    fn ioc_widths_are_conventional() {
        assert_eq!(FaxIoc::Ioc288.width(), 905);
        assert_eq!(FaxIoc::Ioc576.width(), 1810);
    }

    #[test]
    fn standard_phasing_is_thirty_seconds_at_each_lpm() {
        for (lpm, lines) in [
            (FaxLpm::LPM_60, 30),
            (FaxLpm::LPM_90, 45),
            (FaxLpm::LPM_120, 60),
            (FaxLpm::LPM_180, 90),
            (FaxLpm::LPM_240, 120),
        ] {
            assert_eq!(
                FaxSpec::standard(FaxIoc::Ioc576, lpm).phasing_line_count(),
                lines
            );
        }
    }

    #[test]
    fn fax_timeline_has_bounded_rounding_error() {
        let spec = FaxSpec::standard(FaxIoc::Ioc288, FaxLpm::LPM_120);
        let image = GrayImage::new(spec.width(), 2, vec![96; spec.width() as usize * 2]).unwrap();
        let mut encoder =
            FaxEncoder::new(image, spec, 44_100, FaxEncodeOptions::default()).unwrap();
        let mut chunk = [0.0_f32; 8192];
        while !encoder.is_finished() {
            encoder.read_samples(&mut chunk);
        }
        let expected_seconds = f64::from(spec.start_seconds)
            + f64::from(spec.phasing_seconds)
            + 2.0 * spec.lpm.line_seconds()
            + f64::from(spec.stop_seconds)
            + f64::from(spec.trailing_black_seconds);
        let expected = (expected_seconds * 44_100.0).round() as i64;
        assert!((encoder.samples_emitted() as i64 - expected).abs() <= 1);
    }

    #[test]
    fn apt_video_alternation_is_detectable() {
        let ioc = FaxIoc::Ioc288;
        let spec = FaxSpec::standard(ioc, FaxLpm::LPM_240);
        let image = GrayImage::new(ioc.width(), 1, vec![0; ioc.width() as usize]).unwrap();
        let mut encoder =
            FaxEncoder::new(image, spec, WORK_SAMPLE_RATE, FaxEncodeOptions::default()).unwrap();
        let mut demodulator = FrequencyDemodulator::default();
        let mut detector = ModulationDetector::default();
        let mut chunk = [0.0_f32; 127];
        let mut sample_index = 0_u64;
        let mut estimates = Vec::new();
        while sample_index < u64::from(WORK_SAMPLE_RATE) / 2 {
            let count = encoder.read_samples(&mut chunk);
            for &sample in &chunk[..count] {
                let frequency = demodulator.process(sample, sample_index);
                let level = frequency_to_gray(frequency, FaxModulation::WMO_FM);
                if let Some(estimate) = detector.process(level, sample_index) {
                    estimates.push(estimate);
                }
                sample_index += 1;
            }
        }
        let energy = detector
            .window
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>();
        let ratios: Vec<_> = [300.0_f32, 450.0, 675.0]
            .into_iter()
            .map(|frequency| {
                (
                    frequency,
                    goertzel_power(&detector.window, frequency)
                        / (energy * detector.window.len() as f64),
                )
            })
            .collect();
        assert!(
            estimates
                .iter()
                .copied()
                .any(|estimate| detect_ioc(estimate) == Some(ioc)),
            "APT estimates were {estimates:?}; normalized powers {ratios:?}"
        );
    }

    #[test]
    fn custom_inverted_fm_levels_demodulate_to_video_polarity() {
        let modulation = FaxModulation::FmSubcarrier {
            center_hz: 2_000.0,
            deviation_hz: 300.0,
            polarity: FaxPolarity::Inverted,
        };
        for (tone, expected) in [(2_300.0, 0_u8), (1_700.0, 255_u8)] {
            let mut oscillator = Oscillator::default();
            let mut pcm = vec![0.0; 4_000];
            oscillator.fill(&mut pcm, tone, WORK_SAMPLE_RATE, 0.5);
            let mut demodulator = FrequencyDemodulator::new(2_000.0);
            let mut levels = Vec::new();
            for (index, sample) in pcm.into_iter().enumerate() {
                let frequency = demodulator.process(sample, index as u64);
                levels.push(frequency_to_gray(frequency, modulation));
            }
            let actual = levels[2_000..]
                .iter()
                .map(|value| u64::from(*value))
                .sum::<u64>()
                / 2_000;
            assert!(
                actual.abs_diff(u64::from(expected)) < 16,
                "tone {tone} decoded to {actual}, expected {expected}"
            );
        }
    }

    #[test]
    fn carrier_coherence_separates_a_tone_from_wideband_noise() {
        let mut oscillator = Oscillator::default();
        let mut tone = vec![0.0; 12_000];
        oscillator.fill(&mut tone, 1_900.0, WORK_SAMPLE_RATE, 0.5);
        let mut state = 7_u64;
        let mut noise = vec![0.0_f32; 12_000];
        for sample in &mut noise {
            let mut gaussianish = 0.0_f32;
            for _ in 0..12 {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                gaussianish += ((state >> 40) as f32) / ((1_u32 << 24) as f32);
            }
            *sample = (gaussianish - 6.0) * 0.12;
        }
        let mut ratios = Vec::new();
        for pcm in [tone, noise] {
            let mut demodulator = FrequencyDemodulator::default();
            let mut raw = 0.0_f32;
            for (index, sample) in pcm.into_iter().enumerate() {
                raw += (sample.abs() - raw) * 0.02;
                demodulator.process(sample, index as u64);
            }
            ratios.push(demodulator.carrier_level() / raw);
        }
        assert!(ratios[0] > 1.4, "tone coherence was {}", ratios[0]);
        assert!(ratios[1] < 1.1, "noise coherence was {}", ratios[1]);
    }

    #[test]
    fn fax_numeric_validation_rejects_non_finite_and_aliased_parameters() {
        let ioc = FaxIoc::Ioc288;
        let image = GrayImage::new(ioc.width(), 1, vec![0; ioc.width() as usize]).unwrap();
        let mut infinite = FaxSpec::standard(ioc, FaxLpm::LPM_120);
        infinite.stop_seconds = f32::INFINITY;
        assert!(
            FaxEncoder::new(image.clone(), infinite, 12_000, FaxEncodeOptions::default()).is_err()
        );

        let mut aliased = FaxSpec::standard(ioc, FaxLpm::LPM_120);
        aliased.modulation = FaxModulation::FmSubcarrier {
            center_hz: 5_800.0,
            deviation_hz: 300.0,
            polarity: FaxPolarity::Normal,
        };
        assert!(FaxEncoder::new(image, aliased, 48_000, FaxEncodeOptions::default()).is_err());

        let config = FaxDecoderConfig {
            modulation: FaxModulation::AmSubcarrier {
                carrier_hz: f32::NAN,
                black_level: 1.0,
                white_level: 0.1,
            },
            ..FaxDecoderConfig::default()
        };
        assert!(FaxDecoder::new(12_000, config).is_err());
    }

    #[test]
    fn fax_decoder_rejects_non_finite_pcm_without_events() {
        let mut decoder = FaxDecoder::new(12_000, FaxDecoderConfig::default()).unwrap();
        let mut events = Vec::new();
        assert_eq!(
            decoder.process_into(&[f32::INFINITY], &mut events),
            Err(Error::NonFiniteSample)
        );
        assert!(events.is_empty());
    }

    #[test]
    fn immediate_decode_starts_and_emits_rows_without_apt_or_signal_gate() {
        let mut decoder = FaxDecoder::new(
            WORK_SAMPLE_RATE,
            FaxDecoderConfig {
                immediate_decode: true,
                ioc: Some(FaxIoc::Ioc576),
                lpm: Some(FaxLpm::LPM_120),
                minimum_signal_level: 1.0,
                minimum_carrier_coherence: 2.0,
                ..FaxDecoderConfig::default()
            },
        )
        .unwrap();
        let mut events = Vec::new();
        let samples = vec![0.0; (FaxLpm::LPM_120.line_seconds() * 12_000.0) as usize + 2];

        decoder.process_into(&samples, &mut events).unwrap();

        assert!(matches!(
            events.first(),
            Some(FaxDecodeEvent::PageStarted { spec, .. })
                if spec.ioc == FaxIoc::Ioc576 && spec.lpm == FaxLpm::LPM_120
        ));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, FaxDecodeEvent::LineReady { line_index: 0, .. }))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, FaxDecodeEvent::PageCompleted { .. }))
        );
    }

    #[test]
    fn immediate_fax_decode_requires_fixed_ioc_and_lpm() {
        for config in [
            FaxDecoderConfig {
                immediate_decode: true,
                ioc: Some(FaxIoc::Ioc576),
                ..FaxDecoderConfig::default()
            },
            FaxDecoderConfig {
                immediate_decode: true,
                lpm: Some(FaxLpm::LPM_120),
                ..FaxDecoderConfig::default()
            },
        ] {
            assert!(FaxDecoder::new(WORK_SAMPLE_RATE, config).is_err());
        }
    }

    #[test]
    fn robust_phasing_fit_rejects_real_radio_edge_jitter() {
        let starts = VecDeque::from([
            1_757_886, 1_763_872, 1_769_857, 1_775_894, 1_781_891, 1_787_891, 1_793_891, 1_799_891,
        ]);
        let old_ols = {
            let first = *starts.front().unwrap() as f64;
            let mean_x = (starts.len() - 1) as f64 * 0.5;
            let mean_y = starts
                .iter()
                .map(|start| *start as f64 - first)
                .sum::<f64>()
                / starts.len() as f64;
            let mut covariance = 0.0;
            let mut variance = 0.0;
            for (index, start) in starts.iter().enumerate() {
                let x = index as f64 - mean_x;
                covariance += x * (*start as f64 - first - mean_y);
                variance += x * x;
            }
            covariance / variance
        };
        let fit = fitted_phasing_clock(&starts).unwrap();
        assert!((old_ols - 6_002.726_190_476).abs() < 1.0e-6);
        assert!((fit.period_samples - 6_000.0).abs() <= 0.25, "fit={fit:?}");
    }

    #[test]
    fn paper_correction_applies_phase_across_row_boundaries() {
        let input: Vec<u8> = (0..12).collect();
        let point = FaxClockCalibrationPoint {
            revision: 1,
            reference_line: 0,
            phase_pixels: 1.0,
            clock_ppm: 0.0,
            confidence: 1.0,
            source: FaxClockSource::Manual,
            status: FaxClockStatus::Locked,
        };
        let corrected =
            correct_fax_paper(&input, 4, 3, 0, &[point], FaxPaperCorrection::default()).unwrap();
        assert_eq!(corrected, vec![255, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn paper_correction_extrapolates_clock_ppm() {
        let input: Vec<u8> = (0..16).collect();
        let point = FaxClockCalibrationPoint {
            revision: 1,
            reference_line: 0,
            phase_pixels: 0.0,
            clock_ppm: 250_000.0,
            confidence: 1.0,
            source: FaxClockSource::Phasing,
            status: FaxClockStatus::Locked,
        };
        let corrected =
            correct_fax_paper(&input, 4, 4, 0, &[point], FaxPaperCorrection::default()).unwrap();
        assert_eq!(&corrected[4..8], &[3, 4, 5, 6]);
        assert_eq!(&corrected[8..12], &[6, 7, 8, 9]);
    }
}
