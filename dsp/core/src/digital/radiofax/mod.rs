//! HF radiofax (WMO WEFAX) production adapter.
//!
//! ```text
//!   12 kHz complex IQ -> 300–2700 Hz complex band-pass -> linear f32 PCM
//!   -> FaxPaperDecoder -> corrected Gray8 rows
//! ```
//!
//! The vendored paper decoder owns APT, phasing, IOC/LPM detection and the nominal
//! raster. This module keeps the current page, applies paper correction once, and
//! publishes at most 64-row Gray8 spans. It does not use the SSTV AGC or silence end.

#[allow(dead_code, unused_variables)]
pub mod vendor;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::ddc::fir::{design_complex_bandpass, ComplexBandFilter};
use crate::plugin::{
    DigitalDemodulator, RadiofaxConfigView, RadiofaxControl, RadiofaxImageMeta, RadiofaxImageSpan,
    RadiofaxState,
};
use vendor::fax::{
    correct_fax_paper, FaxClockCalibrationPoint, FaxClockRecoveryMode, FaxClockSource,
    FaxClockStatus, FaxIoc, FaxLpm, FaxModulation, FaxPaperCorrection, FaxPolarity, FaxRasterBasis,
    FaxSpec,
};
use vendor::{FaxPaperConfig, FaxPaperDecoder, FaxPaperEvent, FaxPaperEventRef, FaxPaperMode};
use vendor::PaperBoundaryKind;

const DECODE_RATE_HZ: f64 = 12_000.0;
const AUDIO_LO_HZ: f64 = 300.0;
const AUDIO_HI_HZ: f64 = 2_700.0;
const MAX_SEGMENT_LINES: u32 = 2_400;
const SPAN_ROWS: u32 = 64;
const PHASE_LIMIT: f32 = 4_096.0;
const PPM_LIMIT: f32 = 5_000.0;

struct RawLine {
    pixels: Vec<u8>,
    basis: FaxRasterBasis,
}

struct Segment {
    transmission_id: u64,
    boundary_id: u64,
    serial: u64,
    width: u32,
    ioc: u16,
    lpm: u16,
    trusted: bool,
    lines: Vec<RawLine>,
    controls: Vec<FaxClockCalibrationPoint>,
    corrected: Vec<u8>,
    revision: u64,
    dirty_from: u32,
    complete: bool,
    partial: bool,
    closed: bool,
}

pub struct RadiofaxPlugin {
    band: ComplexBandFilter,
    filtered: Vec<f64>,
    pcm: Vec<f32>,
    decoder: FaxPaperDecoder,
    config: RadiofaxConfigView,
    segments: Vec<Segment>,
    next_serial: u64,
    next_transmission: u64,
    stage: &'static str,
    signal_level: f32,
    carrier_coherence: f32,
    observed_ioc: Option<u16>,
    observed_lpm: Option<u16>,
    clock_status: FaxClockStatus,
    clock_source: FaxClockSource,
    clock_phase: f32,
    clock_ppm: f32,
    clock_confidence: f32,
    reason: Option<String>,
    elapsed_samples: u64,
    active_boundary: u64,
    apt_seen: bool,
    phasing_seen: bool,
    page_started: Option<u64>,
    page_complete: bool,
    page_partial: bool,
    fatal: Option<String>,
}

impl RadiofaxPlugin {
    pub fn new(rate: f64) -> Self {
        let config = RadiofaxConfigView {
            acquisition: "auto".into(),
            ioc: "auto".into(),
            lpm: "auto".into(),
            modulation: "fm".into(),
            polarity: "normal".into(),
            clock_recovery: "auto".into(),
            phase_pixels: 0.0,
            clock_ppm: 0.0,
        };
        Self {
            band: band_for(rate),
            filtered: Vec::new(),
            pcm: Vec::new(),
            decoder: paper_decoder(&config).expect("default radiofax configuration"),
            config,
            segments: Vec::new(),
            next_serial: 1,
            next_transmission: 1,
            stage: "searching",
            signal_level: 0.0,
            carrier_coherence: 0.0,
            observed_ioc: None,
            observed_lpm: None,
            clock_status: FaxClockStatus::Nominal,
            clock_source: FaxClockSource::Nominal,
            clock_phase: 0.0,
            clock_ppm: 0.0,
            clock_confidence: 0.0,
            reason: None,
            elapsed_samples: 0,
            active_boundary: 0,
            apt_seen: false,
            phasing_seen: false,
            page_started: None,
            page_complete: false,
            page_partial: false,
            fatal: None,
        }
    }

    fn active_mut(&mut self) -> Option<&mut Segment> {
        self.segments.iter_mut().rev().find(|segment| !segment.closed)
    }

    fn begin_segment(&mut self, spec: FaxSpec, boundary: u64, trusted: bool, transmission: Option<u64>) {
        let transmission_id = transmission.unwrap_or_else(|| {
            let id = self.next_transmission;
            self.next_transmission += 1;
            id
        });
        let serial = self.next_serial;
        self.next_serial += 1;
        self.active_boundary = boundary;
        self.segments.push(Segment {
            transmission_id,
            boundary_id: boundary,
            serial,
            width: spec.width(),
            ioc: spec.ioc.value(),
            lpm: spec.lpm.get(),
            trusted,
            lines: Vec::new(),
            controls: Vec::new(),
            corrected: Vec::new(),
            revision: 1,
            dirty_from: 0,
            complete: false,
            partial: false,
            closed: false,
        });
        self.page_started = Some(self.elapsed_samples);
        self.page_complete = false;
        self.page_partial = false;
        self.stage = if trusted { "receiving" } else { "fallback" };
    }

    fn close_active(&mut self, partial: bool, complete: bool) {
        if let Some(segment) = self.active_mut() {
            if !segment.lines.is_empty() || complete {
                segment.partial = partial;
                segment.complete = complete;
                segment.closed = true;
                segment.dirty_from = segment.dirty_from.min(segment.lines.len() as u32);
            } else {
                segment.closed = true;
            }
        }
        self.page_complete = complete;
        self.page_partial = partial;
        if complete {
            self.stage = "complete";
        } else if partial {
            self.stage = "partial";
        }
    }

    fn absorb(&mut self, events: Vec<FaxPaperEvent>) -> Result<(), String> {
        for event in events {
            match event {
                FaxPaperEvent::PaperStarted { spec, .. } => {
                    if self.segments.is_empty() {
                        self.begin_segment(spec, 1, false, None);
                        self.stage = "searching";
                    }
                }
                FaxPaperEvent::Boundary { boundary_id, spec, kind, trusted, .. } => {
                    self.apt_seen = false;
                    self.phasing_seen = false;
                    match kind {
                        PaperBoundaryKind::Initial => {
                            if self.active_mut().is_none() {
                                self.begin_segment(spec, boundary_id, false, None);
                                self.stage = "searching";
                            }
                        }
                        PaperBoundaryKind::AptPhasing => {
                            self.close_active(self.active_has_lines(), false);
                            self.begin_segment(spec, boundary_id, trusted, None);
                            self.phasing_seen = true;
                            self.stage = "receiving";
                        }
                        PaperBoundaryKind::ProtocolEnd => {
                            self.close_active(false, true);
                            self.reason = None;
                        }
                        PaperBoundaryKind::Discontinuity | PaperBoundaryKind::Reset => {
                            let had = self.active_has_lines();
                            self.close_active(had, false);
                            if had {
                                self.reason = Some(if kind == PaperBoundaryKind::Reset {
                                    "new_page"
                                } else {
                                    "signal_lost"
                                }.into());
                            }
                            self.begin_segment(spec, boundary_id, false, None);
                            self.stage = if had { "partial" } else { "searching" };
                        }
                        PaperBoundaryKind::Vis | PaperBoundaryKind::SyncTiming => {}
                    }
                }
                FaxPaperEvent::AptDetected { ioc } => {
                    self.apt_seen = true;
                    self.observed_ioc = Some(ioc.value());
                    if self.stage == "searching" || self.stage == "fallback" {
                        self.stage = "apt";
                    }
                }
                FaxPaperEvent::ClockCalibration { boundary_id, calibration, .. } => {
                    self.note_clock(calibration);
                    if let Some(segment) = self.segments.iter_mut().rev().find(|segment| {
                        segment.boundary_id == boundary_id && !segment.closed
                    }) {
                        segment.controls.push(calibration);
                        segment.dirty_from = 0;
                    }
                    if calibration.source == FaxClockSource::Phasing {
                        self.phasing_seen = true;
                        if !matches!(self.stage, "receiving" | "complete" | "partial" | "degraded") {
                            self.stage = "phasing";
                        }
                    }
                    if calibration.status == FaxClockStatus::Degraded && self.stage == "receiving" {
                        self.stage = "degraded";
                    }
                }
                FaxPaperEvent::LineReady { boundary_id, spec, pixels, basis, .. } => {
                    if self.signal_level < 0.002 { continue; }
                    self.store_line(boundary_id, spec, pixels, basis)?;
                }
                FaxPaperEvent::TransmissionCompleted { .. } => {
                    self.close_active(false, true);
                    self.reason = None;
                }
                FaxPaperEvent::ProtocolObserved { spec, .. } => {
                    self.observed_ioc = Some(spec.ioc.value());
                    self.observed_lpm = Some(spec.lpm.get());
                    self.reason = Some("protocol_mismatch".into());
                }
                FaxPaperEvent::SignalRejected { reason } => {
                    self.reason = Some(reason.to_string());
                    if self.stage == "apt" || self.stage == "phasing" {
                        self.stage = if self.active_has_lines() { "fallback" } else { "searching" };
                    }
                }
            }
        }
        self.correct_open_segments()
    }

    fn active_has_lines(&self) -> bool {
        self.segments.iter().rev().find(|segment| !segment.closed)
            .is_some_and(|segment| !segment.lines.is_empty())
    }

    fn note_clock(&mut self, point: FaxClockCalibrationPoint) {
        self.clock_status = point.status;
        self.clock_source = point.source;
        self.clock_phase = point.phase_pixels;
        self.clock_ppm = point.clock_ppm;
        self.clock_confidence = point.confidence;
    }

    fn store_line(&mut self, boundary: u64, spec: FaxSpec, pixels: Vec<u8>, basis: FaxRasterBasis) -> Result<(), String> {
        if pixels.len() != spec.width() as usize {
            return Err("radiofax row width does not match IOC".into());
        }
        if self.signal_level < 0.002 && self.carrier_coherence < 0.2 {
            return Ok(());
        }
        let needs_new = self.segments.iter().rev().find(|segment| segment.boundary_id == boundary && !segment.closed)
            .is_none_or(|segment| segment.lines.len() as u32 >= MAX_SEGMENT_LINES || segment.width != spec.width());
        if needs_new {
            let (transmission, trusted) = self.segments.iter().rev()
                .find(|segment| segment.boundary_id == boundary)
                .map(|segment| (segment.transmission_id, segment.trusted))
                .map_or((None, false), |(id, trusted)| (Some(id), trusted));
            if let Some(segment) = self.segments.iter_mut().rev().find(|segment| !segment.closed) {
                segment.closed = true;
                segment.partial = true;
                segment.dirty_from = segment.dirty_from.min(segment.lines.len() as u32);
            }
            self.begin_segment(spec, boundary, trusted, transmission);
            if transmission.is_some() {
                self.reason = Some("segment_limit".into());
            }
        }
        let segment = self.segments.iter_mut().rev().find(|segment| segment.boundary_id == boundary && !segment.closed)
            .ok_or("radiofax row has no open segment")?;
        segment.lines.push(RawLine { pixels, basis });
        self.observed_ioc = Some(spec.ioc.value());
        self.observed_lpm = Some(spec.lpm.get());
        if self.stage != "degraded" {
            self.stage = if segment.trusted || self.phasing_seen { "receiving" } else { "fallback" };
        }
        Ok(())
    }

    fn correct_open_segments(&mut self) -> Result<(), String> {
        let adjustment = FaxPaperCorrection {
            phase_pixels: self.config.phase_pixels,
            clock_ppm: self.config.clock_ppm,
        };
        for segment in &mut self.segments {
            if segment.dirty_from as usize >= segment.lines.len() { continue; }
            let width = segment.width as usize;
            let height = segment.lines.len();
            let mut raw = Vec::with_capacity(width * height);
            let mut nominal = true;
            for line in &segment.lines {
                if line.basis != FaxRasterBasis::NominalPaper { nominal = false; }
                raw.extend_from_slice(&line.pixels);
            }
            let corrected = if nominal && (!segment.controls.is_empty()
                || adjustment.phase_pixels != 0.0 || adjustment.clock_ppm != 0.0)
            {
                correct_fax_paper(&raw, segment.width, height as u32, 0, &segment.controls, adjustment)
                    .map_err(|error| error.to_string())?
            } else {
                raw
            };
            if corrected != segment.corrected {
                if segment.dirty_from > 0 && segment.corrected.len() == corrected.len() {
                    let start = segment.dirty_from as usize * width;
                    if corrected[..start] != segment.corrected[..start] {
                        segment.dirty_from = 0;
                        segment.revision += 1;
                    }
                } else if !segment.corrected.is_empty() {
                    segment.revision += 1;
                }
                segment.corrected = corrected;
            }
        }
        Ok(())
    }

    fn update_level(&mut self) {
        if self.pcm.is_empty() { return; }
        let mut energy = 0.0_f64;
        let step = core::f64::consts::TAU * 1_900.0 / DECODE_RATE_HZ;
        let mut i_acc = 0.0_f64;
        let mut q_acc = 0.0_f64;
        for (index, sample) in self.pcm.iter().copied().enumerate() {
            let value = f64::from(sample);
            energy += value * value;
            let angle = step * index as f64;
            i_acc += value * angle.cos();
            q_acc += value * angle.sin();
        }
        let rms = (energy / self.pcm.len() as f64).sqrt();
        let tone = (i_acc * i_acc + q_acc * q_acc).sqrt() / self.pcm.len() as f64;
        let coherence = (tone / rms.max(1.0e-9)) as f32;
        self.signal_level = self.signal_level * 0.8 + rms as f32 * 0.2;
        self.carrier_coherence = self.carrier_coherence * 0.8 + coherence * 0.2;
    }
}

fn band_for(rate: f64) -> ComplexBandFilter {
    let (re, im) = design_complex_bandpass(rate, AUDIO_LO_HZ, AUDIO_HI_HZ, 129);
    ComplexBandFilter::new(re, im)
}

fn paper_decoder(config: &RadiofaxConfigView) -> Result<FaxPaperDecoder, String> {
    let spec = configured_spec(config)?;
    let mode = if config.acquisition == "manual" {
        FaxPaperMode::Manual { spec }
    } else {
        FaxPaperMode::Auto { fallback: spec }
    };
    let paper = FaxPaperConfig {
        mode,
        clock_recovery: if config.clock_recovery == "off" {
            FaxClockRecoveryMode::Off
        } else {
            FaxClockRecoveryMode::Auto
        },
        ..FaxPaperConfig::default()
    };
    FaxPaperDecoder::new(DECODE_RATE_HZ as u32, paper).map_err(|error| error.to_string())
}

fn configured_spec(config: &RadiofaxConfigView) -> Result<FaxSpec, String> {
    validate_radiofax(config)?;
    let ioc = match config.ioc.as_str() {
        "288" => FaxIoc::Ioc288,
        _ => FaxIoc::Ioc576,
    };
    let lpm = match config.lpm.as_str() {
        "60" => FaxLpm::LPM_60,
        "90" => FaxLpm::LPM_90,
        "180" => FaxLpm::LPM_180,
        "240" => FaxLpm::LPM_240,
        _ => FaxLpm::LPM_120,
    };
    let polarity = if config.polarity == "inverted" { FaxPolarity::Inverted } else { FaxPolarity::Normal };
    let modulation = if config.modulation == "am" {
        FaxModulation::AmSubcarrier { carrier_hz: 1_900.0, black_level: 0.0, white_level: 1.0 }
    } else {
        FaxModulation::FmSubcarrier { center_hz: 1_900.0, deviation_hz: 400.0, polarity }
    };
    let mut spec = FaxSpec::standard(ioc, lpm);
    spec.modulation = match modulation {
        FaxModulation::AmSubcarrier { carrier_hz, black_level, white_level } => {
            let (black_level, white_level) = if polarity == FaxPolarity::Inverted {
                (white_level, black_level)
            } else {
                (black_level, white_level)
            };
            FaxModulation::AmSubcarrier { carrier_hz, black_level, white_level }
        }
        other => other,
    };
    Ok(spec)
}

pub fn validate_radiofax(config: &RadiofaxConfigView) -> Result<(), String> {
    if !matches!(config.acquisition.as_str(), "auto" | "manual")
        || !matches!(config.ioc.as_str(), "auto" | "288" | "576")
        || !matches!(config.lpm.as_str(), "auto" | "60" | "90" | "120" | "180" | "240")
        || !matches!(config.modulation.as_str(), "fm" | "am")
        || !matches!(config.polarity.as_str(), "normal" | "inverted")
        || !matches!(config.clock_recovery.as_str(), "auto" | "off")
        || !config.phase_pixels.is_finite() || !(-PHASE_LIMIT..=PHASE_LIMIT).contains(&config.phase_pixels)
        || !config.clock_ppm.is_finite() || !(-PPM_LIMIT..=PPM_LIMIT).contains(&config.clock_ppm)
    {
        return Err("invalid radiofax configuration".into());
    }
    if config.acquisition == "manual" && (config.ioc == "auto" || config.lpm == "auto") {
        return Err("manual radiofax requires an IOC and LPM".into());
    }
    if config.acquisition == "auto" && config.lpm == "180" {
        return Err("180 LPM is manual-only".into());
    }
    Ok(())
}

fn protocol_changed(left: &RadiofaxConfigView, right: &RadiofaxConfigView) -> bool {
    left.acquisition != right.acquisition || left.ioc != right.ioc || left.lpm != right.lpm
        || left.modulation != right.modulation || left.polarity != right.polarity
        || left.clock_recovery != right.clock_recovery
}

fn clock_name(status: FaxClockStatus) -> &'static str {
    match status {
        FaxClockStatus::Nominal => "nominal",
        FaxClockStatus::Acquiring => "acquiring",
        FaxClockStatus::Locked => "locked",
        FaxClockStatus::Tracking => "tracking",
        FaxClockStatus::Degraded => "degraded",
    }
}

fn source_name(source: FaxClockSource) -> &'static str {
    match source {
        FaxClockSource::Nominal => "nominal",
        FaxClockSource::Phasing => "phasing",
        FaxClockSource::ImageContent => "image_content",
        FaxClockSource::Manual => "manual",
    }
}

fn segment_index(segments: &[Segment], segment: &Segment) -> u32 {
    segments.iter().filter(|candidate| candidate.transmission_id == segment.transmission_id
        && candidate.serial <= segment.serial).count().saturating_sub(1) as u32
}

impl DigitalDemodulator for RadiofaxPlugin {
    fn id(&self) -> &'static str { "radiofax" }

    fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
        if self.fatal.is_some() { return Vec::new(); }
        let n = iq.len() & !1;
        if n == 0 { return Vec::new(); }
        if iq[..n].iter().any(|sample| !sample.is_finite()) {
            self.fatal = Some("radiofax IQ samples must be finite".into());
            return Vec::new();
        }
        let input: Vec<f64> = iq[..n].iter().map(|sample| f64::from(*sample)).collect();
        self.band.process_complex_into(&input, &mut self.filtered);
        self.pcm.clear();
        self.pcm.extend(self.filtered.iter().step_by(2).map(|sample| *sample as f32));
        if self.pcm.iter().any(|sample| !sample.is_finite()) {
            self.fatal = Some("radiofax band-pass produced a non-finite sample".into());
            return Vec::new();
        }
        self.elapsed_samples += self.pcm.len() as u64;
        self.update_level();
        let mut events = Vec::new();
        if let Err(error) = self.decoder.process_into(&self.pcm, &mut events).map_err(|error| error.to_string()).and_then(|_| self.absorb(events)) {
            self.fatal = Some(error);
        }
        Vec::new()
    }

    fn reset(&mut self) {
        let rate = DECODE_RATE_HZ;
        let config = self.config.clone();
        *self = Self::new(rate);
        self.config = config.clone();
        self.decoder = paper_decoder(&config).unwrap_or_else(|_| FaxPaperDecoder::new(
            DECODE_RATE_HZ as u32, FaxPaperConfig::default()).expect("default fax decoder"));
    }

    fn retune(&mut self) {
        if let Err(error) = RadiofaxControl::mark_discontinuity(self) { self.fatal = Some(error); }
    }


    fn audio_level(&self) -> f32 { self.signal_level }

    fn buffered_input(&self) -> usize { self.elapsed_samples.min(usize::MAX as u64) as usize }
    fn as_radiofax(&mut self) -> Option<&mut dyn crate::plugin::RadiofaxControl> { Some(self) }

}

impl RadiofaxControl for RadiofaxPlugin {
    fn radiofax_config(&self) -> RadiofaxConfigView { self.config.clone() }

    fn radiofax_state(&self) -> RadiofaxState {
        let segment = self.segments.iter().rev().find(|segment| !segment.lines.is_empty());
        let elapsed = self.page_started.map(|start| {
            self.elapsed_samples.saturating_sub(start) as f64 / DECODE_RATE_HZ
        }).unwrap_or(0.0);
        let reported_source = if self.config.phase_pixels != 0.0 || self.config.clock_ppm != 0.0 {
            "manual"
        } else {
            source_name(self.clock_source)
        };
        let stage = if self.stage == "fallback" { "fallback" } else { self.stage };
        RadiofaxState {
            running: self.fatal.is_none(),
            stage,
            has_picture: segment.is_some(),
            signal_level: self.signal_level,
            carrier_coherence: self.carrier_coherence,
            ioc: segment.map(|item| item.ioc).or(self.observed_ioc),
            lpm: segment.map(|item| item.lpm).or(self.observed_lpm),
            modulation: if self.config.modulation == "am" { "am" } else { "fm" },
            polarity: if self.config.polarity == "inverted" { "inverted" } else { "normal" },
            width: segment.map(|item| item.width).unwrap_or(0),
            lines: segment.map(|item| item.lines.len() as u32).unwrap_or(0),
            elapsed_seconds: elapsed,
            clock_status: clock_name(self.clock_status),
            clock_source: reported_source,
            phase_pixels: self.clock_phase + self.config.phase_pixels,
            clock_ppm: self.clock_ppm + self.config.clock_ppm,
            clock_confidence: self.clock_confidence,
            complete: segment.is_some_and(|item| item.complete),
            partial: segment.is_some_and(|item| item.partial) || self.page_partial,
            reason: self.fatal.clone().or_else(|| self.reason.clone()),
        }
    }

    fn take_radiofax_spans(&mut self) -> Vec<RadiofaxImageSpan> {
        let mut spans = Vec::new();
        let indexes: Vec<u32> = self.segments.iter().map(|segment| segment_index(&self.segments, segment)).collect();
        for (segment, index) in self.segments.iter_mut().zip(indexes) {
            let lines = segment.lines.len() as u32;
            if segment.dirty_from >= lines { continue; }
            let mut row = segment.dirty_from;
            while row < lines {
                let rows = (lines - row).min(SPAN_ROWS);
                let width = segment.width as usize;
                let start = row as usize * width;
                let end = start + rows as usize * width;
                spans.push(RadiofaxImageSpan {
                    width: segment.width,
                    row,
                    rows,
                    revision: segment.revision,
                    gray: segment.corrected[start..end].to_vec(),
                    meta: RadiofaxImageMeta {
                        serial: segment.serial,
                        ioc: segment.ioc,
                        lpm: segment.lpm,
                        lines,
                        complete: segment.complete,
                        partial: segment.partial,
                        segment_index: index,
                        transmission_id: segment.transmission_id,
                    },
                });
                row += rows;
            }
            segment.dirty_from = lines;
        }
        spans
    }

    fn apply_radiofax(&mut self, config: &RadiofaxConfigView, new_page: bool) -> Result<(), String> {
        validate_radiofax(config)?;
        let correction_changed = self.config.phase_pixels != config.phase_pixels
            || self.config.clock_ppm != config.clock_ppm;
        if protocol_changed(&self.config, config) || new_page {
            let had = self.active_has_lines();
            self.close_active(had, false);
            if had { self.reason = Some(if new_page { "new_page" } else { "reconfigured" }.into()); }
            self.config = config.clone();
            self.decoder = paper_decoder(config)?;
            self.apt_seen = false;
            self.phasing_seen = false;
            self.clock_status = FaxClockStatus::Nominal;
            self.clock_source = FaxClockSource::Nominal;
            self.clock_phase = 0.0;
            self.clock_ppm = 0.0;
            self.clock_confidence = 0.0;
            self.page_started = None;
            self.stage = if had { "partial" } else { "searching" };
        } else if correction_changed {
            self.config.phase_pixels = config.phase_pixels;
            self.config.clock_ppm = config.clock_ppm;
            for segment in &mut self.segments {
                if !segment.closed { segment.dirty_from = 0; }
            }
            self.correct_open_segments()?;
        }
        Ok(())
    }

    fn finish_input(&mut self) -> Result<(), String> {
        let mut events = Vec::new();
        self.decoder.finish(&mut |event: FaxPaperEventRef<'_>| events.push(event.to_owned()))
            .map_err(|error| error.to_string())?;
        let had = self.active_has_lines();
        self.absorb(events)?;
        if had && !self.page_complete {
            self.close_active(true, false);
            self.reason = Some("eof".into());
            self.stage = "partial";
        }
        Ok(())
    }

    fn mark_discontinuity(&mut self) -> Result<(), String> {
        self.band.clear_tail();
        let mut events = Vec::new();
        self.decoder.mark_signal_lost(&mut |event: FaxPaperEventRef<'_>| events.push(event.to_owned()))
            .map_err(|error| error.to_string())?;
        self.absorb(events)?;
        if self.page_partial { self.reason = Some("discontinuity".into()); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::DigitalDemodulator;
    use vendor::fax::{encode_fax, FaxEncodeOptions, FaxEncoder};
    use vendor::GrayImage;

    fn plugin() -> RadiofaxPlugin { RadiofaxPlugin::new(DECODE_RATE_HZ) }

    fn page(width: u32, height: u32, value: u8) -> GrayImage {
        GrayImage::new(width, height, alloc::vec![value; width as usize * height as usize]).unwrap()
    }

    fn fm_iq(pcm: &[f32]) -> Vec<f32> {
        pcm.iter().flat_map(|sample| [*sample, 0.0]).collect()
    }

    #[test]
    fn default_decoder_uses_ioc576_and_120_lpm_fallback() {
        let fax = plugin();
        let state = fax.radiofax_state();
        assert_eq!(state.stage, "searching");
        assert!(!state.has_picture);
        assert_eq!(state.modulation, "fm");
        assert_eq!(state.clock_source, "nominal");
    }

    #[test]
    fn synthesized_fm_page_survives_iq_bandpass_and_reports_gray_rows() {
        let spec = FaxSpec::standard(FaxIoc::Ioc576, FaxLpm::LPM_120);
        let image = page(spec.width(), 4, 40);
        let mut encoder = FaxEncoder::new(image, spec, DECODE_RATE_HZ as u32, FaxEncodeOptions {
            amplitude: 0.5, include_apt: false, include_phasing: false,
        }).unwrap();
        let mut pcm = vec![0.0; DECODE_RATE_HZ as usize / 2];
        let mut rest = Vec::new();
        let mut chunk = [0.0; 4096];
        while !encoder.is_finished() {
            let count = encoder.read_samples(&mut chunk);
            rest.extend_from_slice(&chunk[..count]);
        }
        pcm.extend(rest);
        pcm.extend(core::iter::repeat_n(0.0, DECODE_RATE_HZ as usize));
        let mut fax = plugin();
        for block in fm_iq(&pcm).chunks(1500) {
            assert!(fax.process_iq(block).is_empty());
        }
        let spans = fax.take_radiofax_spans();
        assert!(!spans.is_empty(), "fallback must emit gray rows");
        assert!(spans.iter().all(|span| span.rows <= SPAN_ROWS && span.gray.len() == span.width as usize * span.rows as usize));
        assert_eq!(spans[0].meta.ioc, 576);
        assert_eq!(spans[0].meta.lpm, 120);
        assert!(spans[0].gray.iter().any(|pixel| *pixel < 80));
        let state = fax.radiofax_state();
        assert_eq!(state.stage, "fallback");
        assert!(state.elapsed_seconds > 0.0);
        assert_ne!(state.clock_source, "phasing");
    }

    #[test]
    fn protocol_change_marks_partial_and_phase_does_not_reset_decoder_identity() {
        let mut fax = plugin();
        let spec = FaxSpec::standard(FaxIoc::Ioc288, FaxLpm::LPM_60);
        let pcm = encode_fax(page(spec.width(), 2, 200), spec, DECODE_RATE_HZ as u32).unwrap();
        fax.process_iq(&fm_iq(&pcm));
        let _ = fax.take_radiofax_spans();
        let before = fax.radiofax_state();
        let mut tuned = fax.radiofax_config();
        tuned.phase_pixels = 3.5;
        fax.apply_radiofax(&tuned, false).unwrap();
        assert_eq!(fax.radiofax_config().phase_pixels, 3.5);
        let mut manual = tuned;
        manual.acquisition = "manual".into();
        manual.ioc = "288".into();
        manual.lpm = "180".into();
        fax.apply_radiofax(&manual, false).unwrap();
        if before.has_picture {
            assert_eq!(fax.radiofax_state().stage, "partial");
            assert_eq!(fax.radiofax_state().reason.as_deref(), Some("reconfigured"));
        }
    }

    #[test]
    fn nonfinite_iq_is_a_hard_error_and_noise_does_not_invent_rows() {
        let mut fax = plugin();
        fax.process_iq(&[f32::NAN, 0.0]);
        assert!(fax.fatal.is_some());
        let mut quiet = plugin();
        quiet.process_iq(&vec![0.0; 48_000]);
        quiet.process_iq(&vec![0.0; 48_000]);
    }

    #[test]
    fn manual_one_eighty_is_accepted_and_auto_one_eighty_is_rejected() {
        let mut config = plugin().radiofax_config();
        config.acquisition = "manual".into();
        config.ioc = "576".into();
        config.lpm = "180".into();
        assert!(validate_radiofax(&config).is_ok());
        config.acquisition = "auto".into();
        assert!(validate_radiofax(&config).is_err());
    }
}
