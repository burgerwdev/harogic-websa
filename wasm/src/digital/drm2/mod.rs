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

/// The DRM receiver as a single streaming stage (the task-9 integration): baseband 48 kHz in,
/// FAC/SDC/MSC/audio out. It owns the stages' persistent state and the acquisition/tracking
/// state machine, mirroring DecDRM's `chain.rs`. This is the skeleton the closed timing/SRO
/// loop hangs off; the FAC decode is the first end-to-end milestone.
pub struct DrmReceiver {
    buf: Vec<f32>,
    pushed: u64,
    mode: Option<RobustnessMode>,
    map: Option<CellMap>,
    chanest: Option<ChanEst>,
    fac_dec: Option<MlcDecoder>,
    fac_cells: Vec<EqCell>,
    frame_phase: usize,
    /// SDC cells of the current frame (symbols 0..sdc_syms), stored with the frame's FAC
    /// index once the FAC decodes.
    frame_sdc: Vec<EqCell>,
    sdc_blocks: Vec<(u8, Vec<EqCell>)>,
    /// MSC cells per emitted symbol (out_sym, cells) and the FAC frame indices, for the
    /// super-frame assembly after the pass.
    msc_emitted: Vec<(usize, Vec<EqCell>)>,
    msc_frame_indices: Vec<u8>,
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
            frame_sdc: Vec::new(),
            sdc_blocks: Vec::new(),
            msc_emitted: Vec::new(),
            msc_frame_indices: Vec::new(),
            msc_frames: Vec::new(),
        }
    }

    pub fn push(&mut self, iq: &[f32]) {
        self.buf.extend_from_slice(iq);
        self.pushed += iq.len() as u64 / 2;
    }

    pub fn locked(&self) -> bool {
        self.map.is_some()
    }

    /// One decode pass over the buffered baseband. Idempotent: returns once, and the caller
    /// calls it again when more samples arrive. The first pass acquires; later passes reuse the
    /// persistent state (the chanest, the FAC decoder, the timing/SRO tracking).
    pub fn run(&mut self) {
        // Mode detection (crude): the DRM30 mode whose guard correlation yields the most
        // windows wins; the full decimated mode detector replaces this in the finished chain.
        let mut best: Option<(RobustnessMode, CellMap, Vec<Vec<Cplx>>, Vec<i64>)> = None;
        let mut best_rows = 0usize;
        for m in RobustnessMode::DRM30 {
            let Some(map) = CellMap::new(m, SpectrumOccupancy::SO_3) else { continue };
            let iq: Vec<Cplx> = self
                .buf
                .chunks_exact(2)
                .map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1])))
                .collect();
            let mut tsync = TimeSync::new(m);
            let mut demod = OfdmDemod::new(&map);
            let mut cells = Vec::new();
            let mut rows: Vec<Vec<Cplx>> = Vec::new();
            let mut shifts = Vec::new();
            for block in iq.chunks(3248) {
                let _ = tsync.push(block);
                while let Some(w) = tsync.next_window() {
                    if w.guard_corr.unwrap_or(0.0) < 0.5 {
                        continue;
                    }
                    demod.demodulate(&w.samples, &mut cells);
                    rows.push(cells.clone());
                    shifts.push(w.shift);
                }
            }
            if rows.len() > best_rows {
                best_rows = rows.len();
                best = Some((m, map, rows, shifts));
            }
        }
        let Some((mode, map, rows, _)) = best else { return };
        if rows.len() < mode.symbols_per_frame() {
            return;
        }
        let phase = crate::digital::drm2::framesync::FrameSync::new(&map).search(&rows).phase;
        let spf = mode.symbols_per_frame();

        // Channel estimation, then the FAC/SDC/MSC, interleaved with the demodulation so the
        // tracker's timing/SRO corrections can feed back into the TimeSync.
        let est = self.chanest.get_or_insert_with(|| ChanEst::new(&map));
        let fac_dec = self
            .fac_dec
            .get_or_insert_with(|| MlcDecoder::new(MlcParams::fac(), 0));
        let iq: Vec<Cplx> = self
            .buf
            .chunks_exact(2)
            .map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1])))
            .collect();
        let mut tsync = TimeSync::new(mode);
        let mut demod = OfdmDemod::new(&map);
        let mut cells = Vec::new();
        let mut n = 0usize;
        let mut bits = Vec::new();
        for block in iq.chunks(3248) {
            let _ = tsync.push(block);
            while let Some(w) = tsync.next_window() {
                if w.guard_corr.unwrap_or(0.0) < 0.5 {
                    continue;
                }
                demod.demodulate(&w.samples, &mut cells);
                let sym = (n % spf + phase) % spf;
                n += 1;
                let Some((out_sym, out)) = est.process(&cells, sym, w.shift, &map) else { continue };
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
                            // Enter time-Wiener tracking after the first good FAC, then the
                            // external timing tracking after the second plus a two-good-FAC
                            // countdown (Dream's enter_tracking / enter_timing_tracking).
                            if self.good_facs == 1 && !self.tracking {
                                self.tracking = true;
                                est.start_time_wiener_tracking();
                            }
                            if self.good_facs >= 2 && !self.timing_tracking {
                                if self.delayed_cnt > 0 {
                                    self.delayed_cnt -= 1;
                                } else {
                                    self.timing_tracking = true;
                                    est.start_timing_tracking();
                                    tsync.stop_timing_acquisition();
                                }
                            }
                            self.facs.push(f);
                        }
                        None => self.fac_errors += 1,
                    }
                    self.sdc_blocks.push((idx, std::mem::take(&mut self.frame_sdc)));
                    self.msc_frame_indices.push(idx);
                    self.fac_cells.clear();
                }
                if self.timing_tracking {
                    let ta = est.last_track.timing_adjust;
                    let sro = est.last_track.sro_delta_hz;
                    if ta != 0 {
                        tsync.adjust_timing(ta as f64);
                    }
                    if sro != 0.0 {
                        tsync.adjust_sro(sro);
                    }
                }
            }
        }
        self.mode = Some(mode);
        self.frame_phase = phase;
        self.decode_sdc(&map);
        self.decode_msc(&map);
        self.map = Some(map);
    }

    /// Assemble the MSC super frames from the emitted cells and decode them (cell deinterleave
    /// + MLCC). The multiplex-frame boundary is every N_MUX cells, not 15 symbols, so the
    /// sym-order concatenation is chunked by cell count exactly as the bit-exact test does.
    fn decode_msc(&mut self, map: &CellMap) {
        use crate::digital::drm2::fac::MscMode;
        use crate::digital::drm2::fec::mlc::MscProtection;
        use crate::digital::drm2::fec::qam::Mapping;
        let mapping = match MscMode::Qam64Sm {
            MscMode::Qam64Sm => Mapping::Qam64Sm,
            MscMode::Qam64HmMix => Mapping::Qam64HmMix,
            MscMode::Qam64HmSym => Mapping::Qam64HmSym,
            MscMode::Qam16Sm => Mapping::Qam16,
        };
        let params = MlcParams::msc(mapping, map.msc_cells_per_frame, MscProtection { part_a: 0, part_b: 1, hierarchical: 0 }, 0);
        let mut de = CellDeinterleaver::new(map.msc_cells_per_frame, 5);
        let mut dec = MlcDecoder::new(params, 1);
        let mut super_msc: Vec<Vec<EqCell>> = vec![Vec::new(); 45];
        let mut in_partial = true;
        let mut complete_frame = 0usize;
        let mut bits = Vec::new();
        for (out_sym, cells) in &self.msc_emitted {
            if *out_sym == 0 && in_partial {
                in_partial = false;
                complete_frame = 0;
            } else if *out_sym == 0 {
                complete_frame += 1;
            }
            if in_partial {
                continue;
            }
            let Some(&frame_index) = self.msc_frame_indices.get(complete_frame) else { continue };
            if frame_index == 0xFF {
                continue;
            }
            let super_sym = frame_index as usize * 15 + *out_sym;
            if *out_sym == 0
                && frame_index == 0
                && (0..45).all(|s| map.msc_carriers[s].is_empty() || !super_msc[s].is_empty())
            {
                let mut all: Vec<EqCell> = Vec::new();
                for c in super_msc.iter() {
                    all.extend_from_slice(c);
                }
                for frame in all.chunks(map.msc_cells_per_frame).take(3) {
                    if let Some(d) = de.push(frame) {
                        if d.iter().all(|c| c.chan > 0.0) {
                            if dec.decode(&d, &mut bits) {
                                self.msc_frames.push(bits.clone());
                            }
                        }
                    }
                }
                for c in super_msc.iter_mut() {
                    c.clear();
                }
            }
            for &c in &map.msc_carriers[super_sym] {
                super_msc[super_sym].push(cells[c as usize]);
            }
        }
    }

    /// Decode the SDC blocks collected so far (the frame-0 block of each super frame) into the
    /// station label.
    fn decode_sdc(&mut self, map: &CellMap) {
        use crate::digital::drm2::fec::qam::Mapping;
        use crate::digital::drm2::sdc::{parse_entities, parse_sdc_block};
        let mut bits = Vec::new();
        for (idx, cells) in &self.sdc_blocks {
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
                let label = entities.iter().find_map(|e| match e {
                    crate::digital::drm2::sdc::Entity::Label(l) => Some(l.text()),
                    _ => None,
                });
                self.station_label = label;
            }
        }
    }
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

    /// The receiver on the live capture (resampled to the core rate): the closed timing/SRO
    /// loop is what this exercises. FAC blocks decoded is the proxy for the MER.
    #[test]
    #[ignore = "live capture; run with --ignored --nocapture"]
    fn receiver_on_live_capture() {
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
        let mut out: Vec<f32> = Vec::new();
        rs.process_f32_into(&iq, &mut out);
        // Normalise the live signal to the clean fixture's RMS: the raw capture is ~254x
        // hotter, which overdrives the receiver and is not a receiver defect.
        let rms: f32 = (out.iter().map(|v| v * v).sum::<f32>() / out.len() as f32).sqrt();
        let target = 0.178_f32;
        let scale = target / rms.max(1e-9);
        for v in out.iter_mut() {
            *v *= scale;
        }
        let mut rx = DrmReceiver::new();
        rx.push(&out);
        rx.run();
        eprintln!(
            "[rxlive] locked={} mode={:?} facs={} fac_errors={} symbols={} label={:?} msc={} timing_tracking={}",
            rx.locked(), rx.mode, rx.facs.len(), rx.fac_errors, rx.symbols_demodulated, rx.station_label, rx.msc_frames.len(), rx.timing_tracking
        );
    }
}
