//! The DRM receiver, implemented in Rust along Dream's stage structure.
//!
//! It replaced the previous receiver (deleted): that one found its super-frame phase once and
//! kept it, estimated the channel per symbol with linear interpolation and had no timing or
//! carrier tracking. On the bench capture the reference receiver (DecDRM's `decdrm rx`, which
//! ports Dream's stages) decoded far more audio, so the deficit was in the chain's structure,
//! not in tunable constants.
//!
//! This module follows Dream's stage order, each stage a plain Rust type fed by the
//! previous one and owning the state that belongs to it:
//!
//! ```text
//! baseband 48 kHz
//!   -> sync::freqacq    coarse carrier acquisition from the three continuous pilots
//!   -> sync::timesync   guard-interval correlation (low-passed, decimated), mode detection,
//!                       symbol timing and its tracking
//!   -> sync::framesync  frame phase from the time pilots
//!   -> ofdm             FFT demodulation into cells
//!   -> chanest          gain-reference grid, time- and frequency-Wiener estimation,
//!                       impulse-response tracking (delay/Doppler/SRO)
//!   -> fac / sdc        the signalling channels (FAC every frame, SDC in the super frame)
//!   -> mlc / interleave / msc   FEC, energy dispersal, deinterleaving, multiplex demux
//!   -> audio            the audio super frame (AAC and xHE layouts) into the codecs
//! ```
//!
//! The port is MIT-clean: Dream (`src/`, GPL-2.0) and DecDRM (`crates/decdrm-core`, GPL-2.0)
//! are read as algorithmic references and their behaviour is used as the oracle; no code from
//! either is copied. The stage list and the constants each stage uses are cross-checked against
//! them, and the geometry tables carry the Dream source they were checked against.
//!
//! The application keeps its current interface: the wasm ABI, the readout lines, the audio
//! PCM path and the backend's baseband AGC all stay as they are. The receiver decodes the
//! committed bench capture end to end and drives the browser's DRM audio path directly (see
//! `docs/en/DRM_HANDOFF.md`).

pub mod dsp;
pub mod cellmap;
pub mod chanest;
pub mod ofdm;
pub mod framesync;
pub mod params;
pub mod tables;
pub mod sync;
pub mod fec;
pub mod fac;
pub mod sdc;
pub mod interleave;
pub mod audio;

// The stages land here in the order above, each with its own task and tests; `params` is the
// shared foundation.

use crate::digital::drm::cellmap::CellMap;
use crate::digital::drm::chanest::ChanEst;
use crate::digital::drm::fac::Fac;
use crate::digital::drm::fec::mlc::{MlcDecoder, MlcParams};
use crate::digital::drm::fec::qam::EqCell;
use crate::digital::drm::interleave::CellDeinterleaver;
use crate::digital::drm::ofdm::OfdmDemod;
use crate::digital::drm::params::{RobustnessMode, SpectrumOccupancy};
use crate::digital::drm::sync::timesync::TimeSync;
use crate::digital::drm::dsp::Cplx;
use crate::plugin::{DigitalDemodulator, DigitalReport};

/// The DRM receiver as a single streaming stage (the task-9 integration): baseband 48 kHz in,
/// FAC/SDC/MSC/audio out. It owns the stages' persistent state and the acquisition/tracking
/// state machine, mirroring DecDRM's `chain.rs`. This is the skeleton the closed timing/SRO
/// loop hangs off; the FAC decode is the first end-to-end milestone.
pub struct DrmReceiver {
    buf: Vec<f32>,
    pushed: u64,
    pub mode: Option<RobustnessMode>,
    map: Option<CellMap>,
    chanest: Option<ChanEst>,
    fac_dec: Option<MlcDecoder>,
    fac_cells: Vec<EqCell>,
    frame_phase: usize,
    /// SDC cells of the current frame (symbols 0..sdc_syms), stored with the frame's FAC
    /// index once the FAC decodes.
    frame_sdc: Vec<EqCell>,
    /// MSC cells per emitted symbol (out_sym, cells) and the FAC frame indices, for the
    /// super-frame assembly after the pass. `msc_emitted` is drained by `decode_msc` each pass,
    /// so it only ever holds the symbols not yet assembled.
    msc_emitted: Vec<(usize, Vec<EqCell>)>,
    msc_frame_indices: Vec<u8>,
    /// Streaming super-frame assembly state (persists across `run` calls): the 45 symbol
    /// buckets, the long cell deinterleaver, the MLC decoder and its configuration key.
    msc_super: Vec<Vec<EqCell>>,
    msc_deinterleaver: Option<CellDeinterleaver>,
    msc_decoder: Option<MlcDecoder>,
    msc_decoder_key: Option<(crate::digital::drm::fec::qam::Mapping, usize, usize, usize)>,
    msc_bits: Vec<u8>,
    /// Set once the first `out_sym == 0` boundary is seen, so leading mid-frame symbols are
    /// not mistaken for a frame.
    msc_boundary_seen: bool,
    /// Set once a frame with FAC index 0 (the start of a super frame) is placed. Assembly waits
    /// for it: the first frame boundary after the warm-up is often frame 1 or 2 of a super
    /// frame, and starting there puts the earlier super frame's cells into the wrong buckets.
    msc_started: bool,
    /// The last successfully placed frame's super-frame index, for the continuity check. A
    /// skipped frame boundary (a dropped timing window) shifts the FAC index sequence; the
    /// buckets then hold a broken super frame, and the next super frame would append to them.
    msc_prev_index: Option<u8>,
    /// Frames seen since the first emitted frame boundary (indexes `msc_frame_indices`).
    msc_complete_frame: usize,
    /// The current frame's emitted symbols `(out_sym, cells)`, held until the frame ends. The
    /// FAC that names the frame's position in the super frame only decodes at the frame's end,
    /// so a frame is placed into the buckets one frame after its symbols arrive.
    msc_frame_buf: Vec<(usize, Vec<EqCell>)>,
    /// SDC blocks collected per FAC frame, drained by `decode_sdc` each pass.
    sdc_pending: Vec<(u8, Vec<EqCell>)>,
    /// Decoded MSC multiplex frames (information bits).
    pub msc_frames: Vec<Vec<u8>>,
    msc_frame_count: usize,
    /// Acquisition/tracking state (Dream's RxState).
    tracking: bool,
    timing_tracking: bool,
    good_facs: usize,
    /// Consecutive FAC blocks that failed their CRC. When this grows the receiver has lost the
    /// channel (a fade, or the bench transmitter's cyclic wrap): the phase/estimator state is
    /// re-acquired instead of wedging, which is what the operator otherwise fixes by re-applying
    /// a setting (a pipeline reset).
    consecutive_fac_failures: usize,
    /// Absolute sample count at the last good FAC. No timing windows on a dead channel means
    /// there are no CRC failures to trigger recovery, so we also bound time without progress.
    last_good_fac_sample: u64,
    recover_pending: bool,
    /// Good-FAC countdown before the external timing tracking takes over (DecDRM's
    /// DELAYED_TRACKING_FACS).
    delayed_cnt: usize,
    // Results.
    pub facs: Vec<Fac>,
    pub fac_errors: usize,
    pub fac_constellation: Vec<(f64, f64)>,
    pub fac_soft_bits: Vec<f64>,
    pub symbols_demodulated: usize,
    pub carrier_offset_hz: f64,
    /// The station label from the SDC, once a super frame's SDC block passes its CRC.
    pub station_label: Option<String>,
    pub sdc_ok: usize,
    /// The multiplex description and audio descriptor from the SDC (the audio path's inputs).
    pub multiplex: Option<crate::digital::drm::sdc::MultiplexDescription>,
    pub audio: Option<crate::digital::drm::sdc::AudioInfo>,
    /// Stateful xHE-AAC audio super frame parser (its frames span super frames).
    xhe_deframer: crate::digital::drm::audio::XheAacDeframer,
    /// Mode E's 200 ms audio super frame spans two 100 ms multiplex frames.
    mode_e_audio_half: Option<Vec<u8>>,
    /// AAC access units deframed from the audio stream (ready for the codec).
    pub audio_access_units: Vec<Vec<u8>>,
    audio_unit_count: usize,
    /// Decoded audio PCM (interleaved i16), wasm32 only.
    pub audio_pcm: Vec<i16>,
    /// Audio units already decoded, so a later pass decodes only the new ones.
    audio_units_decoded: usize,
    /// Cumulative PCM samples the codec has produced (diagnostics: the audio production rate).
    pcm_total: u64,
    audio_debug: String,
    #[cfg(target_arch = "wasm32")]
    audio_decoder: Option<crate::fdk::AacDecoder>,
    #[cfg(target_arch = "wasm32")]
    audio_configured_with: Option<Vec<u8>>,
    // Incremental processing state: the mode detection runs once (when enough samples are
    // buffered), then the demodulation and the decode consume only the NEW samples.
    mode_detected: bool,
    processed_complex: usize,
    phase_computed: bool,
    demod_rows: usize,
    all_rows: Vec<Vec<Cplx>>,
    all_shifts: Vec<i64>,
    coarse: f64,
    nco: Option<crate::digital::drm::sync::nco::Nco>,
    ft: Option<crate::digital::drm::sync::freqtrack::FreqTrack>,
    tsync: Option<TimeSync>,
    /// Streaming frame-phase accumulator (the block-fed counterpart of `FrameSync::search`).
    fs_acq: Option<crate::digital::drm::framesync::FramePhaseAcquisition>,
    demod: Option<OfdmDemod>,
    spf: usize,
    /// The baseband sample rate this receiver is fed (48 kHz for modes A-D, 96 kHz for mode E).
    sample_rate: u32,
    /// The occupancy selected with the mode.
    occupancy: SpectrumOccupancy,
    phase: usize,
    sym_count: usize,
    freq_track: f64,
}

/// The audio worklet consumes mono PCM. FDK returns interleaved channels; average each frame
/// before the worker resamples, or stereo samples become twice as long and the wrong pitch.
#[cfg(any(test, target_arch = "wasm32"))]
fn downmix_pcm(pcm: &[i16], channels: usize) -> Vec<i16> {
    if channels <= 1 {
        return pcm.to_vec();
    }
    pcm.chunks_exact(channels)
        .map(|frame| (frame.iter().map(|&s| i32::from(s)).sum::<i32>() / channels as i32) as i16)
        .collect()
}

impl DrmReceiver {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            pushed: 0,
            mode: None,
            map: None,
            chanest: None,
            fac_dec: None,
            fac_cells: Vec::new(),
            frame_phase: 0,
            tracking: false,
            timing_tracking: false,
            good_facs: 0,
            consecutive_fac_failures: 0,
            last_good_fac_sample: 0,
            recover_pending: false,
            delayed_cnt: 2,
            facs: Vec::new(),
            fac_errors: 0,
            fac_constellation: Vec::new(),
            fac_soft_bits: Vec::new(),
            symbols_demodulated: 0,
            carrier_offset_hz: 0.0,
            station_label: None,
            sdc_ok: 0,
            multiplex: None,
            audio: None,
            xhe_deframer: crate::digital::drm::audio::XheAacDeframer::new(),
            mode_e_audio_half: None,
            audio_access_units: Vec::new(),
            audio_unit_count: 0,
            audio_pcm: Vec::new(),
            audio_units_decoded: 0,
            pcm_total: 0,
            audio_debug: String::new(),
            #[cfg(target_arch = "wasm32")]
            audio_decoder: None,
            #[cfg(target_arch = "wasm32")]
            audio_configured_with: None,
            frame_sdc: Vec::new(),
            msc_emitted: Vec::new(),
            msc_frame_indices: Vec::new(),
            msc_super: vec![Vec::new(); 45],
            msc_deinterleaver: None,
            msc_decoder: None,
            msc_decoder_key: None,
            msc_bits: Vec::new(),
            msc_boundary_seen: false,
            msc_started: false,
            msc_prev_index: None,
            msc_complete_frame: 0,
            msc_frame_buf: Vec::new(),
            sdc_pending: Vec::new(),
            msc_frames: Vec::new(),
            msc_frame_count: 0,
            mode_detected: false,
            processed_complex: 0,
            phase_computed: false,
            demod_rows: 0,
            all_rows: Vec::new(),
            all_shifts: Vec::new(),
            coarse: 0.0,
            nco: None,
            ft: None,
            tsync: None,
            fs_acq: None,
            demod: None,
            spf: 15,
            sample_rate: 48_000,
            occupancy: SpectrumOccupancy::SO_3,
            phase: 0,
            sym_count: 0,
            freq_track: 0.0,
        }
    }

    /// A receiver fed at `sample_rate` Hz: 48 kHz selects DRM30 modes A-D, 96 kHz selects mode E
    /// (DRM+). The pipeline resamples the baseband to the demodulator's rate before it arrives.
    pub fn new_at(sample_rate: u32) -> Self {
        let mut r = Self::new();
        r.sample_rate = sample_rate;
        r
    }

    pub fn push(&mut self, iq: &[f32]) {
        self.buf.extend_from_slice(iq);
        self.pushed += iq.len() as u64 / 2;
    }

    pub fn locked(&self) -> bool {
        // A detected mode alone is not a lock: the readout must not say "locked" before a FAC
        // block has decoded and named the station.
        self.map.is_some() && !self.facs.is_empty()
    }

    /// The complex baseband samples buffered for the next decode attempt.
    pub fn buffered(&self) -> usize {
        self.buf.len() / 2
    }

    /// The channel estimator's FAC SNR, dB (None before the first frame).
    pub fn snr_db(&self) -> Option<f64> {
        self.chanest.as_ref().and_then(|e| e.stats.snr_db)
    }

    /// The spectrum occupancy this receiver decodes (SO3 for the bench DRM30 signal, SO0 for
    /// mode E).
    pub fn occupancy(&self) -> SpectrumOccupancy {
        self.occupancy
    }

    /// Incremental decode: each call demodulates the samples that arrived since the previous
    /// call and feeds them through the channel estimation and the FAC/SDC/MSC decode. The
    /// first call (when enough samples are buffered) also runs the mode detection and the
    /// coarse carrier acquisition, so later calls only carry the new samples.
    pub fn run(&mut self) {
        self.run_with_layout_retry(true);
    }

    fn run_with_layout_retry(&mut self, may_retry: bool) {
        // Mode detection: once, when enough samples are buffered (the guard correlation and
        // the frame sync need a few frames).
        if !self.mode_detected {
            if self.buffered() < 40_000 {
                return;
            }
            let iq: Vec<Cplx> = self
                .buf
                .chunks_exact(2)
                .map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1])))
                .collect();
            let mut best: Option<(RobustnessMode, CellMap)> = None;
            let mut best_rows = 0usize;
            for m in RobustnessMode::ALL
                .iter()
                .copied()
                .filter(|m| m.sample_rate() == self.sample_rate)
            {
                // Mode E (DRM+) is defined for SO 0 only; DRM30 modes here use the 10 kHz
                // occupancy the bench signal carries.
                let so = if m == RobustnessMode::E {
                    SpectrumOccupancy::SO_0
                } else {
                    SpectrumOccupancy::SO_3
                };
                let Some(cmap) = CellMap::new(m, so) else { continue };
                let mut tsync = TimeSync::new(m);
                let mut demod = OfdmDemod::new(&cmap);
                let mut cells = Vec::new();
                let mut rows = 0usize;
                for block in iq.chunks(3248) {
                    let _ = tsync.push(block);
                    while let Some(w) = tsync.next_window() {
                        if w.guard_corr.unwrap_or(0.0) < 0.5 {
                            continue;
                        }
                        demod.demodulate(&w.samples, &mut cells);
                        rows += 1;
                    }
                }
                if rows > best_rows {
                    best_rows = rows;
                    best = Some((m, cmap));
                    self.occupancy = so;
                }
            }
            let Some((mode, cmap)) = best else {
                if self.buf.len() > 200_000 * 2 {
                    self.buf.drain(..self.buf.len() - 200_000 * 2);
                }
                return;
            };

            // Coarse carrier acquisition.
            let flat: Vec<f64> = self
                .buf
                .chunks_exact(2)
                .flat_map(|c| [f64::from(c[0]), f64::from(c[1])])
                .collect();
            let coarse = if mode == RobustnessMode::E {
                // Mode E has no continuous frequency pilots; follow the guard phase below.
                0.0
            } else {
                crate::digital::drm::sync::freqacq::FreqAcquisition::new(true, f64::from(mode.sample_rate()))
                    .push_iq(&flat).map(|a| a.dc_hz).unwrap_or(0.0)
            };

            self.carrier_offset_hz = coarse;
            self.coarse = coarse;
            self.freq_track = coarse;
            self.mode = Some(mode);
            self.map = Some(cmap.clone());
            self.spf = mode.symbols_per_frame();
            // The MSC super-frame assembly holds one bucket per super-frame symbol, which
            // depends on the mode (mode A/B: 45, mode C: 60, mode D: 72).
            self.msc_super = vec![Vec::new(); cmap.msc_carriers.len()];
            self.nco = Some(crate::digital::drm::sync::nco::Nco::new(coarse, f64::from(mode.sample_rate())));
            let mut ft = crate::digital::drm::sync::freqtrack::FreqTrack::new(&cmap);
            ft.set_freq_time_constant(0.1);
            self.ft = Some(ft);
            self.tsync = Some(TimeSync::new(mode));
            self.fs_acq = Some(crate::digital::drm::framesync::FramePhaseAcquisition::new(&cmap));
            self.demod = Some(OfdmDemod::new(&cmap));
            self.mode_detected = true;
            // The tracked demodulation consumes the buffered capture from the start (the
            // frame sync needs the whole capture for the phase).
            self.processed_complex = 0;
        }

        let map_owned = self.map.clone().expect("mode detected");
        let map = &map_owned;

        // Demodulate the samples that arrived since the previous call, with the streaming NCO
        // re-tuned every symbol by the frequency tracker.
        let start_f32 = self.processed_complex * 2;
        let new_iq: Vec<Cplx> = self.buf[start_f32..]
            .chunks_exact(2)
            .map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1])))
            .collect();
        {
            let nco = self.nco.as_mut().expect("acquired");
            let ft = self.ft.as_mut().expect("acquired");
            let ts = self.tsync.as_mut().expect("acquired");
            let demod = self.demod.as_mut().expect("acquired");
            let mut cells = Vec::new();
            for block in new_iq.chunks(3248) {
                let (mut cp_sum, mut cp_count) = (0.0, 0usize);
                let mut mixed = block.to_vec();
                nco.process(&mut mixed);
                let _ = ts.push(&mixed);
                while let Some(w) = ts.next_window() {
                    if w.guard_corr.unwrap_or(0.0) < 0.5 {
                        continue;
                    }
                    if map.mode() == RobustnessMode::E {
                        if let Some(hz) = w.guard_offset_hz.filter(|v| v.is_finite()) {
                            if w.guard_corr.unwrap_or(0.0) > 0.7 {
                                cp_sum += hz;
                                cp_count += 1;
                            }
                        }
                    }
                    demod.demodulate(&w.samples, &mut cells);
                    self.all_rows.push(cells.clone());
                    self.all_shifts.push(w.shift);
                    if !self.phase_computed {
                        self.fs_acq.as_mut().expect("acquired").push(&cells);
                    }
                    let o = ft.process(&cells, w.shift);
                    self.freq_track += o.freq_delta_hz;
                    nco.set_offset(self.freq_track);
                    self.sym_count += 1;
                    if self.sym_count == 3 * self.spf {
                        ft.set_freq_time_constant(1.0);
                    }
                }
                if map.mode() == RobustnessMode::E && cp_count > 0 {
                    // All windows in this block saw the same pre-correction samples. Apply
                    // the averaged residual only once, to the next block's NCO.
                    let gain = if self.sym_count < 60 { 1.0 } else { 0.2 };
                    self.freq_track += gain * cp_sum / cp_count as f64;
                    nco.set_offset(self.freq_track);
                }
            }
        }
        self.processed_complex = self.buf.len() / 2;
        // Before the first valid FAC, keep enough capture for a possible occupancy
        // switch: re-decoding only the final four seconds made whole-buffer and
        // worker-block feeds disagree. Once the layout is known, four seconds suffice.
        let keep = if self.facs.is_empty() { 400_000 * 2 } else { 200_000 * 2 };
        if self.buf.len() > keep {
            let drop = self.buf.len() - keep;
            self.buf.drain(..drop);
            self.processed_complex -= drop / 2;
        }

        // The frame phase: accumulated across all rows so far, not just a short prefix. The
        // first frames are still converging (the frequency tracker and the timing loop are
        // settling), so a fixed row count can commit on a wrong phase; require the correct
        // phase to clearly beat the runner-up instead. The row cap is a safety valve for a
        // signal so weak the margin never opens (the coherent score still grows as sqrt(n), so
        // at the cap the best candidate is the right one).
        if !self.phase_computed {
            let (phase, best, second, rows) = {
                let acq = self.fs_acq.as_ref().expect("acquired");
                let (p, b, s) = acq.best();
                (p, b, s, acq.rows())
            };
            let enough = rows >= 3 * self.spf && best >= 1.5 * second;
            if enough || rows >= 12 * self.spf {
                self.phase = phase;
                self.frame_phase = phase;
                self.phase_computed = true;
                self.demod_rows = 0;
            } else {
                return;
            }
        }

        // Channel estimation and FAC/SDC/MSC over the rows that arrived since the previous
        // call. The estimator uses the exact linear time interpolation (the path the reference
        // keeps until its timing/SRO loop is closed and the Doppler-adapted time-Wiener can be
        // trusted; the time-Wiener corrupts the clean-fixture MSC while the window still carries
        // a residual timing ramp — see the handoff).
        if self.chanest.is_none() {
            let est = ChanEst::new(map);
            self.chanest = Some(est);
        }
        let fac_dec = self
            .fac_dec
            .get_or_insert_with(|| MlcDecoder::new(MlcParams::fac_for(self.mode.unwrap_or(RobustnessMode::B)), 0));

        // Take the unprocessed rows out of the receiver so the decode can borrow the other
        // fields freely; the phase-adjusted symbol index and the timing shift ride along.
        let rows: Vec<(usize, Vec<Cplx>, i64)> = (0..self.all_rows.len())
            .map(|i| (self.demod_rows + i, self.all_rows[i].clone(), self.all_shifts[i]))
            .collect();
        self.demod_rows += rows.len();

        let live_rows = rows.len() <= self.spf;
        let mut occupancy_change: Option<CellMap> = None;
        let mut enable_timing_tracking = false;
        {
            let est = self.chanest.as_mut().unwrap();
            let mut bits = Vec::new();
            for (i, row, shift) in rows {
                let sym = (i % self.spf + self.phase) % self.spf;
                let Some((out_sym, out)) = est.process(&row, sym, shift, map) else { continue };
                // Close the timing/SRO loop: the impulse-response tracker's window correction and
                // sample-rate offset go back into the TimeSync, as the reference's chain does.
                // This is what keeps the FFT window aligned (and the channel free of a residual
                // timing ramp) once the tracking has been enabled.
                if est.timing_tracking() {
                    let t = est.last_track;
                    if let Some(ts) = self.tsync.as_mut() {
                        if t.timing_adjust != 0 {
                            ts.adjust_timing(t.timing_adjust as f64);
                        }
                        if t.sro_delta_hz != 0.0 {
                            ts.adjust_sro(t.sro_delta_hz);
                        }
                    }
                }
                self.symbols_demodulated += 1;
                self.msc_emitted.push((out_sym, out.clone()));
                if out_sym == 0 {
                    self.fac_cells.clear();
                    self.frame_sdc.clear();
                }
                let sdc_syms = map.mode().sdc_symbols();
                if out_sym < sdc_syms {
                    for &c in &map.sdc_carriers[out_sym] {
                        self.frame_sdc.push(out[c as usize]);
                    }
                }
                for &c in &map.fac_carriers[out_sym] {
                    self.fac_cells.push(out[c as usize]);
                    self.fac_constellation.push((out[c as usize].sig.re, out[c as usize].sig.im));
                }
                if self.fac_cells.len() == crate::digital::drm::tables::fac_cell_count(map.mode()) {
                    let decoded = fac_dec.decode(&self.fac_cells, &mut bits);
                    let parsed = if decoded { Fac::parse_for(map.mode(), &bits) } else { None };
                    if may_retry {
                        if let Some(f) = parsed {
                            if f.channel.occupancy != map.occupancy() {
                                occupancy_change = CellMap::new(map.mode(), f.channel.occupancy);
                                if occupancy_change.is_some() { break; }
                            }
                        }
                    }
                    // Even a CRC-valid FAC is unusable with the wrong cell map. On the
                    // replay pass wait for the new occupancy instead of switching again.
                    let fac = parsed.filter(|f| f.channel.occupancy == map.occupancy());
                    let idx = fac.as_ref().map(|f| f.channel.frame_index).unwrap_or(0xFF);
                    match fac {
                        Some(f) => {
                            self.good_facs += 1;
                            self.consecutive_fac_failures = 0;
                            self.last_good_fac_sample = self.pushed;
                            self.tracking = true;
                            if self.good_facs >= 2 && !self.timing_tracking {
                                if self.delayed_cnt > 0 {
                                    self.delayed_cnt -= 1;
                                } else {
                                    self.timing_tracking = true;
                                    if live_rows {
                                        // Keep guard timing active: turning it off here lost
                                        // live xHE frames on the committed bench capture.
                                        est.start_timing_tracking();
                                    } else {
                                        enable_timing_tracking = true;
                                    }
                                }
                            }
                            self.facs.push(f);
                        }
                        None => {
                            self.fac_errors += 1;
                            self.consecutive_fac_failures += 1;
                            if self.consecutive_fac_failures >= 12 {
                                self.recover_pending = true;
                            }
                        }
                    }
                    self.sdc_pending.push((idx, std::mem::take(&mut self.frame_sdc)));
                    self.msc_frame_indices.push(idx);
                    self.fac_cells.clear();
                }
            }
        }
        self.all_rows.clear();
        self.all_shifts.clear();
        if let Some(selected) = occupancy_change {
            self.occupancy = selected.occupancy();
            self.map = Some(selected.clone());
            // A valid FAC names this signal's layout, not a lost channel. Replay the
            // buffered capture so SDC and the long MSC interleaver get their warm-up.
            self.recover_with_keep(&selected, self.buf.len());
            self.run_with_layout_retry(false);
            return;
        }
        if enable_timing_tracking {
            // A large backlog was demodulated before channel estimation. Activating
            // tracking mid-replay would add every historical timing correction to one
            // future FFT window; start only after the backlog has been consumed.
            self.chanest.as_mut().unwrap().start_timing_tracking();
        }
        if self.locked()
            && self.pushed.saturating_sub(self.last_good_fac_sample) >= u64::from(self.sample_rate) * 3
        {
            self.recover_pending = true;
        }
        if self.recover_pending {
            self.recover(map);
            return;
        }
        self.decode_sdc(map);
        self.decode_msc(map);
        #[cfg(target_arch = "wasm32")]
        self.prune_history();
    }

    // Keep the browser's diagnostics recent while the displayed totals remain cumulative.
    // Native receivers retain full captures for analysis and bit-exact fixture comparisons.
    #[cfg(any(test, target_arch = "wasm32"))]
    fn prune_history(&mut self) {
        if self.facs.len() > 256 {
            self.facs.drain(..self.facs.len() - 128);
        }
        if self.msc_frames.len() > 256 {
            self.msc_frames.drain(..self.msc_frames.len() - 128);
        }
        if self.audio_access_units.len() > 1024 {
            let dropped = self.audio_access_units.len() - 512;
            self.audio_access_units.drain(..dropped);
            self.audio_units_decoded = self.audio_units_decoded.saturating_sub(dropped);
        }
        if self.fac_constellation.len() > 2048 {
            self.fac_constellation.drain(..self.fac_constellation.len() - 1024);
        }
    }

    /// Re-acquire after a run of FAC failures: drop the stale phase/estimator/MSC state and run
    /// the coarse acquisition again on the recent baseband, so a fade or a bench-loop wrap does
    /// not wedge the receiver until the operator resets it.
    fn recover(&mut self, map: &CellMap) {
        self.recover_with_keep(map, self.sample_rate as usize * 3); // 1.5 s of interleaved I/Q
    }

    fn recover_with_keep(&mut self, map: &CellMap, keep: usize) {
        use crate::digital::drm::sync::freqacq::FreqAcquisition;
        use crate::digital::drm::sync::freqtrack::FreqTrack;
        use crate::digital::drm::sync::nco::Nco;
        // A lost channel must not replay the old station. A newly identified occupancy
        // can replay the entire buffered capture: it belongs to the same transmission.
        if self.buf.len() > keep {
            let start = self.buf.len() - keep;
            self.buf.drain(..start);
        }
        self.processed_complex = 0;
        self.phase_computed = false;
        self.phase = 0;
        self.frame_phase = 0;
        self.fs_acq = Some(crate::digital::drm::framesync::FramePhaseAcquisition::new(map));
        self.all_rows.clear();
        self.all_shifts.clear();
        self.demod_rows = 0;
        self.chanest = None;
        self.fac_dec = None;
        self.fac_cells.clear();
        self.frame_sdc.clear();
        self.sdc_pending.clear();
        self.msc_super = vec![Vec::new(); map.msc_carriers.len()];
        self.msc_deinterleaver = None;
        self.msc_decoder = None;
        self.msc_decoder_key = None;
        self.msc_bits.clear();
        self.msc_boundary_seen = false;
        self.msc_started = false;
        self.msc_complete_frame = 0;
        self.msc_frame_buf.clear();
        self.msc_emitted.clear();
        self.msc_frame_indices.clear();
        self.msc_prev_index = None;
        self.facs.clear();
        self.fac_errors = 0;
        self.fac_constellation.clear();
        self.fac_soft_bits.clear();
        self.station_label = None;
        self.sdc_ok = 0;
        self.multiplex = None;
        self.audio = None;
        self.xhe_deframer = crate::digital::drm::audio::XheAacDeframer::new();
        self.mode_e_audio_half = None;
        self.audio_access_units.clear();
        self.audio_units_decoded = 0;
        self.audio_pcm.clear();
        self.audio_debug.clear();
        #[cfg(target_arch = "wasm32")]
        {
            self.audio_decoder = None;
            self.audio_configured_with = None;
        }
        self.msc_frames.clear();
        self.msc_frame_count = 0;
        self.audio_unit_count = 0;
        self.tracking = false;
        self.timing_tracking = false;
        self.good_facs = 0;
        self.delayed_cnt = 2;
        self.sym_count = 0;
        self.consecutive_fac_failures = 0;
        self.last_good_fac_sample = self.pushed;
        self.recover_pending = false;
        // Re-run the coarse carrier acquisition on the kept samples.
        let flat: Vec<f64> = self
            .buf
            .chunks_exact(2)
            .flat_map(|c| [f64::from(c[0]), f64::from(c[1])])
            .collect();
        let coarse = if self.mode == Some(RobustnessMode::E) {
            0.0
        } else {
            FreqAcquisition::new(true, f64::from(self.sample_rate))
                .push_iq(&flat)
                .map(|a| a.dc_hz)
                .unwrap_or(self.carrier_offset_hz)
        };
        self.carrier_offset_hz = coarse;
        self.coarse = coarse;
        self.freq_track = coarse;
        self.nco = Some(Nco::new(coarse, f64::from(self.sample_rate)));
        let mut ft = FreqTrack::new(map);
        ft.set_freq_time_constant(0.1);
        self.ft = Some(ft);
        if let Some(mode) = self.mode {
            self.tsync = Some(TimeSync::new(mode));
        }
        self.demod = Some(OfdmDemod::new(map));
    }

    /// Assemble the MSC super frames from the emitted cells and decode them (cell deinterleave
    /// + MLCC). The multiplex-frame boundary is every N_MUX cells, not 15 symbols, so the
    /// sym-order concatenation is chunked by cell count exactly as the bit-exact test does.
    fn decode_msc(&mut self, map: &CellMap) {
        use crate::digital::drm::fac::MscMode;
        use crate::digital::drm::fec::mlc::MscProtection;
        use crate::digital::drm::fec::qam::Mapping;
        // Nothing to assemble until the SDC has described the stream layout; the emitted cells
        // stay pending in `msc_emitted` until then.
        if self.multiplex.is_none() {
            return;
        }
        // The MSC configuration comes from the FAC (mode) and the SDC (protection and the
        // higher-protected part-A bytes), not a hardcoded EEP 0/1.
        let mode = self.facs.first().map(|f| f.channel.msc_mode).unwrap_or(MscMode::Qam64Sm);
        let mapping = match mode {
            MscMode::Qam64Sm => Mapping::Qam64Sm,
            MscMode::Qam64HmMix => Mapping::Qam64HmMix,
            MscMode::Qam64HmSym => Mapping::Qam64HmSym,
            MscMode::Qam16Sm => Mapping::Qam16,
            MscMode::Qam4 => Mapping::Qam4,
        };
        let prot = MscProtection {
            part_a: self.multiplex.as_ref().map(|m| m.protection_a as usize).unwrap_or(0),
            part_b: self.multiplex.as_ref().map(|m| m.protection_b as usize).unwrap_or(1),
            hierarchical: 0,
        };
        let part_a_bytes = self
            .multiplex
            .as_ref()
            .map(|m| m.streams.iter().map(|s| s.len_a as usize).sum::<usize>())
            .unwrap_or(0);
        let params = if map.mode() == RobustnessMode::E {
            MlcParams::msc_e(mapping, map.msc_cells_per_frame, prot, part_a_bytes)
        } else {
            MlcParams::msc(mapping, map.msc_cells_per_frame, prot, part_a_bytes)
        };
        // Persistent stages: the deinterleaver holds the long-interleaving memory, so it must
        // survive across calls; the MLC decoder is rebuilt only when the configuration changes.
        if self.msc_deinterleaver.is_none() {
            self.msc_deinterleaver = Some(CellDeinterleaver::new(
                map.msc_cells_per_frame, if map.mode() == RobustnessMode::E { 6 } else { 5 },
            ));
        }
        let key = (mapping, prot.part_a, prot.part_b, part_a_bytes);
        if self.msc_decoder_key != Some(key) {
            self.msc_decoder = Some(MlcDecoder::new(params, 1));
            self.msc_decoder_key = Some(key);
        }
        // Consume only the symbols that arrived since the previous call (`msc_emitted` is
        // drained, so the assembly state below carries over). Symbols are grouped into frames by
        // their `out_sym == 0` boundary and a frame is placed into the super-frame buckets when
        // the NEXT frame starts, because the current frame's FAC index (its position in the super
        // frame) is only known once the FAC completes, at the end of the frame.
        let emitted = std::mem::take(&mut self.msc_emitted);
        let mut decoded: Vec<(usize, Vec<u8>)> = Vec::new();
        for (out_sym, cells) in &emitted {
            let out_sym = *out_sym;
            if out_sym == 0 {
                self.msc_boundary_seen = true;
                let buf = std::mem::take(&mut self.msc_frame_buf);
                if !buf.is_empty() {
                    self.assemble_msc_frame(map, &buf, &mut decoded);
                }
            }
            if self.msc_boundary_seen {
                self.msc_frame_buf.push((out_sym, cells.clone()));
            }
        }
        // No trailing partial-super-frame flush: in the streaming model the last super frame is
        // decoded when its next frame 0 arrives (or, for a finite capture, it is simply the one
        // incomplete frame at the end). Flushing it on every call decoded it repeatedly.
        for (frame_index, b) in decoded {
            self.deframe_audio(&b, frame_index);
            self.msc_frames.push(b);
            self.msc_frame_count += 1;
        }
        // The FAC indices of completed frames have already been placed in the super-frame
        // buckets; retain only the current frame's index for the next streaming pass.
        if self.msc_complete_frame > 0 {
            self.msc_frame_indices.drain(..self.msc_complete_frame);
            self.msc_complete_frame = 0;
        }
    }

    /// Place one completed frame's cells into the super-frame buckets, decoding a super frame
    /// when its next one begins. `buf` is the frame's `(out_sym, cells)` in emitted order; its
    /// FAC index is looked up here because a full frame has passed since those symbols arrived.
    fn assemble_msc_frame(
        &mut self,
        map: &CellMap,
        buf: &[(usize, Vec<EqCell>)],
        decoded: &mut Vec<(usize, Vec<u8>)>,
    ) {
        let cf = self.msc_complete_frame;
        let Some(&frame_index) = self.msc_frame_indices.get(cf) else {
            return;
        };
        self.msc_complete_frame += 1;
        if frame_index == 0xFF {
            // A frame with no FAC: its cells are missing from the super frame, so the buckets are
            // broken. Drop them rather than let the next super frame append to them.
            for c in self.msc_super.iter_mut() {
                c.clear();
            }
            self.msc_prev_index = None;
            return;
        }
        // Wait for the first real super-frame start (frame 0): the frames before it belong to a
        // super frame the warm-up truncated, and placing them would leave buckets that the next
        // super frame then appends to.
        if !self.msc_started {
            if frame_index != 0 {
                return;
            }
            self.msc_started = true;
        } else if let Some(prev) = self.msc_prev_index {
            if frame_index != (prev + 1) % self.mode.expect("acquired").frames_per_superframe() as u8 {
                // A frame boundary was skipped (a dropped timing window), so the buckets hold an
                // incomplete super frame. Drop it and restart at this frame.
                for c in self.msc_super.iter_mut() {
                    c.clear();
                }
            }
        }
        self.msc_prev_index = Some(frame_index);
        if frame_index == 0
            && (0..self.msc_super.len())
                .all(|s| map.msc_carriers[s].is_empty() || !self.msc_super[s].is_empty())
        {
            let mut all: Vec<EqCell> = Vec::new();
            for c in self.msc_super.iter() {
                all.extend_from_slice(c);
            }
            for (input_index, frame) in all.chunks(map.msc_cells_per_frame)
                .take(map.mode().frames_per_superframe()).enumerate() {
                let de = self.msc_deinterleaver.as_mut().expect("created");
                let output_index = (input_index + map.mode().frames_per_superframe()
                    - (de.depth() - 1) % map.mode().frames_per_superframe())
                    % map.mode().frames_per_superframe();
                if let Some(d) = de.push(frame) {
                    if d.iter().all(|c| c.chan > 0.0)
                        && self.msc_decoder.as_mut().expect("created").decode(&d, &mut self.msc_bits)
                    {
                        decoded.push((output_index, self.msc_bits.clone()));
                    }
                }
            }
            for c in self.msc_super.iter_mut() {
                c.clear();
            }
        }
        for (out_sym, cells) in buf {
            let super_sym = frame_index as usize * self.spf + out_sym;
            for &c in &map.msc_carriers[super_sym] {
                if let Some(cell) = cells.get(c as usize) {
                    self.msc_super[super_sym].push(*cell);
                }
            }
        }
    }

    /// Decode the SDC blocks collected so far (the frame-0 block of each super frame) into the
    /// station label.
    fn decode_sdc(&mut self, map: &CellMap) {
        use crate::digital::drm::fec::qam::Mapping;
        use crate::digital::drm::sdc::parse_sdc_block;
        let pending = std::mem::take(&mut self.sdc_pending);
        let mut bits = Vec::new();
        for (idx, cells) in &pending {
            if *idx != 0 || cells.len() != map.sdc_cells_per_superframe {
                continue;
            }
            if map.mode() == RobustnessMode::E {
                let protection = self.facs.last().map(|f| f.channel.sdc_protection).unwrap_or(0);
                let mut dec = MlcDecoder::new(MlcParams::sdc_e(map.sdc_cells_per_superframe, protection), 0);
                if dec.decode(cells, &mut bits) {
                    if let Some(b) = parse_sdc_block(&bits).filter(|b| b.crc_ok) {
                        self.apply_sdc_entities(&b.data);
                    }
                }
                continue;
            }
            let mut sdc16 = MlcDecoder::new(MlcParams::sdc(Mapping::Qam16, map.sdc_cells_per_superframe), 0);
            let mut sdc4 = MlcDecoder::new(MlcParams::sdc(Mapping::Qam4, map.sdc_cells_per_superframe), 0);
            let mut block = if sdc16.decode(cells, &mut bits) {
                parse_sdc_block(&bits).filter(|b| b.crc_ok)
            } else {
                None
            };
            if block.is_none() {
                let mut b4 = Vec::new();
                block = if sdc4.decode(cells, &mut b4) {
                    parse_sdc_block(&b4).filter(|b| b.crc_ok)
                } else {
                    None
                };
            }
            if let Some(b) = block {
                self.apply_sdc_entities(&b.data);
            }
        }
    }

    fn apply_sdc_entities(&mut self, data: &[u8]) {
        use crate::digital::drm::sdc::{parse_entities, Entity};
        self.sdc_ok += 1;
        for entity in parse_entities(data) {
            match entity {
                Entity::Label(label) => {
                    if self.station_label.is_none() { self.station_label = Some(label.text()); }
                }
                Entity::Multiplex(mux) => {
                    if self.multiplex.is_none() { self.multiplex = Some(mux); }
                }
                Entity::Audio(audio) => {
                    if self.audio.is_none() { self.audio = Some(audio); }
                }
                _ => {}
            }
        }
    }

    /// Demultiplex one decoded MSC multiplex frame and deframe the audio stream's access
    /// units (the codec consumes these). AAC: the FDK TT_DRM transport expects the DRM AAC CRC
    /// byte in front of each access unit. xHE-AAC: the USAC access unit, deframed by the
    /// stateful parser (its frames span super frames).
    /// In DRM+ each audio super frame covers two successive 100 ms logical frames.
    /// The frame index is recovered after cell deinterleaving; an orphaned second half
    /// cannot be decoded and must not be joined to the following pair.
    fn complete_audio_super_frame(&mut self, logical: &[u8], frame_index: usize) -> Option<Vec<u8>> {
        if self.mode != Some(RobustnessMode::E) {
            return Some(logical.to_vec());
        }
        if frame_index % 2 == 0 {
            self.mode_e_audio_half = Some(logical.to_vec());
            None
        } else {
            let mut first = self.mode_e_audio_half.take()?;
            first.extend_from_slice(logical);
            Some(first)
        }
    }

    fn deframe_audio(&mut self, msc_bits: &[u8], frame_index: usize) {
        use crate::digital::drm::audio::{demultiplex, parse_aac_super_frame, split_text_message, AacSuperFrameFormat};
        let Some(mux) = self.multiplex.clone() else { return };
        let Some(audio) = self.audio.clone() else { return };
        let text_flag = audio.text;
        let Some(stream) = mux.streams.first() else { return };
        let before = self.audio_access_units.len();
        for lf in demultiplex(msc_bits, &mux).into_iter().flatten() {
            if lf.stream_id != 0 {
                continue;
            }
            let logical = split_text_message(&lf.data, text_flag);
            let Some(super_frame) = self.complete_audio_super_frame(logical, frame_index) else { continue };
            if audio.coding == 3 {
                // xHE-AAC (MPEG-D USAC): the super frame's directory lists a variable number of
                // frame borders, and a frame may span super frames, so the parser is stateful.
                if let Some((_header, frames)) = self.xhe_deframer.push(&super_frame) {
                    for f in frames {
                        self.audio_access_units.push(f.access_unit().to_vec());
                    }
                }
                continue;
            }
            let frames = if self.mode == Some(RobustnessMode::E) {
                match audio.sample_rate {
                    3 => 5,  // 24 kHz core, 200 ms
                    4 => 10, // 48 kHz core, 200 ms
                    _ => continue,
                }
            } else {
                match audio.sample_rate {
                    1 => 5,  // 12 kHz core, 400 ms
                    3 => 10, // 24 kHz core, 400 ms
                    _ => continue,
                }
            };
            let format_stream = if self.mode == Some(RobustnessMode::E) {
                crate::digital::drm::sdc::StreamDescription {
                    len_a: stream.len_a.saturating_mul(2),
                    len_b: stream.len_b.saturating_mul(2),
                }
            } else {
                *stream
            };
            let fmt = AacSuperFrameFormat::aac(frames, &format_stream);
            if let Some(aus) = parse_aac_super_frame(&super_frame, &fmt) {
                for f in aus {
                    let mut au = Vec::with_capacity(f.data.len() + 1);
                    if let Some(c) = f.crc_byte {
                        au.push(c);
                    }
                    au.extend_from_slice(&f.data);
                    self.audio_access_units.push(au);
                }
            }
        }
        self.audio_unit_count += self.audio_access_units.len() - before;
        self.decode_audio();
    }

    /// The decoded audio PCM's sample rate, Hz (0 when the SDC audio info is unknown).
    pub fn audio_rate_hz(&self) -> u32 {
        let Some(a) = self.audio.as_ref() else { return 0 };
        if a.coding == 3 {
            // xHE-AAC uses its own sampling-rate table (ES 201 980 table 26): code 4 is
            // 24 kHz, not the AAC 48 kHz.
            return match a.sample_rate {
                0 => 9_600,
                1 => 12_000,
                2 => 16_000,
                3 => 19_200,
                4 => 24_000,
                5 => 32_000,
                6 => 38_400,
                7 => 48_000,
                _ => 0,
            };
        }
        let core = match a.sample_rate {
            0 => 8_000,
            1 => 12_000,
            2 => 16_000,
            3 => 24_000,
            4 => 48_000,
            _ => return 0,
        };
        if a.sbr { core * 2 } else { core }
    }

    /// Decode the deframed audio access units into PCM (wasm32 only; the FDK decoder is not
    /// linked natively). Both AAC (coding 0) and xHE-AAC (coding 3, MPEG-D USAC) go through the
    /// FDK `TT_DRM` decoder: the SDC type-9 bytes carry the xHE-AAC config after the two header
    /// bytes, and FDK's DRM transport handles USAC (DecDRM's `open_decoder` does the same).
    #[cfg(target_arch = "wasm32")]
    fn decode_audio(&mut self) {
        let Some(audio) = self.audio.clone() else { return };
        let type9 = audio.to_type9_bytes();
        if self.audio_decoder.is_none() || self.audio_configured_with.as_deref() != Some(type9.as_slice()) {
            self.audio_decoder = crate::fdk::AacDecoder::new();
            self.audio_configured_with = Some(type9.clone());
            self.audio_units_decoded = 0;
            if let Some(dec) = self.audio_decoder.as_mut() {
                dec.configure(&type9);
            }
        }
        if let Some(dec) = self.audio_decoder.as_mut() {
            let mut fed = 0usize;
            let mut produced = 0usize;
            for au in self.audio_access_units.iter().skip(self.audio_units_decoded) {
                let pcm = dec.decode(au);
                fed += 1;
                produced += pcm.len() / dec.last_stream_info.2.max(1) as usize;
                self.audio_pcm.extend(downmix_pcm(&pcm, dec.last_stream_info.2 as usize));
            }
            self.pcm_total += produced as u64;
            self.audio_units_decoded = self.audio_access_units.len();
            if fed > 0 {
                self.audio_debug = format!(
                    "au={} units={} err={} pcm={} total={} fed={} new={} sr={} fs={} ch={}",
                    self.audio_unit_count, self.audio_units_decoded, dec.last_error,
                    self.audio_pcm.len(), self.pcm_total, fed, produced,
                    dec.last_stream_info.0, dec.last_stream_info.1, dec.last_stream_info.2
                );
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn decode_audio(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_e_joins_only_matching_audio_halves() {
        let mut rx = DrmReceiver::new_at(96_000);
        rx.mode = Some(RobustnessMode::E);
        assert!(rx.complete_audio_super_frame(&[10, 11], 1).is_none());
        assert!(rx.complete_audio_super_frame(&[20, 21], 0).is_none());
        assert_eq!(rx.complete_audio_super_frame(&[22, 23], 1), Some(vec![20, 21, 22, 23]));
        assert!(rx.complete_audio_super_frame(&[30], 2).is_none());
        assert!(rx.complete_audio_super_frame(&[40], 0).is_none());
        assert_eq!(rx.complete_audio_super_frame(&[41], 1), Some(vec![40, 41]));
        assert!(rx.complete_audio_super_frame(&[43], 3).is_none());
        assert!(rx.complete_audio_super_frame(&[50], 2).is_none());
        assert_eq!(rx.complete_audio_super_frame(&[51], 3), Some(vec![50, 51]));
    }

    #[test]
    fn mode_e_uses_six_frame_interleaving_and_four_msc_frames() {
        let map = CellMap::new(RobustnessMode::E, SpectrumOccupancy::SO_0).unwrap();
        assert_eq!(map.msc_cells_per_frame, 7460);
        assert_eq!(map.mode().frames_per_superframe(), 4);
        let de = CellDeinterleaver::new(map.msc_cells_per_frame, 6);
        assert_eq!(de.depth(), 6);
        let indices: Vec<usize> = (0..8).map(|i| {
            (i + map.mode().frames_per_superframe() - (de.depth() - 1) % map.mode().frames_per_superframe())
                % map.mode().frames_per_superframe()
        }).collect();
        assert_eq!(indices, [3, 0, 1, 2, 3, 0, 1, 2]);
    }

    #[test]
    fn plugin_reports_fac_mer_and_estimated_snr() {
        let mut plugin = DrPlugin::new(48_000.0);
        let lines = plugin.process_iq(&load_iq("../tests/fixtures/drm/drm_modeB_so3_48k.f32"));
        assert!(lines.iter().any(|line| line.starts_with("FAC MER ")), "{lines:?}");
        assert!(lines.iter().any(|line| line.starts_with("FAC SNR ")), "{lines:?}");
        let snr = plugin.rx.snr_db().expect("FAC decisions must yield an SNR estimate");
        assert!(snr.is_finite() && (10.0..70.0).contains(&snr), "FAC SNR {snr:.1} dB");
    }

    #[test]
    fn stereo_pcm_is_mono_at_the_correct_duration() {
        assert_eq!(downmix_pcm(&[1000, 3000, -1000, -3000], 2), [2000, -2000]);
        assert_eq!(downmix_pcm(&[1000, -1000], 1), [1000, -1000]);
    }

    #[test]
    fn recovery_forgets_the_old_station_and_decoder_state() {
        let iq = load_iq("../tests/fixtures/drm/drm_live_modeB_so3_48828.f32");
        let mut rs = crate::ddc::resampler::ComplexResampler::new(48_828.125, 48_000.0);
        let mut baseband = Vec::new();
        rs.process_f32_into(&iq, &mut baseband);
        let mut rx = DrmReceiver::new();
        for block in baseband.chunks(3248 * 2) {
            rx.push(block);
            rx.run();
        }
        assert!(rx.locked());
        assert_eq!(rx.station_label.as_deref(), Some("SAN90 DRM BENCH"));
        assert!(rx.buf.len() <= 400_000, "baseband must not grow forever");
        assert!(rx.all_rows.is_empty() && rx.all_shifts.is_empty(), "processed rows must be released");
        let map = rx.map.clone().unwrap();
        rx.recover(&map);
        assert!(!rx.locked(), "a lost station must not remain locked");
        assert!(rx.station_label.is_none() && rx.audio.is_none() && rx.multiplex.is_none());
        assert!(rx.audio_access_units.is_empty() && rx.msc_frames.is_empty());
        assert!(rx.msc_prev_index.is_none());
        assert!(rx.facs.is_empty() && rx.fac_constellation.is_empty());
        assert_eq!(rx.processed_complex, 0);
    }

    #[test]
    fn a_dead_channel_releases_the_old_lock_without_fac_failures() {
        let iq = load_iq("../tests/fixtures/drm/drm_live_modeB_so3_48828.f32");
        let mut rs = crate::ddc::resampler::ComplexResampler::new(48_828.125, 48_000.0);
        let mut baseband = Vec::new();
        rs.process_f32_into(&iq, &mut baseband);
        let mut rx = DrmReceiver::new();
        for block in baseband.chunks(3248 * 2) {
            rx.push(block);
            rx.run();
        }
        assert!(rx.locked());
        let errors = rx.fac_errors;
        let silence = vec![0.0f32; 4 * 48_000 * 2];
        for block in silence.chunks(3248 * 2) {
            rx.push(block);
            rx.run();
        }
        assert!(!rx.locked(), "no timing windows must not leave a stale station lock");
        assert!(rx.station_label.is_none() && rx.audio.is_none());
        assert!(errors < 12, "this case needs the no-FAC timeout, not the CRC-failure path");
        for block in baseband.chunks(3248 * 2) {
            rx.push(block);
            rx.run();
        }
        assert!(rx.locked(), "a fresh station must be acquired without a manual reset");
        assert_eq!(rx.station_label.as_deref(), Some("SAN90 DRM BENCH"));
    }

    #[test]
    fn history_pruning_keeps_totals_and_the_audio_decode_cursor() {
        let iq = load_iq("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let mut rx = DrmReceiver::new();
        rx.push(&iq);
        rx.run();
        let fac = *rx.facs.first().unwrap();
        rx.facs.resize(300, fac);
        rx.msc_frames.resize(300, vec![1]);
        rx.audio_access_units.resize(1100, vec![1]);
        rx.audio_units_decoded = 1100;
        rx.fac_constellation.resize(2100, (1.0, 1.0));
        rx.good_facs = 300;
        rx.msc_frame_count = 300;
        rx.audio_unit_count = 1100;
        rx.prune_history();
        assert_eq!((rx.facs.len(), rx.msc_frames.len(), rx.audio_access_units.len()), (128, 128, 512));
        assert_eq!(rx.fac_constellation.len(), 1024);
        assert_eq!(rx.audio_units_decoded, 512);
        assert_eq!((rx.good_facs, rx.msc_frame_count, rx.audio_unit_count), (300, 300, 1100));
        assert!(rx.msc_frame_indices.len() <= 1, "placed FAC indices must not accumulate");
    }

    fn load_iq(path: &str) -> Vec<f32> {
        let raw = std::fs::read(path).expect("capture file");
        raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    }

    /// The skeleton end-to-end: the clean fixture's FAC decodes through the receiver (the first
    /// milestone of the task-9 integration; the MSC/audio stages land here next).
    #[test]
    fn receiver_decodes_the_clean_fixture_fac() {
        let iq = load_iq("../tests/fixtures/drm/drm_modeB_so3_48k.f32");
        let mut rx = DrmReceiver::new();
        rx.push(&iq);
        rx.run();
        eprintln!(
            "[rx] locked={} mode={:?} facs={} fac_errors={} symbols={} label={:?} msc_frames={}",
            rx.locked(), rx.mode, rx.facs.len(), rx.fac_errors, rx.symbols_demodulated, rx.station_label, rx.msc_frames.len()
        );
        assert!(rx.locked(), "the receiver must lock on the clean fixture");
        assert!(rx.facs.len() >= 8, "at least 8 FAC blocks, got {}", rx.facs.len());
        assert_eq!(rx.fac_errors, 0, "the clean fixture FAC must decode without CRC errors");
        assert_eq!(rx.station_label.as_deref(), Some("SAN90 DRM TEST"), "the SDC station label");
        assert!(!rx.msc_frames.is_empty(), "the MSC must decode multiplex frames");
        let ids: Vec<u8> = rx.facs.iter().map(|f| f.channel.frame_index).collect();
        eprintln!("[rx] FAC frame_index sequence: {ids:?}");
    }

    /// Feeding the receiver in 3248-sample blocks (the DSP worker's push size) must produce the
    /// same decode as one whole-buffer push. The committed bench capture pins it: the frame
    /// phase, the FAC blocks, the station label and the audio access units all match. This is
    /// the regression for the streaming model — the frame phase, the MSC super-frame assembly
    /// and the SDC decode each had a batch-only assumption that only showed up once the capture
    /// was fed in blocks (which is exactly what the wasm worker does), and they all read 0 FAC
    /// blocks while the batch decode read 64.
    #[test]
    fn the_block_fed_receiver_matches_the_batch_decode() {
        let raw = std::fs::read("../tests/fixtures/drm/drm_live_modeB_so3_48828.f32").expect("live");
        let iq: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut rs = crate::ddc::resampler::ComplexResampler::new(48_828.125, 48_000.0);
        let mut baseband: Vec<f32> = Vec::new();
        rs.process_f32_into(&iq, &mut baseband);

        let mut batch = DrmReceiver::new();
        batch.push(&baseband);
        batch.run();

        let mut streamed = DrmReceiver::new();
        for block in baseband.chunks(3248 * 2) {
            streamed.push(block);
            streamed.run();
        }
        eprintln!(
            "[stream] batch facs={}/{} label={:?} aus={} msc={} | block-fed facs={}/{} label={:?} aus={} msc={}",
            batch.facs.len(), batch.fac_errors, batch.station_label, batch.audio_access_units.len(), batch.msc_frames.len(),
            streamed.facs.len(), streamed.fac_errors, streamed.station_label, streamed.audio_access_units.len(), streamed.msc_frames.len()
        );
        assert!(batch.locked() && streamed.locked(), "both must lock");
        assert!(!batch.msc_frames.is_empty(), "the committed bench capture must decode MSC frames");
        assert!(!batch.audio_access_units.is_empty(), "the committed bench capture must deframe audio AUs");
        assert_eq!(streamed.facs.len(), batch.facs.len(), "FAC ok blocks");
        assert_eq!(streamed.fac_errors, batch.fac_errors, "FAC errors");
        assert_eq!(streamed.phase, batch.phase, "frame phase");
        assert_eq!(streamed.station_label, batch.station_label, "station label");
        assert_eq!(streamed.msc_frames.len(), batch.msc_frames.len(), "MSC frames");
        assert_eq!(streamed.audio_access_units.len(), batch.audio_access_units.len(), "audio AUs");
    }

    /// Mode E (DRM+, VHF) at 96 kHz: the receiver is fed a synthesised mode E signal (cyclic
    /// prefix + the mode's carriers with the map's reference pilots) and must select mode E and
    /// demodulate it. DecDRM defines no mode E transmitter, so a synthetic signal is the only
    /// way to exercise the 96 kHz front end (see the handoff).
    /// A PCM burst larger than the ABI's fixed output block must be handed over across several
    /// drains, not truncated: the DRM audio arrives once per super frame (~1.2 s at 38.4 kHz,
    /// ~46k samples) while the wasm block holds 32768, so a plain drain dropped ~30 % of every
    /// burst and the worklet's ring underran every second.
    #[test]
    fn drain_audio_pcm_keeps_the_tail() {
        let mut plugin = DrPlugin::new(48_000.0);
        plugin.rx.audio_pcm = (0..10).collect();
        assert_eq!(plugin.drain_audio_pcm(4), vec![0, 1, 2, 3]);
        assert_eq!(plugin.rx.audio_pcm, vec![4, 5, 6, 7, 8, 9], "the tail must stay buffered");
        assert_eq!(plugin.drain_audio_pcm(4), vec![4, 5, 6, 7]);
        assert_eq!(plugin.drain_audio_pcm(4), vec![8, 9], "the last call takes the remainder");
        assert!(plugin.rx.audio_pcm.is_empty());
    }

    #[test]
    fn mode_e_acquires_and_demodulates_a_synthetic_signal() {
        let map = CellMap::new(RobustnessMode::E, SpectrumOccupancy::SO_0).expect("mode E layout");
        let n = map.mode().fft_size();
        let g = map.mode().guard_len();
        let spf = map.mode().symbols_per_frame();
        let mut fac_symbols = vec![vec![Cplx::zero(); map.num_carriers]; map.symbols_per_superframe];
        let identities = [(0, 0), (1, 1), (1, 0), (2, 1)];
        for (frame, (identity, toggle)) in identities.into_iter().enumerate() {
            let bits = crate::digital::drm::fac::tests::mode_e_fac(identity, toggle, 3, 0);
            let encoded = MlcParams::fac_for(RobustnessMode::E).encode_test_single_level(&bits);
            let mut it = encoded.into_iter();
            for s in 0..spf {
                for &c in &map.fac_carriers[s] {
                    fac_symbols[frame * spf + s][c as usize] = it.next().unwrap();
                }
            }
            assert!(it.next().is_none());
        }
        let mut sdc_bits = vec![0u8; 4 + 113 * 8]; // AFS index + SDC entities
        // Type-0 multiplex entity: one stream, protection 0/0, 93 bytes in part B.
        let mut sdc_data = vec![0u8; 113];
        sdc_data[..9].copy_from_slice(&[0x06, 0x00, 0x00, 0x00, 0x5d,
                                         0x04, 0x90, 0x03, 0x00]); // type-9 AAC, 24 kHz, no SBR
        for (i, byte) in sdc_data.iter().enumerate() {
            for bit in (0..8).rev() { sdc_bits[4 + i * 8 + 7 - bit] = (byte >> bit) & 1; }
        }
        let mut crc = crate::digital::drm::fec::crc::Crc::crc16();
        crc.add_byte(0);
        crc.add_bytes(&sdc_data);
        for bit in (0..16).rev() { sdc_bits.push(((crc.value() >> bit) & 1) as u8); }
        sdc_bits.extend_from_slice(&[0; 6]); // padding to the 930-bit SDC block
        let coded_sdc = MlcParams::sdc_e(map.sdc_cells_per_superframe, 0)
            .encode_test_single_level(&sdc_bits);
        let mut sdc_symbols = vec![vec![Cplx::zero(); map.num_carriers]; map.symbols_per_superframe];
        let mut it = coded_sdc.into_iter();
        for s in 0..5 {
            for &c in &map.sdc_carriers[s] {
                sdc_symbols[s][c as usize] = it.next().unwrap();
            }
        }
        assert!(it.next().is_none());
        let p = MlcParams::msc_e(crate::digital::drm::fec::qam::Mapping::Qam4,
            map.msc_cells_per_frame,
            crate::digital::drm::fec::mlc::MscProtection { part_a: 0, part_b: 0, hierarchical: 0 }, 0);
        let audio_raw = std::fs::read("../tests/fixtures/drm/aac_sine_24k.drm").unwrap();
        let aus: Vec<&[u8]> = audio_raw.chunks_exact(36).collect();
        assert!(aus.len() >= 5);
        let audio_superframe = |pair: usize| -> Vec<u8> {
            let mut header = vec![0u8; 6];
            for border in 1..5 {
                let value = (border * 35) as u16;
                for bit in 0..12 {
                    let pos = (border - 1) * 12 + bit;
                    header[pos / 8] |= (((value >> (11 - bit)) & 1) as u8) << (7 - pos % 8);
                }
            }
            for i in 0..5 { header.push(aus[(pair * 5 + i) % aus.len()][0]); }
            for i in 0..5 { header.extend_from_slice(&aus[(pair * 5 + i) % aus.len()][1..]); }
            assert_eq!(header.len(), 186);
            header
        };
        let table = crate::digital::drm::fec::interleaver::permutation(map.msc_cells_per_frame, 5);
        let mut tx_mem = vec![vec![Cplx::zero(); map.msc_cells_per_frame]; 6];
        let mut tx_cur: Vec<usize> = (0..6).collect();
        let mut msc_symbols = vec![vec![Cplx::zero(); map.num_carriers]; 5 * map.symbols_per_superframe];
        let mut expected = Vec::new();
        for sf in 0..5 {
            let mut stream = Vec::new();
            for f in 0..4 {
                let index = sf * 4 + f;
                let mut seed = 0x9e37_79b9u32.wrapping_mul(index as u32 + 1);
                let mut bits: Vec<u8> = (0..p.total_bits()).map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed & 1) as u8
                }).collect();
                let sf = audio_superframe(index / 2);
                for (i, &byte) in sf[index % 2 * 93..(index % 2 + 1) * 93].iter().enumerate() {
                    for bit in (0..8).rev() { bits[i * 8 + 7 - bit] = (byte >> bit) & 1; }
                }
                let encoded = p.encode_test_single_level(&bits);
                expected.push(bits);
                tx_mem[tx_cur[0]].copy_from_slice(&encoded);
                stream.extend((0..map.msc_cells_per_frame)
                    .map(|i| tx_mem[tx_cur[i % 6]][table[i]]));
                for c in &mut tx_cur { *c = if *c == 0 { 5 } else { *c - 1 }; }
            }
            stream.extend_from_slice(&[Cplx::zero(); 2]); // MSC dummy cells
            let mut cells = stream.into_iter();
            for s in 0..map.symbols_per_superframe {
                for &c in &map.msc_carriers[s] {
                    msc_symbols[sf * map.symbols_per_superframe + s][c as usize] = cells.next().unwrap();
                }
            }
            assert!(cells.next().is_none());
        }
        let mut iq: Vec<f32> = Vec::new();
        for sym in 0..740usize {
            let mut window = vec![Cplx::zero(); n];
            for (t, w) in window.iter_mut().enumerate() {
                let mut acc = Cplx::zero();
                for c in 0..map.num_carriers {
                    let k = f64::from(map.kmin + c as i32);
                    let pilot = map.pilot(sym % map.symbols_per_superframe, c);
                    let v = if map.cell(sym % map.symbols_per_superframe, c).is_fac() {
                        fac_symbols[sym % map.symbols_per_superframe][c]
                    } else if map.cell(sym % map.symbols_per_superframe, c).is_sdc() {
                        sdc_symbols[sym % map.symbols_per_superframe][c]
                    } else if map.cell(sym % map.symbols_per_superframe, c).is_msc() {
                        msc_symbols[sym][c]
                    } else if pilot.norm_sqr() > 0.0 {
                        pilot
                    } else {
                        Cplx::from_polar(1.0, 0.3 * c as f64)
                    };
                    acc += v * Cplx::from_polar(1.0, 2.0 * core::f64::consts::PI * k * t as f64 / n as f64);
                }
                *w = acc;
            }
            for t in 0..g {
                iq.push(window[n - g + t].re as f32);
                iq.push(window[n - g + t].im as f32);
            }
            for w in &window {
                iq.push(w.re as f32);
                iq.push(w.im as f32);
            }
        }
        let mut rx = DrmReceiver::new_at(96_000);
        // Diagnostic: how many timing windows does the mode E TimeSync see, and at what score?
        {
            let cx: Vec<Cplx> = iq.chunks_exact(2).map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1]))).collect();
            let mut ts = TimeSync::new(RobustnessMode::E);
            let mut windows = 0usize;
            let mut best: f64 = 0.0;
            for block in cx.chunks(3248) {
                let _ = ts.push(block);
                while let Some(w) = ts.next_window() {
                    windows += 1;
                    best = best.max(w.guard_corr.unwrap_or(0.0));
                }
            }
            eprintln!("[modeE] timesync windows={windows} best_guard_corr={best:.3} samples={}", iq.len() / 2);
        }
        rx.push(&iq);
        rx.run();
        eprintln!("[modeE] locked={} mode={:?} symbols={} facs={} sdc={} mux={:?} msc={} phase={} buckets={:?} pending={} indices={:?} prev={:?} started={} rows={} demod_rows={}",
            rx.locked(), rx.mode, rx.symbols_demodulated, rx.facs.len(), rx.sdc_ok,
            rx.multiplex.as_ref().map(|m| (m.protection_a, m.protection_b, m.streams.clone())),
            rx.msc_frames.len(), rx.phase, rx.msc_super.iter().map(Vec::len).sum::<usize>(),
            rx.msc_frame_buf.len(), rx.msc_frame_indices, rx.msc_prev_index, rx.msc_started,
            rx.all_rows.len(), rx.demod_rows
        );
        eprintln!("[modeE] FAC indices={:?}; SDC={} MSC buckets per frame {:?}",
            rx.facs.iter().map(|f| f.channel.frame_index).collect::<Vec<_>>(),
            rx.sdc_ok,
            (0..4).map(|f| rx.msc_super[f*40..(f+1)*40].iter().map(Vec::len).sum::<usize>()).collect::<Vec<_>>());
        assert_eq!(rx.mode, Some(RobustnessMode::E), "mode detection must pick mode E at 96 kHz");
        assert!(rx.symbols_demodulated > 0, "the mode E demodulator must emit symbols");
        assert!(rx.facs.len() >= 5, "the synthetic mode E FAC should decode: good={} bad={}", rx.facs.len(), rx.fac_errors);
        assert!(rx.sdc_ok >= 1, "the synthetic mode E SDC should pass CRC");
        assert!(rx.multiplex.is_some(), "the SDC must describe the MSC stream");
        assert_eq!(rx.audio.as_ref().map(|a| (a.coding, a.sample_rate)), Some((0, 3)));
        assert!(rx.audio_access_units.len() >= 10, "mode E audio must deframe paired logical frames");
        assert!(rx.msc_frames.len() >= 4,
            "mode E MSC must decode after the six-frame interleaver fills (got {})", rx.msc_frames.len());
        let shift = (0..expected.len() - rx.msc_frames.len()).find(|&start| {
            rx.msc_frames.iter().enumerate().all(|(i, bits)| bits == &expected[start + i])
        });
        assert!(shift.is_some(), "mode E MSC must match consecutive transmitted frames bit-exactly");
        let mut streamed = DrmReceiver::new_at(96_000);
        for block in iq.chunks(3248 * 2) {
            streamed.push(block);
            streamed.run();
        }
        assert_eq!(streamed.facs, rx.facs, "mode E streaming FAC");
        assert_eq!(streamed.sdc_ok, rx.sdc_ok, "mode E streaming SDC");
        assert_eq!(streamed.msc_frames, rx.msc_frames, "mode E streaming MSC bits");
        assert_eq!(streamed.audio_access_units, rx.audio_access_units, "mode E streaming audio AUs");
        if let Ok(path) = std::env::var("WEBSA_DRM_MODE_E_FIXTURE") {
            let bytes: Vec<u8> = iq.iter().flat_map(|v| v.to_le_bytes()).collect();
            std::fs::write(path, bytes).expect("write mode E test signal");
        }
        for pair in rx.facs.windows(2) {
            assert_eq!(pair[1].channel.frame_index, (pair[0].channel.frame_index + 1) % 4);
        }
    }

    /// The full bench-capture decode through the receiver, fed in 3248-sample blocks exactly like
    /// the DSP worker feeds the wasm build: coarse acquisition, streaming frequency tracking,
    /// channel estimation, FAC/SDC/MSC and the audio deframing. This is the task-7 acceptance on
    /// the committed capture (6 s, HE-AAC mono 12 kHz core with SBR, station `SAN90 DRM BENCH`);
    /// it is what the browser's DRM audio path (`drmLiveAudio`) decodes.
    #[test]
    fn receiver_decodes_the_committed_bench_capture_end_to_end() {
        let raw = std::fs::read("../tests/fixtures/drm/drm_live_modeB_so3_48828.f32").expect("live");
        let iq: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut rs = crate::ddc::resampler::ComplexResampler::new(48_828.125, 48_000.0);
        let mut baseband: Vec<f32> = Vec::new();
        rs.process_f32_into(&iq, &mut baseband);
        let mut rx = DrmReceiver::new();
        for block in baseband.chunks(3248 * 2) {
            rx.push(block);
            rx.run();
        }
        eprintln!(
            "[rxbench] locked={} mode={:?} facs={} fac_errors={} label={:?} msc={} aus={} rate={}",
            rx.locked(), rx.mode, rx.facs.len(), rx.fac_errors, rx.station_label, rx.msc_frames.len(), rx.audio_access_units.len(), rx.audio_rate_hz()
        );
        assert!(rx.locked(), "the receiver must lock on the bench capture");
        assert_eq!(rx.station_label.as_deref(), Some("SAN90 DRM BENCH"), "station label");
        assert!(rx.fac_errors <= 2, "FAC errors {} on the clean bench capture", rx.fac_errors);
        assert!(!rx.msc_frames.is_empty(), "the MSC must decode multiplex frames");
        assert_eq!(rx.audio_rate_hz(), 24_000, "HE-AAC mono 12 kHz core with SBR");
        assert!(
            rx.audio_access_units.len() >= 30,
            "bench audio access units {} too few",
            rx.audio_access_units.len()
        );
    }

    /// The /tmp 30 s bench capture is a bonus (it was taken at an over-driven reference level,
    /// RMS ~64 vs the committed capture's ~9), so it only pins acquisition and the station
    /// label; the committed capture above is the audio acceptance.
    #[test]
    #[ignore = "live capture /tmp/live30.f32 required; run with --ignored --nocapture"]
    fn receiver_locks_the_long_live_capture() {
        let path = "/tmp/live30.f32";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let raw = std::fs::read(path).expect("capture file");
        let iq: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut rs = crate::ddc::resampler::ComplexResampler::new(48_828.125, 48_000.0);
        let mut baseband: Vec<f32> = Vec::new();
        rs.process_f32_into(&iq, &mut baseband);
        let mut rx = DrmReceiver::new();
        for block in baseband.chunks(3248 * 2) {
            rx.push(block);
            rx.run();
        }
        eprintln!(
            "[rxlive] locked={} mode={:?} facs={} fac_errors={} label={:?} msc={} aus={} rate={}",
            rx.locked(), rx.mode, rx.facs.len(), rx.fac_errors, rx.station_label, rx.msc_frames.len(), rx.audio_access_units.len(), rx.audio_rate_hz()
        );
        assert!(rx.locked(), "the receiver must lock on the live capture");
        assert_eq!(rx.station_label.as_deref(), Some("SAN90 DRM BENCH"), "station label");
    }
}

/// `DigitalDemodulator` wrapper that streams baseband into the drm `DrmReceiver` and reports
/// the decoded station label, mode and SNR once the receiver has locked. This replaces the
/// previous receiver's plugin (task-9): the wasm ABI and the readout stay as they are.
pub struct DrPlugin {
    rx: DrmReceiver,
    /// The baseband rate the receiver was built at, preserved across resets (a retune must not
    /// drop a DRM+ receiver back to 48 kHz).
    sample_rate: u32,
    /// The registry id: DRM30 and DRM+ share the receiver but have separate rate presets.
    plugin_id: &'static str,
    lines: Vec<String>,
    sent: Vec<String>,
    /// Blocks since the pipeline was built, for the throttled search status.
    status_blocks: u32,
    /// Blocks since the last locked report, so the readout returns after the operator clears
    /// the window instead of staying empty until the text changes.
    report_blocks: u32,
}

impl DrPlugin {
    /// Build the receiver at the rate the pipeline resamples the baseband to before it arrives:
    /// 48 kHz selects DRM30 modes A-D, 96 kHz selects mode E (DRM+). The pipeline's
    /// `set_digital_demod(demod, fs_in, rate)` converts the input to this rate, so an operator
    /// selecting a 96 kHz DRM+ preset reaches the mode E path.
    pub fn new(rate: f64) -> Self {
        let sample_rate = if rate.is_finite() && rate >= 48_000.0 {
            rate.round() as u32
        } else {
            48_000
        };
        Self {
            rx: DrmReceiver::new_at(sample_rate),
            sample_rate,
            plugin_id: "drm",
            lines: Vec::new(),
            sent: Vec::new(),
            status_blocks: 0,
            report_blocks: 0,
        }
    }
    pub fn new_plus() -> Self {
        let mut plugin = Self::new(96_000.0);
        plugin.plugin_id = "drmplus";
        plugin
    }
}

impl DigitalDemodulator for DrPlugin {
    fn id(&self) -> &'static str {
        self.plugin_id
    }

    fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
        self.rx.push(iq);
        self.rx.run();
        if !self.rx.locked() {
            // One status line per second while the search runs. Without it a receiver that
            // has not locked shows nothing at all, and a dead stream looks the same as a
            // search in progress.
            self.status_blocks += 1;
            if self.status_blocks % 15 != 0 {
                return Vec::new();
            }
            let seconds = self.rx.buffered() as f64 / f64::from(params::SAMPLE_RATE);
            let mode = self
                .rx
                .mode
                .map(|m| format!("{m:?}"))
                .unwrap_or_else(|| "no mode yet".to_string());
            let occupancy = format!("{:.0} kHz", self.rx.occupancy().bandwidth_khz());
            let line = format!(
                "DRM searching: {seconds:.1}s buffered, {mode} {occupancy}, carrier {:+.1} Hz, FAC errors {}",
                self.rx.carrier_offset_hz, self.rx.fac_errors
            );
            if self.sent.first() != Some(&line) {
                self.lines = vec![line.clone()];
                self.sent = vec![line.clone()];
                return vec![line];
            }
            return Vec::new();
        }
        let mut lines = Vec::new();
        if let Some(m) = self.rx.mode {
            lines.push(format!("locked: {m:?}, {:.0} kHz", self.rx.occupancy().bandwidth_khz()));
        }
        match &self.rx.station_label {
            Some(label) => lines.push(format!("station: {label}")),
            None => lines.push("station: (SDC not decoded yet)".to_string()),
        }
        if let Some(mer) = self.rx.chanest.as_ref().and_then(|e| e.stats.fac_mer_db) {
            lines.push(format!("FAC MER {mer:.1} dB"));
        }
        if let Some(snr) = self.rx.snr_db() {
            // Whole decibels: a tenth of a decibel changes on every block and the change
            // would post a report per block.
            lines.push(format!("FAC SNR {snr:.0} dB"));
        }
        lines.push(format!(
            "{} MSC frames, {} audio AUs, FAC ok {} err {}",
            self.rx.msc_frame_count,
            self.rx.audio_unit_count,
            self.rx.good_facs,
            self.rx.fac_errors
        ));
        if !self.rx.audio_debug.is_empty() {
            lines.push(format!("dbg {}", self.rx.audio_debug));
        }
        self.lines = lines.clone();
        self.report_blocks += 1;
        let changed = lines != self.sent;
        if changed || self.report_blocks % 75 == 0 {
            self.sent = lines;
            self.report_blocks = 0;
            return self.lines.clone();
        }
        Vec::new()
    }

    fn reset(&mut self) {
        self.rx = DrmReceiver::new_at(self.sample_rate);
        self.lines.clear();
        self.sent.clear();
        self.status_blocks = 0;
        self.report_blocks = 0;
    }

    fn last_report(&self) -> Option<DigitalReport> {
        self.rx.snr_db().map(|snr| DigitalReport {
            frequency_hz: self.rx.carrier_offset_hz,
            time_offset_s: 0.0,
            snr_db: snr,
        })
    }

    fn decoded(&self) -> Vec<(String, DigitalReport)> {
        let report = self
            .last_report()
            .unwrap_or(DigitalReport { frequency_hz: 0.0, time_offset_s: 0.0, snr_db: 0.0 });
        self.lines.iter().cloned().map(|line| (line, report)).collect()
    }

    fn buffered_input(&self) -> usize {
        self.rx.buffered()
    }

    fn constellation(&self) -> Vec<(f64, f64)> {
        self.rx.fac_constellation.clone()
    }

    fn take_audio_pcm(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.rx.audio_pcm)
    }

    fn drain_audio_pcm(&mut self, limit: usize) -> Vec<i16> {
        if self.rx.audio_pcm.len() <= limit {
            return std::mem::take(&mut self.rx.audio_pcm);
        }
        // Keep the tail: `split_off` leaves the first `limit` samples in the buffer, so hand those
        // back and put the remainder where the next call finds it.
        let tail = self.rx.audio_pcm.split_off(limit);
        std::mem::replace(&mut self.rx.audio_pcm, tail)
    }

    fn audio_rate_hz(&self) -> u32 {
        self.rx.audio_rate_hz()
    }
}
