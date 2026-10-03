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
        for c in iq.chunks_exact(2) {
            self.buf.push(Cplx::new(f64::from(c[0]), f64::from(c[1])));
        }
        // Before lock, keep only a bounded window: acquisition + one super frame need a few
        // seconds, and a long listen to a band without a signal must not grow forever.
        let cap = 8 * params::SAMPLE_RATE as usize;
        if self.buf.len() > cap {
            let drop = self.buf.len() - cap;
            self.buf.drain(0..drop);
        }
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
        // Two seconds is enough for mode detection (the guard correlations need a few
        // symbol periods) plus at least one frame.
        if self.buf.len() < 2 * params::SAMPLE_RATE as usize {
            return;
        }
        let Some(acq) = timesync::acquire(&self.buf) else { return };
        let mode = acq.mode;
        let n = mode.fft_size();
        let ts = mode.symbol_len();

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

        let Some(so) = detect_occupancy(mode, &power, n) else { return };
        let Some(map) = CellMap::new(mode, so) else { return };
        let (r, score) = frame_sync(mode, &rows, n);

        self.mode = Some(mode);
        self.occupancy = Some(so);
        self.frame_offset = r;
        self.frame_sync_score = score;
        self.symbols_demodulated = rows.len();
        self.decode(&map, &rows, r);
        self.locked = true;
    }

    /// Channel estimation + equalisation + QAM demapping + FAC/SDC decoding.
    fn decode(&mut self, map: &CellMap, rows: &[Vec<Cplx>], r: usize) {
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
            let sym = (i + r) % spsf;
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
                fac_dec.decode(&fac_cells, &mut bits);
                if let Some(fac) = Fac::parse(&bits) {
                    self.facs.push(fac);
                } else {
                    self.fac_errors += 1;
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
                    sdc_dec16.decode(&sdc_cells, &mut bits);
                    let mut block = sdc::parse_sdc_block(&bits).filter(|b| b.crc_ok);
                    if block.is_none() {
                        let mut bits4 = Vec::new();
                        sdc_dec4.decode(&sdc_cells, &mut bits4);
                        block = sdc::parse_sdc_block(&bits4).filter(|b| b.crc_ok);
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
                for frame in super_cells.chunks(map.msc_cells_per_frame).take(3) {
                    let Some(deint) = de.push(frame) else { continue };
                    // Skip frames still containing erasures (the long interleaver's
                    // fill-in delay and any trailing truncation).
                    if deint.iter().all(|c| c.chan > 0.0) {
                        let mut bits = Vec::new();
                        msc_dec.decode(&deint, &mut bits);
                        self.msc_frames.push(bits.clone());
                        self.deframe_audio(&bits);
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

/// `DigitalDemodulator` wrapper that streams baseband into a `DrmReceiver` and reports
/// the decoded station label, mode and SNR once the receiver has locked.
pub struct DrPlugin {
    rx: DrmReceiver,
    reported: bool,
    last_snr: Option<f64>,
    last_label: Option<String>,
}

impl DrPlugin {
    /// The receiver works at 48 kHz; the pipeline resamples the baseband to the
    /// decoder's rate before it reaches here.
    pub fn new(_rate: f64) -> Self {
        Self { rx: DrmReceiver::new(), reported: false, last_snr: None, last_label: None }
    }
}

impl DigitalDemodulator for DrPlugin {
    fn id(&self) -> &'static str {
        "drm"
    }

    fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
        self.rx.push(iq);
        self.rx.run();
        if self.rx.locked() && !self.reported {
            self.reported = true;
            self.last_snr = self.rx.snr_db;
            self.last_label = self.rx.station_label.clone();
            let mut lines = Vec::new();
            if let (Some(mode), Some(occ)) = (self.rx.mode, self.rx.occupancy) {
                lines.push(format!("DRM {mode:?} {:.0} kHz", occ.bandwidth_khz()));
            }
            if let Some(label) = &self.last_label {
                lines.push(format!("station: {label}"));
            }
            if let Some(snr) = self.last_snr {
                lines.push(format!("FAC SNR {snr:.1} dB"));
            }
            return lines;
        }
        Vec::new()
    }

    fn reset(&mut self) {
        self.rx = DrmReceiver::new();
        self.reported = false;
        self.last_snr = None;
        self.last_label = None;
    }

    fn last_report(&self) -> Option<DigitalReport> {
        self.last_snr.map(|snr| DigitalReport { frequency_hz: 0.0, time_offset_s: 0.0, snr_db: snr })
    }

    fn decoded(&self) -> Vec<(String, DigitalReport)> {
        let Some(label) = &self.last_label else { return Vec::new() };
        let report = self
            .last_report()
            .unwrap_or(DigitalReport { frequency_hz: 0.0, time_offset_s: 0.0, snr_db: 0.0 });
        vec![(format!("DRM: {label}"), report)]
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
fn detect_occupancy(mode: RobustnessMode, power: &[f64], n: usize) -> Option<SpectrumOccupancy> {
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
                return Some(so);
            }
        }
    }
    // Otherwise the closest layout within a small tolerance: a real signal's edge carriers
    // are attenuated by the channel filter, so the detected span can be a carrier or two
    // narrower than the nominal layout (the exact match would reject every real signal).
    let mut best: Option<SpectrumOccupancy> = None;
    let mut best_dist = i32::MAX;
    for so in SpectrumOccupancy::ALL {
        if let Some((a, b)) = params::carrier_range(mode, so) {
            let d = (a - kmin).abs() + (b - kmax).abs();
            if d < best_dist {
                best_dist = d;
                best = Some(so);
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
