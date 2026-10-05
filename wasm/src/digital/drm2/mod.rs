//! The DRM receiver, re-implemented in Rust along Dream's stage structure.
//!
//! Why a new chain rather than more patches to the previous receiver: the previous one found
//! its super-frame phase once and kept it, estimated the channel per symbol with linear
//! interpolation and had no timing or carrier tracking. On one bench capture the reference
//! receiver (DecDRM's `decdrm rx`, which ports Dream's stages) decoded 200 audio frames where
//! the previous chain managed 55, with 9 FAC errors against our 52 — the deficit is in the
//! chain's structure, not in tunable constants.
//!
//! This module therefore follows Dream's stage order, each stage a plain Rust type fed by the
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
//! PCM path and the backend's baseband AGC all stay as they are. The previous chain is deleted
//! once this one decodes (see `docs/en/DRM_HANDOFF.md`).

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

use crate::digital::drm2::cellmap::CellMap;
use crate::digital::drm2::chanest::ChanEst;
use crate::digital::drm2::fac::Fac;
use crate::digital::drm2::fec::mlc::{MlcDecoder, MlcParams};
use crate::digital::drm2::fec::qam::EqCell;
use crate::digital::drm2::interleave::CellDeinterleaver;
use crate::digital::drm2::ofdm::OfdmDemod;
use crate::digital::drm2::params::{RobustnessMode, SpectrumOccupancy};
use crate::digital::drm2::sync::timesync::TimeSync;
use crate::digital::drm2::dsp::Cplx;
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
    msc_decoder_key: Option<(crate::digital::drm2::fec::qam::Mapping, usize, usize, usize)>,
    msc_bits: Vec<u8>,
    /// Set once the first `out_sym == 0` boundary is seen, so leading mid-frame symbols are
    /// not mistaken for a frame.
    msc_boundary_seen: bool,
    /// Set once a frame with FAC index 0 (the start of a super frame) is placed. Assembly waits
    /// for it: the first frame boundary after the warm-up is often frame 1 or 2 of a super
    /// frame, and starting there puts the earlier super frame's cells into the wrong buckets.
    msc_started: bool,
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
    /// Acquisition/tracking state (Dream's RxState).
    tracking: bool,
    timing_tracking: bool,
    good_facs: usize,
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
    multiplex: Option<crate::digital::drm2::sdc::MultiplexDescription>,
    audio: Option<crate::digital::drm2::sdc::AudioInfo>,
    /// AAC access units deframed from the audio stream (ready for the codec).
    pub audio_access_units: Vec<Vec<u8>>,
    /// Decoded audio PCM (interleaved i16), wasm32 only.
    pub audio_pcm: Vec<i16>,
    /// Audio units already decoded, so a later pass decodes only the new ones.
    audio_units_decoded: usize,
    audio_debug: String,
    #[cfg(target_arch = "wasm32")]
    audio_decoder: Option<crate::fdk::AacDecoder>,
    #[cfg(target_arch = "wasm32")]
    xhe_decoder: Option<crate::xaac::XaacDecoder>,
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
    nco: Option<crate::digital::drm2::sync::nco::Nco>,
    ft: Option<crate::digital::drm2::sync::freqtrack::FreqTrack>,
    tsync: Option<TimeSync>,
    /// Streaming frame-phase accumulator (the block-fed counterpart of `FrameSync::search`).
    fs_acq: Option<crate::digital::drm2::framesync::FramePhaseAcquisition>,
    demod: Option<OfdmDemod>,
    spf: usize,
    phase: usize,
    sym_count: usize,
    freq_track: f64,
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
            audio_access_units: Vec::new(),
            audio_pcm: Vec::new(),
            audio_units_decoded: 0,
            audio_debug: String::new(),
            #[cfg(target_arch = "wasm32")]
            audio_decoder: None,
            #[cfg(target_arch = "wasm32")]
            xhe_decoder: None,
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
            msc_complete_frame: 0,
            msc_frame_buf: Vec::new(),
            sdc_pending: Vec::new(),
            msc_frames: Vec::new(),
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
            phase: 0,
            sym_count: 0,
            freq_track: 0.0,
        }
    }

    pub fn push(&mut self, iq: &[f32]) {
        self.buf.extend_from_slice(iq);
        self.pushed += iq.len() as u64 / 2;
    }

    pub fn locked(&self) -> bool {
        self.map.is_some()
    }

    /// The complex baseband samples buffered for the next decode attempt.
    pub fn buffered(&self) -> usize {
        self.buf.len() / 2
    }

    /// The channel estimator's FAC SNR, dB (None before the first frame).
    pub fn snr_db(&self) -> Option<f64> {
        self.chanest.as_ref().and_then(|e| e.stats.snr_db)
    }

    /// The spectrum occupancy this receiver decodes.
    pub fn occupancy(&self) -> SpectrumOccupancy {
        SpectrumOccupancy::SO_3
    }

    /// Incremental decode: each call demodulates the samples that arrived since the previous
    /// call and feeds them through the channel estimation and the FAC/SDC/MSC decode. The
    /// first call (when enough samples are buffered) also runs the mode detection and the
    /// coarse carrier acquisition, so later calls only carry the new samples.
    pub fn run(&mut self) {
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
            for m in RobustnessMode::DRM30 {
                let Some(cmap) = CellMap::new(m, SpectrumOccupancy::SO_3) else { continue };
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
                }
            }
            let Some((mode, cmap)) = best else { return };

            // Coarse carrier acquisition.
            let flat: Vec<f64> = self
                .buf
                .chunks_exact(2)
                .flat_map(|c| [f64::from(c[0]), f64::from(c[1])])
                .collect();
            let coarse = crate::digital::drm2::sync::freqacq::FreqAcquisition::new(true)
                .push_iq(&flat)
                .map(|a| a.dc_hz)
                .unwrap_or(0.0);

            self.carrier_offset_hz = coarse;
            self.coarse = coarse;
            self.freq_track = coarse;
            self.mode = Some(mode);
            self.map = Some(cmap.clone());
            self.spf = mode.symbols_per_frame();
            self.nco = Some(crate::digital::drm2::sync::nco::Nco::new(coarse));
            let mut ft = crate::digital::drm2::sync::freqtrack::FreqTrack::new(&cmap);
            ft.set_freq_time_constant(0.1);
            self.ft = Some(ft);
            self.tsync = Some(TimeSync::new(mode));
            self.fs_acq = Some(crate::digital::drm2::framesync::FramePhaseAcquisition::new(&cmap));
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
                let mut mixed = block.to_vec();
                nco.process(&mut mixed);
                let _ = ts.push(&mixed);
                while let Some(w) = ts.next_window() {
                    if w.guard_corr.unwrap_or(0.0) < 0.5 {
                        continue;
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
                    if self.sym_count == 45 {
                        ft.set_freq_time_constant(1.0);
                    }
                }
            }
        }
        self.processed_complex = self.buf.len() / 2;

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
            .get_or_insert_with(|| MlcDecoder::new(MlcParams::fac(), 0));

        // Take the unprocessed rows out of the receiver so the decode can borrow the other
        // fields freely; the phase-adjusted symbol index and the timing shift ride along.
        let rows: Vec<(usize, Vec<Cplx>, i64)> = (self.demod_rows..self.all_rows.len())
            .map(|i| (i, self.all_rows[i].clone(), self.all_shifts[i]))
            .collect();
        self.demod_rows = self.all_rows.len();

        {
            let est = self.chanest.as_mut().unwrap();
            let mut bits = Vec::new();
            for (i, row, shift) in rows {
                let sym = (i % self.spf + self.phase) % self.spf;
                let Some((out_sym, out)) = est.process(&row, sym, shift, map) else { continue };
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
                if self.fac_cells.len() == 65 {
                    let decoded = fac_dec.decode(&self.fac_cells, &mut bits);
                    let idx = if decoded {
                        Fac::parse(&bits).map(|f| f.channel.frame_index).unwrap_or(0xFF)
                    } else {
                        0xFF
                    };
                    match if decoded { Fac::parse(&bits) } else { None } {
                        Some(f) => {
                            self.good_facs += 1;
                            self.tracking = true;
                            if self.good_facs >= 2 && !self.timing_tracking {
                                if self.delayed_cnt > 0 {
                                    self.delayed_cnt -= 1;
                                } else {
                                    self.timing_tracking = true;
                                    est.start_timing_tracking();
                                }
                            }
                            self.facs.push(f);
                        }
                        None => self.fac_errors += 1,
                    }
                    self.sdc_pending.push((idx, std::mem::take(&mut self.frame_sdc)));
                    self.msc_frame_indices.push(idx);
                    self.fac_cells.clear();
                }
            }
        }
        self.decode_sdc(map);
        self.decode_msc(map);
    }

    /// Assemble the MSC super frames from the emitted cells and decode them (cell deinterleave
    /// + MLCC). The multiplex-frame boundary is every N_MUX cells, not 15 symbols, so the
    /// sym-order concatenation is chunked by cell count exactly as the bit-exact test does.
    fn decode_msc(&mut self, map: &CellMap) {
        use crate::digital::drm2::fac::MscMode;
        use crate::digital::drm2::fec::mlc::MscProtection;
        use crate::digital::drm2::fec::qam::Mapping;
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
        let params = MlcParams::msc(mapping, map.msc_cells_per_frame, prot, part_a_bytes);
        // Persistent stages: the deinterleaver holds the long-interleaving memory, so it must
        // survive across calls; the MLC decoder is rebuilt only when the configuration changes.
        if self.msc_deinterleaver.is_none() {
            self.msc_deinterleaver = Some(CellDeinterleaver::new(map.msc_cells_per_frame, 5));
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
        let mut decoded: Vec<Vec<u8>> = Vec::new();
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
        for b in decoded {
            self.deframe_audio(&b);
            self.msc_frames.push(b);
        }
    }

    /// Place one completed frame's cells into the super-frame buckets, decoding a super frame
    /// when its next one begins. `buf` is the frame's `(out_sym, cells)` in emitted order; its
    /// FAC index is looked up here because a full frame has passed since those symbols arrived.
    fn assemble_msc_frame(
        &mut self,
        map: &CellMap,
        buf: &[(usize, Vec<EqCell>)],
        decoded: &mut Vec<Vec<u8>>,
    ) {
        let cf = self.msc_complete_frame;
        let Some(&frame_index) = self.msc_frame_indices.get(cf) else {
            return;
        };
        self.msc_complete_frame += 1;
        if frame_index == 0xFF {
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
        }
        if frame_index == 0
            && (0..45).all(|s| map.msc_carriers[s].is_empty() || !self.msc_super[s].is_empty())
        {
            let mut all: Vec<EqCell> = Vec::new();
            for c in self.msc_super.iter() {
                all.extend_from_slice(c);
            }
            for frame in all.chunks(map.msc_cells_per_frame).take(3) {
                if let Some(d) = self.msc_deinterleaver.as_mut().expect("created").push(frame) {
                    if d.iter().all(|c| c.chan > 0.0)
                        && self.msc_decoder.as_mut().expect("created").decode(&d, &mut self.msc_bits)
                    {
                        decoded.push(self.msc_bits.clone());
                    }
                }
            }
            for c in self.msc_super.iter_mut() {
                c.clear();
            }
        }
        for (out_sym, cells) in buf {
            let super_sym = frame_index as usize * 15 + out_sym;
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
        use crate::digital::drm2::fec::qam::Mapping;
        use crate::digital::drm2::sdc::{parse_entities, parse_sdc_block};
        let pending = std::mem::take(&mut self.sdc_pending);
        let mut bits = Vec::new();
        for (idx, cells) in &pending {
            if *idx != 0 || cells.len() != map.sdc_cells_per_superframe {
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
                self.sdc_ok += 1;
                let entities = parse_entities(&b.data);
                for e in &entities {
                    match e {
                        crate::digital::drm2::sdc::Entity::Label(l) => {
                            if self.station_label.is_none() {
                                self.station_label = Some(l.text());
                            }
                        }
                        crate::digital::drm2::sdc::Entity::Multiplex(m) => {
                            if self.multiplex.is_none() {
                                self.multiplex = Some(m.clone());
                            }
                        }
                        crate::digital::drm2::sdc::Entity::Audio(a) => {
                            if self.audio.is_none() {
                                self.audio = Some(a.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Demultiplex one decoded MSC multiplex frame and deframe the audio stream's AAC access
    /// units (the codec consumes these). The FDK TT_DRM transport expects the DRM AAC CRC byte
    /// in front of each access unit.
    fn deframe_audio(&mut self, msc_bits: &[u8]) {
        use crate::digital::drm2::audio::{demultiplex, parse_aac_super_frame, split_text_message, AacSuperFrameFormat};
        let Some(mux) = self.multiplex.clone() else { return };
        let frames = match self.audio.as_ref().map(|a| a.sample_rate) {
            Some(1) => 5,  // 12 kHz
            Some(3) => 10, // 24 kHz
            _ => return,
        };
        let Some(stream) = mux.streams.first() else { return };
        let fmt = AacSuperFrameFormat::aac(frames, stream);
        let text_flag = self.audio.as_ref().map(|a| a.text).unwrap_or(false);
        for lf in demultiplex(msc_bits, &mux).into_iter().flatten() {
            if lf.stream_id == 0 {
                let super_frame = split_text_message(&lf.data, text_flag);
                if let Some(aus) = parse_aac_super_frame(super_frame, &fmt) {
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
        }
        self.decode_audio();
    }

    /// The decoded audio PCM's sample rate, Hz (0 when the SDC audio info is unknown).
    pub fn audio_rate_hz(&self) -> u32 {
        let Some(a) = self.audio.as_ref() else { return 0 };
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

    /// Decode the deframed audio access units into PCM (wasm32 only; the FDK AAC and libxaac
    /// decoders are not linked natively). AAC (coding 0) goes through the FDK TT_DRM decoder;
    /// xHE-AAC (coding 3, MPEG-D USAC) goes through libxaac.
    #[cfg(target_arch = "wasm32")]
    fn decode_audio(&mut self) {
        let Some(audio) = self.audio.clone() else { return };
        if audio.coding == 3 {
            if self.xhe_decoder.is_none() {
                let mut dec = crate::xaac::XaacDecoder::new();
                if let Some(d) = dec.as_mut() {
                    if !audio.xhe_aac_config.is_empty() {
                        d.feed(&audio.xhe_aac_config, true);
                    }
                }
                self.xhe_decoder = dec;
                self.audio_units_decoded = 0;
            }
            if let Some(dec) = self.xhe_decoder.as_mut() {
                for au in self.audio_access_units.iter().skip(self.audio_units_decoded) {
                    if let Some(pcm) = dec.feed(au, false) {
                        self.audio_pcm.extend_from_slice(&pcm);
                    }
                }
                self.audio_units_decoded = self.audio_access_units.len();
            }
            return;
        }
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
            for au in self.audio_access_units.iter().skip(self.audio_units_decoded) {
                let pcm = dec.decode(au);
                self.audio_pcm.extend_from_slice(&pcm);
            }
            self.audio_units_decoded = self.audio_access_units.len();
            self.audio_debug = format!("au={} units={} err={} pcm={}", self.audio_access_units.len(), self.audio_units_decoded, dec.last_error, self.audio_pcm.len());
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn decode_audio(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

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

/// `DigitalDemodulator` wrapper that streams baseband into the drm2 `DrmReceiver` and reports
/// the decoded station label, mode and SNR once the receiver has locked. This replaces the
/// previous receiver's plugin (task-9): the wasm ABI and the readout stay as they are.
pub struct DrPlugin {
    rx: DrmReceiver,
    lines: Vec<String>,
    sent: Vec<String>,
    /// Blocks since the pipeline was built, for the throttled search status.
    status_blocks: u32,
    /// Blocks since the last locked report, so the readout returns after the operator clears
    /// the window instead of staying empty until the text changes.
    report_blocks: u32,
}

impl DrPlugin {
    /// The receiver works at 48 kHz; the pipeline resamples the baseband to the
    /// decoder's rate before it reaches here.
    pub fn new(_rate: f64) -> Self {
        Self {
            rx: DrmReceiver::new(),
            lines: Vec::new(),
            sent: Vec::new(),
            status_blocks: 0,
            report_blocks: 0,
        }
    }
}

impl DigitalDemodulator for DrPlugin {
    fn id(&self) -> &'static str {
        "drm"
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
            lines.push(format!(
                "locked: {m:?}, {:.0} kHz, {} symbols",
                self.rx.occupancy().bandwidth_khz(),
                self.rx.symbols_demodulated,
            ));
        }
        match &self.rx.station_label {
            Some(label) => lines.push(format!("station: {label}")),
            None => lines.push("station: (SDC not decoded yet)".to_string()),
        }
        if let Some(snr) = self.rx.snr_db() {
            // Whole decibels: a tenth of a decibel changes on every block and the change
            // would post a report per block.
            lines.push(format!("FAC SNR {snr:.0} dB"));
        }
        lines.push(format!(
            "{} MSC frames, {} audio AUs, FAC ok {} err {}",
            self.rx.msc_frames.len(),
            self.rx.audio_access_units.len(),
            self.rx.facs.len(),
            self.rx.fac_errors
        ));
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
        self.rx = DrmReceiver::new();
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

    fn audio_rate_hz(&self) -> u32 {
        self.rx.audio_rate_hz()
    }
}
