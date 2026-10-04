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
        let Some((mode, map, _, _)) = best else { return };

        // Coarse carrier acquisition, then re-demodulate the selected mode with the streaming
        // NCO re-tuned every symbol by the frequency tracker. The time-domain correction is
        // what a live signal needs: a post-FFT rotation cannot undo the inter-carrier
        // interference the drifting residual offset bakes into the cells.
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
        let iq: Vec<Cplx> = self
            .buf
            .chunks_exact(2)
            .map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1])))
            .collect();
        let mut nco = crate::digital::drm2::sync::nco::Nco::new(coarse);
        let mut ft = crate::digital::drm2::sync::freqtrack::FreqTrack::new(&map);
        ft.set_freq_time_constant(0.1);
        let mut tsync = TimeSync::new(mode);
        let mut demod = OfdmDemod::new(&map);
        let mut cells = Vec::new();
        let mut rows: Vec<Vec<Cplx>> = Vec::new();
        let mut shifts: Vec<i64> = Vec::new();
        let mut track = coarse;
        let mut n = 0usize;
        for block in iq.chunks(3248) {
            let mut mixed = block.to_vec();
            nco.process(&mut mixed);
            let _ = tsync.push(&mixed);
            while let Some(w) = tsync.next_window() {
                if w.guard_corr.unwrap_or(0.0) < 0.5 {
                    continue;
                }
                demod.demodulate(&w.samples, &mut cells);
                rows.push(cells.clone());
                shifts.push(w.shift);
                let o = ft.process(&cells, w.shift);
                track += o.freq_delta_hz;
                nco.set_offset(track);
                n += 1;
                if n == 45 {
                    ft.set_freq_time_constant(1.0);
                }
            }
        }
        if rows.len() < mode.symbols_per_frame() {
            return;
        }
        let phase = crate::digital::drm2::framesync::FrameSync::new(&map).search(&rows).phase;
        let spf = mode.symbols_per_frame();

        // Channel estimation and FAC/SDC/MSC over the tracked rows. The time-Wiener is chosen
        // up front (the reference's estimator always uses it) and its Doppler adaptation turns
        // on after the first frame of history, as in the tracked chanest harness.
        if self.chanest.is_none() {
            let mut est = ChanEst::new(&map);
            est.use_time_wiener();
            self.chanest = Some(est);
        }
        let est = self.chanest.as_mut().unwrap();
        let fac_dec = self
            .fac_dec
            .get_or_insert_with(|| MlcDecoder::new(MlcParams::fac(), 0));
        // Channel estimation and FAC/SDC/MSC over the demodulated rows the mode-detection
        // pass just produced (the chanest harness's bit-exact path). Re-demodulating here
        // re-runs the timing acquisition and can differ by a border-case window, which shifts
        // the FAC frame alignment; the stored rows are already the windows we want.
        let est = self.chanest.get_or_insert_with(|| ChanEst::new(&map));
        let fac_dec = self
            .fac_dec
            .get_or_insert_with(|| MlcDecoder::new(MlcParams::fac(), 0));
        let mut bits = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if i == 45 {
                est.start_time_wiener_tracking();
            }
            let sym = (i % spf + phase) % spf;
            let shift = shifts[i];
            let Some((out_sym, out)) = est.process(row, sym, shift, &map) else { continue };
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
                self.sdc_blocks.push((idx, std::mem::take(&mut self.frame_sdc)));
                self.msc_frame_indices.push(idx);
                self.fac_cells.clear();
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
        let mut de = CellDeinterleaver::new(map.msc_cells_per_frame, 5);
        let mut dec = MlcDecoder::new(params, 1);
        let mut super_msc: Vec<Vec<EqCell>> = vec![Vec::new(); 45];
        let mut in_partial = true;
        let mut complete_frame = 0usize;
        let mut bits = Vec::new();
        let mut decoded: Vec<Vec<u8>> = Vec::new();
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
                                decoded.push(bits.clone());
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
        // Flush the last (possibly partial) super frame, as the chanest harness does.
        {
            let mut all: Vec<EqCell> = Vec::new();
            for c in super_msc.iter() {
                all.extend_from_slice(c);
            }
            for frame in all.chunks(map.msc_cells_per_frame).take(3) {
                if let Some(d) = de.push(frame) {
                    if d.iter().all(|c| c.chan > 0.0) {
                        if dec.decode(&d, &mut bits) {
                            decoded.push(bits.clone());
                        }
                    }
                }
            }
        }
        for b in decoded {
            self.deframe_audio(&b);
            self.msc_frames.push(b);
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

    /// The full live30 decode through the receiver: coarse acquisition, streaming frequency
    /// tracking, channel estimation, FAC/SDC/MSC and the audio deframing. This is the task-7
    /// acceptance: the reference decodes the same capture to FAC 64 ok / 9 bad, station
    /// `SAN90 DRM BENCH` (HE-AAC mono 12 kHz) and 200 audio frames.
    #[test]
    #[ignore = "live capture /tmp/live30.f32 required; run with --ignored --nocapture"]
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
        let mut rx = DrmReceiver::new();
        rx.push(&out);
        rx.run();
        eprintln!(
            "[rxlive] locked={} mode={:?} facs={} fac_errors={} symbols={} label={:?} msc={} aus={} rate={} timing_tracking={}",
            rx.locked(), rx.mode, rx.facs.len(), rx.fac_errors, rx.symbols_demodulated, rx.station_label, rx.msc_frames.len(), rx.audio_access_units.len(), rx.audio_rate_hz(), rx.timing_tracking
        );
        assert!(rx.locked(), "the receiver must lock on the live capture");
        assert_eq!(rx.facs.len(), 64, "FAC ok blocks {} (reference 64)", rx.facs.len());
        assert!(rx.fac_errors <= 11, "FAC errors {} (reference 9)", rx.fac_errors);
        assert_eq!(rx.station_label.as_deref(), Some("SAN90 DRM BENCH"), "station label");
        assert!(rx.audio_rate_hz() == 24_000 || rx.audio_rate_hz() == 48_000, "HE-AAC mono 12 kHz core");
        assert!(
            rx.audio_access_units.len() >= 200,
            "live audio access units {} below the reference's 200",
            rx.audio_access_units.len()
        );
    }
}
