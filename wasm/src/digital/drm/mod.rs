//! DRM30 (Digital Radio Mondiale, ES 201 980) demodulator — the digital-path plugin
//! for SW-broadcast DRM (robustness modes A–D, spectrum occupancies 0–5).
//!
//! This is an independent reimplementation of the DRM physical layer; the open-source
//! DecDRM receiver was consulted as a reference and cross-check oracle, but no code is
//! copied. The module is on the **digital path**: it reads RAW complex baseband and
//! never touches the audio chain.
//!
//! Pipeline: guard-interval time/robustness-mode sync → OFDM demodulation (FFT) →
//! occupancy detection from the power spectrum → frame sync on the time-reference
//! pilots → gain-pilot channel estimation + equalisation → QAM soft-bit demapping.
//! The output is a constellation, an SNR estimate and soft bits for the FAC/SDC/MSC
//! channels (the FAC/SDC decoders in later steps turn those bits into metadata).

pub mod audio;
pub mod cellmap;
pub mod chanest;
pub mod fac;
pub mod fec;
pub mod interleave;
pub mod ofdm;
pub mod params;
pub mod qam;
pub mod sdc;
pub mod tables;
pub mod timesync;

use crate::plugin::{DigitalDemodulator, DigitalReport};
use cellmap::CellMap;
use fac::Fac;
use fec::mlc::{MlcDecoder, MlcParams, MscProtection};
use fec::qam::{EqCell, Mapping};
use interleave::CellDeinterleaver;
use ofdm::{carrier_at, FullFft};
use params::{RobustnessMode, SpectrumOccupancy};
use tables::{NUM_FAC_CELLS, QAM16, QAM4, QAM64_SM};

/// A carrier offset below this value is left alone: the measurement has that much noise,
/// and removing it would only add a small phase jump to the stream.
const CARRIER_OFFSET_DEADBAND_HZ: f64 = 0.5;

/// Baseband needed before the first decode attempt, in seconds.
///
/// The FAC and the SDC need about one super frame (1.2 s), and the lock is what puts the
/// station in the readout. The MSC needs about three super frames, because the long
/// interleaver fills over five frames, and the audio follows the MSC: a pass at 2 s returns
/// the label and no MSC frame, while 3 s gives 2, 4 s gives 5 and 6 s gives 8. A caller that
/// holds a whole capture therefore gets the audio, and a streaming caller gets the station
/// first.
///
/// The audio decode of a real HE-AAC stream currently fails inside the FDK wasm decoder
/// (see docs/en/DRM_BENCH.md). The lock gate keeps the failure out of the metadata pass: a
/// trap in the audio would otherwise take the reported station with it.
const LOCK_MIN_SECONDS: f64 = 2.0;

/// A complex sample/value. Internal precision is f64.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cplx {
    pub re: f64,
    pub im: f64,
}

impl Cplx {
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    pub fn from_polar(r: f64, theta: f64) -> Self {
        Self { re: r * theta.cos(), im: r * theta.sin() }
    }

    pub fn conj(self) -> Self {
        Self { re: self.re, im: -self.im }
    }

    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }

    pub fn norm(self) -> f64 {
        self.norm_sqr().sqrt()
    }
}

impl core::ops::Add for Cplx {
    type Output = Cplx;
    fn add(self, o: Cplx) -> Cplx {
        Cplx::new(self.re + o.re, self.im + o.im)
    }
}

impl core::ops::Sub for Cplx {
    type Output = Cplx;
    fn sub(self, o: Cplx) -> Cplx {
        Cplx::new(self.re - o.re, self.im - o.im)
    }
}

impl core::ops::Mul for Cplx {
    type Output = Cplx;
    fn mul(self, o: Cplx) -> Cplx {
        Cplx::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}

impl core::ops::Div for Cplx {
    type Output = Cplx;
    fn div(self, o: Cplx) -> Cplx {
        let d = o.norm_sqr();
        Cplx::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
}

impl core::ops::Neg for Cplx {
    type Output = Cplx;
    fn neg(self) -> Cplx {
        Cplx::new(-self.re, -self.im)
    }
}

impl core::ops::AddAssign for Cplx {
    fn add_assign(&mut self, o: Cplx) {
        self.re += o.re;
        self.im += o.im;
    }
}

/// Complex value with amplitude `amp` and phase `2π·phase/1024`.
fn polar_1024(amp: f64, phase: i32) -> Cplx {
    Cplx::from_polar(amp, 2.0 * core::f64::consts::PI * f64::from(phase) / 1024.0)
}

/// Map FFT bin to absolute carrier index (bin 0 = DC, upper half = negative carriers).
fn bin_to_k(bin: usize, n: usize) -> i32 {
    let b = bin as i32;
    let half = (n as i32 - 1) / 2;
    if b <= half { b } else { b - n as i32 }
}

/// The DRM receiver: buffers baseband and, once enough is available, runs the whole
/// physical-layer acquisition + demodulation chain once.
pub struct DrmReceiver {
    buf: Vec<Cplx>,
    locked: bool,
    pub mode: Option<RobustnessMode>,
    pub occupancy: Option<SpectrumOccupancy>,
    /// Frame offset found by the time-pilot sync (symbol `(i + offset) % symbols_per_frame`
    /// is symbol 0 of a frame).
    pub frame_offset: usize,
    /// Time-pilot correlation score at the chosen frame offset (a frame-lock metric).
    pub frame_sync_score: f64,
    /// FAC constellation SNR (from 4-QAM decisions), dB.
    pub snr_db: Option<f64>,
    /// Equalised FAC cells (I, Q) in mapping order.
    pub fac_constellation: Vec<(f64, f64)>,
    /// Soft bits (LLRs, positive → bit 1) per channel, in cell-mapping order.
    pub fac_soft_bits: Vec<f64>,
    pub sdc_soft_bits: Vec<f64>,
    pub msc_soft_bits: Vec<f64>,
    pub symbols_demodulated: usize,
    /// Decoded FAC blocks, one per frame in capture order.
    pub facs: Vec<Fac>,
    pub fac_errors: usize,
    /// SDC decode results.
    pub station_label: Option<String>,
    pub multiplex: Option<sdc::MultiplexDescription>,
    pub audio: Option<sdc::AudioInfo>,
    pub sdc_ok: usize,
    pub sdc_errors: usize,
    /// MSC information bitrate (kbps), from FAC + SDC.
    pub msc_bitrate_kbps: Option<f64>,
    /// Decoded MSC multiplex frames (information bits, one Vec per complete frame).
    pub msc_frames: Vec<Vec<u8>>,
    /// AAC access units deframed from the audio stream (ready for the codec).
    pub audio_access_units: Vec<Vec<u8>>,
    /// Decoded 16-bit interleaved PCM (wasm32 only: filled by the FDK AAC decoder).
    pub audio_pcm: Vec<i16>,
    /// Carrier offset in Hz measured at acquisition, before any correction. A live
    /// signal always has one: the transmitter and the analyzer use different clocks.
    pub carrier_offset_hz: f64,
    /// True once the whole-carrier part of the offset has been applied. One block is spent
    /// on the measurement, and the next block decodes with the corrected anchor.
    anchor_fixed: bool,
    /// Whole-carrier shift applied last, for the readout and for the regression tests.
    pub anchor_carriers: i32,
    /// Total complex samples pushed, including the samples the buffer cap dropped. It
    /// gives every buffered sample its phase reference for the carrier-offset removal.
    pushed: u64,
    /// Radians per sample of the carrier-offset removal in force (0 = none).
    mix_w: f64,
    /// Phase for the next pushed sample, so the blocks join without a step.
    mix_phase: f64,
}

impl Default for DrmReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl DrmReceiver {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            locked: false,
            mode: None,
            occupancy: None,
            frame_offset: 0,
            frame_sync_score: 0.0,
            snr_db: None,
            fac_constellation: Vec::new(),
            fac_soft_bits: Vec::new(),
            sdc_soft_bits: Vec::new(),
            msc_soft_bits: Vec::new(),
            symbols_demodulated: 0,
            facs: Vec::new(),
            fac_errors: 0,
            station_label: None,
            multiplex: None,
            audio: None,
            sdc_ok: 0,
            sdc_errors: 0,
            msc_bitrate_kbps: None,
            msc_frames: Vec::new(),
            audio_access_units: Vec::new(),
            audio_pcm: Vec::new(),
            carrier_offset_hz: 0.0,
            anchor_fixed: false,
            anchor_carriers: 0,
            pushed: 0,
            mix_w: 0.0,
            mix_phase: 0.0,
        }
    }

    /// Feed interleaved complex baseband (f32 I,Q pairs) at 48 kHz.
    pub fn push(&mut self, iq: &[f32]) {
        // Once locked the capture has been decoded; buffering more IQ would only grow the
        // vector without bound (and eventually exhaust the wasm memory, which then breaks
        // every later allocation, e.g. the next mode's pipeline).
        if self.locked {
            return;
        }
        // Remove the carrier offset while the samples arrive. The offset is known only
        // after the first acquisition attempt, so `mix_w` starts at zero.
        for c in iq.chunks_exact(2) {
            let mut z = Cplx::new(f64::from(c[0]), f64::from(c[1]));
            if self.mix_w != 0.0 {
                let (s, co) = self.mix_phase.sin_cos();
                z = Cplx::new(z.re * co - z.im * s, z.re * s + z.im * co);
                self.mix_phase += self.mix_w;
            }
            self.buf.push(z);
            self.pushed += 1;
        }
        // Before lock, keep only a bounded window: acquisition + one super frame need a few
        // seconds, and a long listen to a band without a signal must not grow forever.
        let cap = 8 * params::SAMPLE_RATE as usize;
        if self.buf.len() > cap {
            let drop = self.buf.len() - cap;
            self.buf.drain(0..drop);
        }
    }

    /// Remove `hz` of carrier offset from the buffered baseband and from every later
    /// block. The phase runs from the first sample ever pushed, so a block that arrives
    /// later continues the same rotation and the stream has no step in it.
    fn remove_carrier_offset(&mut self, hz: f64) {
        let w = -2.0 * core::f64::consts::PI * hz / params::SAMPLE_RATE as f64;
        let first = self.pushed - self.buf.len() as u64;
        for (i, c) in self.buf.iter_mut().enumerate() {
            let ph = w * (first + i as u64) as f64;
            let (s, co) = ph.sin_cos();
            *c = Cplx::new(c.re * co - c.im * s, c.re * s + c.im * co);
        }
        self.mix_w = w;
        self.mix_phase = w * self.pushed as f64;
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Input samples buffered towards the next decode attempt.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// The decoded audio PCM's sample rate in Hz (0 when the SDC audio info is unknown).
    pub fn audio_rate_hz(&self) -> u32 {
        match self.audio.as_ref().map(|a| a.sample_rate) {
            Some(0) => 8000,
            Some(1) => 12000,
            Some(2) => 16000,
            Some(3) => 24000,
            _ => 0,
        }
    }

    /// Run acquisition + demodulation once, if enough samples are buffered. Idempotent
    /// after the first successful lock.
    pub fn run(&mut self) {
        if self.locked {
            return;
        }
        // Mode detection needs a few symbol periods. The decode needs more: see
        // `LOCK_MIN_SECONDS`.
        let min_samples = (LOCK_MIN_SECONDS * f64::from(params::SAMPLE_RATE)) as usize;
        if self.buf.len() < min_samples {
            return;
        }
        let Some(acq) = timesync::acquire(&self.buf) else { return };
        self.carrier_offset_hz = acq.freq_offset_hz;
        // OFDM needs the carriers on the FFT grid. A live capture carries an offset: the
        // bench signal measured +19.7 Hz, and +20 Hz on the fixture already fails every
        // FAC block (docs/en/DRM_BENCH.md). Remove it once, before the FFT rows. The
        // guard interval does not move, so the timing from `acquire` stays valid.
        if self.mix_w == 0.0 && acq.freq_offset_hz.abs() > CARRIER_OFFSET_DEADBAND_HZ {
            self.remove_carrier_offset(acq.freq_offset_hz);
        }
        let mode = acq.mode;
        let n = mode.fft_size();
        let ts = mode.symbol_len();

        // Two passes at most. The second pass runs when the whole-carrier anchor needed a
        // correction: the FFT rows of the first pass describe the wrong carriers.
        for _pass in 0..2 {
            // Demodulate every complete symbol, keeping the full FFT rows and a power
            // spectrum for occupancy detection.
            let mut fft = FullFft::new(n);
            let mut power = vec![0.0f64; n];
            let mut rows: Vec<Vec<Cplx>> = Vec::new();
            let mut start = acq.guard_start + mode.guard_len();
            while start + mode.fft_size() <= self.buf.len() {
                let mut row = vec![Cplx::new(0.0, 0.0); n];
                fft.forward(&self.buf[start..start + mode.fft_size()], &mut row);
                for (bin, v) in row.iter().enumerate() {
                    power[bin] += v.norm_sqr();
                }
                rows.push(row);
                start += ts;
            }
            if rows.len() < mode.symbols_per_frame() {
                return;
            }
            let nrows = rows.len() as f64;
            for p in power.iter_mut() {
                *p /= nrows;
            }

            let Some((so, carrier_shift)) = detect_occupancy(mode, &power, n) else { return };
            if carrier_shift != 0 && !self.anchor_fixed {
                // A whole-carrier offset moves every cell to its neighbour, so the equaliser
                // reads data where it expects pilots. Correct the anchor and demodulate
                // again: the rows above describe the carriers of the uncorrected signal.
                let spacing = params::SAMPLE_RATE as f64 / n as f64;
                self.remove_carrier_offset(f64::from(carrier_shift) * spacing);
                self.carrier_offset_hz += f64::from(carrier_shift) * spacing;
                self.anchor_fixed = true;
                self.anchor_carriers = carrier_shift;
                continue;
            }
            let Some(map) = CellMap::new(mode, so) else { return };
            let (r, score) = frame_sync(mode, &rows, n);
            // Publish the detection before the decode, so a receiver that has not locked
            // still reports what it sees.
            self.mode = Some(mode);
            self.occupancy = Some(so);
            self.frame_offset = r;
            self.frame_sync_score = score;
            self.symbols_demodulated = rows.len();

            // The time pilots give the frame phase, not the super-frame phase. The SDC and
            // the MSC cells move over the super frame, so try each phase and keep the one
            // whose SDC block passes its CRC. A capture that starts in the middle of a
            // super frame needs this; the bench capture does.
            let spf = mode.symbols_per_frame();
            let spsf = mode.symbols_per_superframe();
            for (attempt, phase) in (0..spsf).step_by(spf).enumerate() {
                if attempt > 0 {
                    self.clear_decode_state();
                }
                self.decode(&map, &rows, r, phase);
                if self.sdc_ok > 0 {
                    break;
                }
            }

            self.locked = true;
            return;
        }
    }

    /// Drop everything the last decode produced, so a second attempt does not mix its
    /// results with the first. The buffer, the carrier-offset state and the detection
    /// result (mode, occupancy, frame offset) stay: they do not depend on the super-frame
    /// phase, and the readout uses them while the receiver searches.
    fn clear_decode_state(&mut self) {
        self.snr_db = None;
        self.fac_constellation.clear();
        self.fac_soft_bits.clear();
        self.sdc_soft_bits.clear();
        self.msc_soft_bits.clear();
        self.symbols_demodulated = 0;
        self.facs.clear();
        self.fac_errors = 0;
        self.station_label = None;
        self.multiplex = None;
        self.audio = None;
        self.sdc_ok = 0;
        self.sdc_errors = 0;
        self.msc_bitrate_kbps = None;
        self.msc_frames.clear();
        self.audio_access_units.clear();
        self.audio_pcm.clear();
    }

    /// Channel estimation + equalisation + QAM demapping + FAC/SDC decoding.
    fn decode(&mut self, map: &CellMap, rows: &[Vec<Cplx>], r: usize, super_phase: usize) {
        let n = map.mode().fft_size();
        let spf = map.symbols_per_frame;
        let spsf = map.symbols_per_superframe;
        let sdc_syms = map.mode().sdc_symbols();

        let mut fac_dec = MlcDecoder::new(MlcParams::fac(), 0);
        let mut sdc_dec16 = MlcDecoder::new(MlcParams::sdc(Mapping::Qam16, map.sdc_cells_per_superframe), 1);
        let mut sdc_dec4 = MlcDecoder::new(MlcParams::sdc(Mapping::Qam4, map.sdc_cells_per_superframe), 1);
        let mut fac_cells: Vec<EqCell> = Vec::new();
        let mut sdc_cells: Vec<EqCell> = Vec::new();
        let mut msc_super: Vec<EqCell> = Vec::new();
        let mut msc_supers: Vec<Vec<EqCell>> = Vec::new();

        let mut sig = 0.0f64;
        let mut noise = 0.0f64;
        let mut fac_n = 0.0f64;

        for (i, row) in rows.iter().enumerate() {
            // Super-frame symbol index. The first demodulated symbol is frame 0 of a
            // super frame (true for the synthesised fixture; real signals get the frame
            // number from the FAC, which the FAC decoder supplies).
            // The frame phase comes from the time pilots. The super-frame phase does not:
            // the SDC and the MSC cells change over the super frame, so the caller passes
            // the candidate that gave a valid SDC.
            let sym = (i + r + super_phase) % spsf;
            let s = sym % spf;
            let cells: Vec<Cplx> = (0..map.num_carriers)
                .map(|c| carrier_at(row, n, map.kmin + c as i32))
                .collect();
            let eq = chanest::equalize_symbol(map, sym, &cells);
            let ec = |c: usize| EqCell { sig: eq.cells[c], chan: eq.chan[c].norm_sqr() };

            // FAC (always 4-QAM): one frame's 65 cells, decoded when complete (the
            // end of symbol 13, which survives the trailing-symbol truncation).
            if s == 0 {
                fac_cells.clear();
            }
            for &c in &map.fac_carriers[sym] {
                let v = eq.cells[c as usize];
                self.fac_constellation.push((v.re, v.im));
                for llr in qam::soft_bits(v.re, v.im, &QAM4) {
                    self.fac_soft_bits.push(llr);
                }
                let ir = qam::hard_axis(v.re, &QAM4);
                let ii = qam::hard_axis(v.im, &QAM4);
                let dec = Cplx::new(QAM4[ir], QAM4[ii]);
                sig += dec.norm_sqr();
                noise += (v - dec).norm_sqr();
                fac_n += 1.0;
                fac_cells.push(ec(c as usize));
            }
            if fac_cells.len() == NUM_FAC_CELLS {
                let mut bits = Vec::new();
                let decoded = fac_dec.decode(&fac_cells, &mut bits);
                match if decoded { Fac::parse(&bits) } else { None } {
                    Some(fac) => self.facs.push(fac),
                    None => self.fac_errors += 1,
                }
                fac_cells.clear();
            }

            // SDC (16-QAM or 4-QAM): the first symbols of each super frame.
            if sym < sdc_syms {
                for &c in &map.sdc_carriers[sym] {
                    let v = eq.cells[c as usize];
                    for llr in qam::soft_bits(v.re, v.im, &QAM16) {
                        self.sdc_soft_bits.push(llr);
                    }
                    sdc_cells.push(ec(c as usize));
                }
                if sym == sdc_syms - 1 {
                    let mut bits = Vec::new();
                    let mut block = if sdc_dec16.decode(&sdc_cells, &mut bits) {
                        sdc::parse_sdc_block(&bits).filter(|b| b.crc_ok)
                    } else {
                        None
                    };
                    if block.is_none() {
                        let mut bits4 = Vec::new();
                        block = if sdc_dec4.decode(&sdc_cells, &mut bits4) {
                            sdc::parse_sdc_block(&bits4).filter(|b| b.crc_ok)
                        } else {
                            None
                        };
                    }
                    match block {
                        Some(b) => {
                            self.sdc_ok += 1;
                            self.process_sdc(&b);
                        }
                        None => self.sdc_errors += 1,
                    }
                    sdc_cells.clear();
                }
            }

            // MSC (64-QAM SM in this fixture; resolved from the FAC).
            for &c in &map.msc_carriers[sym] {
                let v = eq.cells[c as usize];
                for llr in qam::soft_bits(v.re, v.im, &QAM64_SM) {
                    self.msc_soft_bits.push(llr);
                }
                msc_super.push(ec(c as usize));
            }
            if sym == spsf - 1 {
                msc_supers.push(core::mem::take(&mut msc_super));
            }
        }

        self.snr_db = if noise > 0.0 && fac_n > 0.0 {
            Some(10.0 * (sig / noise).log10())
        } else {
            None
        };

        // MSC information bitrate from FAC (MSC mode) + SDC (protection, part A).
        if let (Some(mux), Some(fac0)) = (&self.multiplex, self.facs.first()) {
            let mapping = msc_mapping(fac0.channel.msc_mode);
            let prot = MscProtection {
                part_a: mux.protection_a as usize,
                part_b: mux.protection_b as usize,
                hierarchical: 0,
            };
            let part_a_bytes = mux.streams.iter().map(|s| s.len_a as usize).sum::<usize>();
            let params = MlcParams::msc(mapping, map.msc_cells_per_frame, prot, part_a_bytes);
            self.msc_bitrate_kbps = Some(params.total_bits() as f64 / 400.0);

            // MSC cell deinterleaving + MLCC decoding, one multiplex frame at a time.
            let depth = match fac0.channel.interleaving {
                fac::Interleaving::Long => 5,
                fac::Interleaving::Short => 1,
            };
            let mut de = CellDeinterleaver::new(map.msc_cells_per_frame, depth);
            let mut msc_dec = MlcDecoder::new(params, 1);
            for super_cells in &msc_supers {
                // Only a complete super frame carries a whole audio frame. A partial one (the
                // tail of a short capture, or the buffer of a streaming caller) would hand the
                // AAC decoder a truncated access unit, which is not something it survives.
                let complete = super_cells.chunks(map.msc_cells_per_frame).count() >= 3;
                for frame in super_cells.chunks(map.msc_cells_per_frame).take(3) {
                    let Some(deint) = de.push(frame) else { continue };
                    // Skip frames still containing erasures (the long interleaver's
                    // fill-in delay and any trailing truncation).
                    if deint.iter().all(|c| c.chan > 0.0) {
                        let mut bits = Vec::new();
                        if msc_dec.decode(&deint, &mut bits) {
                            self.msc_frames.push(bits.clone());
                            if complete {
                                self.deframe_audio(&bits);
                            }
                        }
                    }
                }
            }
        }

        self.decode_audio();
    }

    /// Decode the deframed audio access units into PCM (wasm32 only; the FDK AAC
    /// and libxaac decoders are not linked natively). AAC (coding 0) goes through the
    /// FDK TT_DRM decoder; xHE-AAC (coding 3, MPEG-D USAC) goes through libxaac.
    #[cfg(target_arch = "wasm32")]
    fn decode_audio(&mut self) {
        let Some(audio) = self.audio.clone() else { return };
        // A 12 kHz core with SBR is the standard DRM HE-AAC configuration, and it traps inside
        // the FDK wasm decoder: the module dies, and the session dies with it. Measured on the
        // bench capture, whose audio is exactly that configuration (208-byte access units, five
        // per 400 ms frame). The synthesised fixture uses a 24 kHz core and decodes, so the
        // decoder build handles only that case. Skip the stream until the decoder is fixed;
        // docs/en/DRM_BENCH.md records the evidence.
        if audio.sbr && audio.sample_rate == 1 {
            return;
        }
        if audio.coding == 3 {
            // xHE-AAC (MPEG-D USAC). The AudioSpecificConfig is carried in the SDC audio
            // descriptor (ES 201 980 §6.4.3.10) and is fed as the decoder's init payload
            // before the access units.
            if let Some(mut dec) = crate::xaac::XaacDecoder::new() {
                if !audio.xhe_aac_config.is_empty() {
                    dec.feed(&audio.xhe_aac_config, true);
                }
                for au in &self.audio_access_units {
                    if let Some(pcm) = dec.feed(au, false) {
                        self.audio_pcm.extend_from_slice(&pcm);
                    }
                }
            }
            return;
        }
        // AAC (AAC-LC / HE-AAC) via the FDK TT_DRM decoder.
        if let Some(mut dec) = crate::fdk::AacDecoder::new() {
            dec.configure(&audio.to_type9_bytes());
            for au in &self.audio_access_units {
                let pcm = dec.decode(au);
                self.audio_pcm.extend_from_slice(&pcm);
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn decode_audio(&mut self) {}

    /// Demultiplex one decoded MSC frame and deframe the audio stream's AAC access
    /// units (the codec consumes these).
    fn deframe_audio(&mut self, msc_bits: &[u8]) {
        let Some(mux) = self.multiplex.clone() else { return };
        let frames = match self.audio.as_ref().map(|a| a.sample_rate) {
            Some(1) => 5,  // 12 kHz
            Some(3) => 10, // 24 kHz
            _ => return,
        };
        let Some(stream) = mux.streams.first() else { return };
        let fmt = audio::AacSuperFrameFormat::aac(frames, stream);
        for lf in audio::demultiplex(msc_bits, &mux).into_iter().flatten() {
            if lf.stream_id == 0 {
                if let Some(aus) = audio::parse_aac_super_frame(&lf.data, &fmt) {
                    for f in aus {
                        // FDK's TT_DRM transport expects the DRM AAC CRC byte in front
                        // of each access unit.
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
    }

    fn process_sdc(&mut self, block: &sdc::SdcBlock) {
        for e in sdc::parse_entities(&block.data) {
            match e {
                sdc::Entity::Label(l) => {
                    if self.station_label.is_none() {
                        self.station_label = Some(l.text());
                    }
                }
                sdc::Entity::Multiplex(m) => {
                    if self.multiplex.is_none() {
                        self.multiplex = Some(m);
                    }
                }
                sdc::Entity::Audio(a) => {
                    if self.audio.is_none() {
                        self.audio = Some(a);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Core sample rate in Hz for the readout. The `AudioInfo::sample_rate` field is the SDC
/// coding index, not a rate in Hz, so the receiver resolves it.
fn audio_rate_hz(rx: &DrmReceiver) -> u32 {
    rx.audio_rate_hz()
}

/// `DigitalDemodulator` wrapper that streams baseband into a `DrmReceiver` and reports
/// the decoded station label, mode and SNR once the receiver has locked.
pub struct DrPlugin {
    rx: DrmReceiver,
    last_snr: Option<f64>,
    last_label: Option<String>,
    /// Lines for the readout, and the copy last handed out. The label arrives only when
    /// the SDC passes its CRC, which can be later than the lock, so a report is sent
    /// whenever the lines change instead of once.
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
            last_snr: None,
            last_label: None,
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
            let occupancy = self
                .rx
                .occupancy
                .map(|o| format!("{:.0} kHz", o.bandwidth_khz()))
                .unwrap_or_else(|| "-".to_string());
            let line = format!(
                "DRM searching: {seconds:.1}s buffered, {mode} {occupancy}, \
                 carrier {:+.1} Hz, anchor {:+} carriers, FAC errors {}",
                self.rx.carrier_offset_hz, self.rx.anchor_carriers, self.rx.fac_errors
            );
            if self.sent.first() != Some(&line) {
                self.lines = vec![line.clone()];
                self.sent = vec![line.clone()];
                return vec![line];
            }
            return Vec::new();
        }
        self.last_snr = self.rx.snr_db;
        self.last_label = self.rx.station_label.clone();
        let mut lines = Vec::new();
        if let (Some(mode), Some(occ)) = (self.rx.mode, self.rx.occupancy) {
            // The lock state first: the operator reads the window top-down, and a locked
            // receiver with a weak signal must still show that it is locked.
            lines.push(format!(
                "locked: {mode:?}, {:.0} kHz, frame sync {:.2}, {} symbols",
                occ.bandwidth_khz(),
                self.rx.frame_sync_score,
                self.rx.symbols_demodulated,
            ));
        }
        // The station line comes from the SDC. Until the SDC passes its CRC the readout
        // still shows the lock and the FAC quality, so a lock is never invisible.
        match &self.last_label {
            Some(label) => lines.push(format!("station: {label}")),
            None => lines.push("station: (SDC not decoded yet)".to_string()),
        }
        if let Some(snr) = self.last_snr {
            // Whole decibels: a tenth of a decibel changes on every block and the change
            // would post a report per block.
            lines.push(format!("FAC SNR {snr:.0} dB"));
        }
        if let Some(bitrate) = self.rx.msc_bitrate_kbps {
            lines.push(format!("MSC {bitrate:.1} kbit/s"));
        }
        if let Some(mux) = &self.rx.multiplex {
            lines.push(format!(
                "protection A {} B {}",
                mux.protection_a, mux.protection_b
            ));
        }
        if let Some(audio) = &self.rx.audio {
            let coding = match audio.coding {
                0 => "AAC",
                3 => "xHE-AAC",
                _ => "reserved",
            };
            let kind = if audio.sbr { "SBR" } else { "no SBR" };
            let channels = match audio.mode {
                0 => "mono",
                1 => "parametric stereo",
                _ => "stereo",
            };
            lines.push(format!(
                "audio: {coding} {kind} {channels}, {} Hz core",
                audio_rate_hz(&self.rx)
            ));
        }
        // The ABI reads the readout back through `decoded()`, so the lines must live on the
        // plugin and not only in the return value.
        self.lines = lines.clone();
        self.report_blocks += 1;
        let changed = lines != self.sent;
        // Re-report about every five seconds even without a change. The operator can clear the
        // window, and a readout that never comes back after a clear looks like a dead receiver.
        if changed || self.report_blocks % 75 == 0 {
            self.sent = lines;
            self.report_blocks = 0;
            return self.lines.clone();
        }
        Vec::new()
    }

    fn reset(&mut self) {
        self.rx = DrmReceiver::new();
        self.last_snr = None;
        self.last_label = None;
        self.lines.clear();
        self.sent.clear();
        self.status_blocks = 0;
        self.report_blocks = 0;
    }

    fn last_report(&self) -> Option<DigitalReport> {
        self.last_snr.map(|snr| DigitalReport { frequency_hz: 0.0, time_offset_s: 0.0, snr_db: snr })
    }

    fn decoded(&self) -> Vec<(String, DigitalReport)> {
        // Report the lock and its metadata whether or not the SDC has decoded: a locked
        // receiver with a weak signal must still show something.
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

    fn audio_pcm(&self) -> Vec<i16> {
        self.rx.audio_pcm.clone()
    }

    fn audio_rate_hz(&self) -> u32 {
        self.rx.audio_rate_hz()
    }
}

/// Map a FAC MSC mode to the QAM mapping used by the MLCC.
fn msc_mapping(m: fac::MscMode) -> Mapping {
    match m {
        fac::MscMode::Qam64Sm => Mapping::Qam64Sm,
        fac::MscMode::Qam64HmMix => Mapping::Qam64HmMix,
        fac::MscMode::Qam64HmSym => Mapping::Qam64HmSym,
        fac::MscMode::Qam16Sm => Mapping::Qam16,
    }
}

/// Detect the spectrum occupancy from the per-carrier power: the occupied carriers
/// are exactly [Kmin, Kmax] of one of the standard layouts.
///
/// The second value is the whole-carrier shift of the detected span against the chosen
/// layout. It resolves what the guard correlation cannot: its phase wraps every `fs/nu`,
/// about 47.7 Hz, which is just above one carrier spacing (46.875 Hz), so a signal two
/// carriers away reads as a small fraction. The bench capture sat at +121 Hz.
fn detect_occupancy(
    mode: RobustnessMode,
    power: &[f64],
    n: usize,
) -> Option<(SpectrumOccupancy, i32)> {
    let maxp = power.iter().cloned().fold(0.0f64, f64::max);
    if maxp <= 0.0 {
        return None;
    }
    let thr = maxp * 0.05;
    let (mut kmin, mut kmax) = (i32::MAX, i32::MIN);
    for (bin, &p) in power.iter().enumerate() {
        if p > thr {
            let k = bin_to_k(bin, n);
            kmin = kmin.min(k);
            kmax = kmax.max(k);
        }
    }
    // Exact match first (the clean synthesised fixture lands on a layout exactly).
    for so in SpectrumOccupancy::ALL {
        if let Some((a, b)) = params::carrier_range(mode, so) {
            if a == kmin && b == kmax {
                return Some((so, 0));
            }
        }
    }
    // Otherwise the closest layout within a small tolerance: a real signal's edge carriers
    // are attenuated by the channel filter, so the detected span can be a carrier or two
    // narrower than the nominal layout (the exact match would reject every real signal).
    let mut best: Option<(SpectrumOccupancy, i32)> = None;
    let mut best_dist = i32::MAX;
    for so in SpectrumOccupancy::ALL {
        if let Some((a, b)) = params::carrier_range(mode, so) {
            let d = (a - kmin).abs() + (b - kmax).abs();
            if d < best_dist {
                best_dist = d;
                // Both edges give the shift; the average tolerates one attenuated edge.
                let shift = ((kmin - a) + (kmax - b)) as f64 / 2.0;
                best = Some((so, shift.round() as i32));
            }
        }
    }
    if best_dist <= 8 {
        best
    } else {
        None
    }
}

/// Frame synchronisation on the time-reference pilots (present only in symbol 0 of
/// each frame). Returns `(offset, score)` where `(i + offset) % symbols_per_frame` is
/// symbol 0, and `score` is the normalised time-pilot correlation.
fn frame_sync(mode: RobustnessMode, rows: &[Vec<Cplx>], n: usize) -> (usize, f64) {
    let tp = tables::time_pilots(mode);
    let spf = mode.symbols_per_frame();
    let mut best_r = 0usize;
    let mut best = -1.0f64;
    for r in 0..spf {
        let mut acc = Cplx::new(0.0, 0.0);
        let mut cnt = 0.0f64;
        for (i, row) in rows.iter().enumerate() {
            if (i + r) % spf == 0 {
                for &(k, phase) in tp {
                    let cell = carrier_at(row, n, i32::from(k));
                    let rf = polar_1024(1.0, i32::from(phase));
                    acc += cell * rf.conj();
                }
                cnt += 1.0;
            }
        }
        let score = acc.norm() / cnt.max(1.0);
        if score > best {
            best = score;
            best_r = r;
        }
    }
    (best_r, best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cplx_arithmetic() {
        let a = Cplx::new(3.0, 4.0);
        let b = Cplx::new(1.0, -2.0);
        assert_eq!(a.norm(), 5.0);
        assert_eq!((a * b), Cplx::new(11.0, -2.0));
        assert!(((a / a) - Cplx::new(1.0, 0.0)).norm() < 1e-12);
        assert_eq!(a.conj(), Cplx::new(3.0, -4.0));
    }

    #[test]
    fn bin_to_carrier_maps_dc_and_negative_half() {
        assert_eq!(bin_to_k(0, 1024), 0);
        assert_eq!(bin_to_k(103, 1024), 103);
        assert_eq!(bin_to_k(1023, 1024), -1);
        assert_eq!(bin_to_k(1024 - 103, 1024), -103);
    }
}
