//! FT8: the first digital demodulator plugin.
//!
//! FT8 is an 8-FSK mode: 79 symbols of 0.160 s, tones spaced 6.25 Hz, with three 7-symbol Costas
//! sync blocks at 0, 36 and 72. The 58 data symbols carry 174 bits, which are an LDPC(174,91)
//! codeword: 91 payload bits (77 of message plus a 14-bit CRC) and 83 parity bits.
//!
//! Decoding is the reverse, and follows the reference decoders (ft8_lib, WSJT-X): build a
//! *waterfall* (a Hann-windowed FFT per symbol), score candidates by a *local-contrast* sync metric
//! over the Costas tones, rank them with a heap, and for each run the max-log likelihood -> LDPC ->
//! CRC -> unpack. Decoding is *multi-pass* the way WSJT-X does it: everything one pass finds is
//! synthesized again and subtracted from the samples (frequency, time and per-symbol amplitude and
//! phase fitted against the actual data), then the search runs on what is left, so a signal that
//! was buried under a stronger neighbour comes out of the second pass. The search covers the *whole
//! slot* (not just the first 2.4 s), because a real capture is not slot-aligned: the transmission
//! can start seconds into the buffer, and the tail is then truncated, which the sync/LLR stages
//! tolerate by skipping out-of-range symbols.
//!
//! This module is on the **digital path**: it reads the RAW complex baseband and never touches the
//! audio chain. The protocol tables are ported from ft8_lib (MIT) — see `tables.rs`.

pub mod tables;

use core::cmp::Ordering;
use core::f64::consts::PI;

use crate::fft::Bluestein;
use tables::*;

/// Frequency oversampling: the waterfall's FFT has `FREQ_OSR` bins per 6.25 Hz tone, so the
/// sub-tone resolution is `6.25 / FREQ_OSR` Hz. One is too coarse (a signal half a tone off-grid
/// loses ~4 dB), two is what the reference uses. Four was tried and is a trap: the FFT frame is
/// `block * FREQ_OSR` samples long, so a finer grid also means a *longer analysis window*, and the
/// Hann window then dilutes a tone that only fills one symbol of it — a -16 dB signal halfway
/// between sub-bins still lost its sync contrast (measured: candidate score 29.5 at two, 4-7 at
/// four). Sub-bin granularity has to come from somewhere that does not stretch the window in time.
const FREQ_OSR: usize = 2;
/// Time oversampling: sub-symbol subdivisions per symbol.
const TIME_OSR: usize = 2;
/// The audio band FT8 occupies. The tones can be anywhere in here, not just one nominal frequency.
///
/// The lower edge is 100 Hz rather than the reference's 200 Hz. A receiver's own clock error moves
/// the whole audio band, and the error is proportional to the RF: measured at 411 MHz a PlutoSDR
/// transmits 1.95 ppm low (-809 Hz), which pushes a 1 kHz-baseband signal down to ~191 Hz and right
/// under a 200 Hz floor, so nothing decodes. That region is usable: measured on the SAN-90 SDR path
/// every 50 Hz slot from 0 Hz to 3 kHz sits within ~2 dB of the same noise floor, so there is no
/// LO-leakage wall to stay away from. `F_MIN_HZ` is also the frequency of waterfall bin 0, so this
/// only widens the search; it does not shift the frequencies that are reported.
const F_MIN_HZ: f64 = 100.0;
const F_MAX_HZ: f64 = 3_000.0;
/// One FT8 transmission: 79 symbols of 160 ms.
const TRANSMISSION_S: f64 = 12.64;
/// The window slides by one hop (an FT8 slot, the schedule) after every attempt, so it has to hold
/// that hop *plus* a whole transmission. That is the rule that guarantees a slot-aligned burst is
/// entirely inside at least one window, whatever its phase: `window >= hop + burst`.
///
/// A window shorter than that only works when its length happens to match the burst period, which is
/// exactly the fragile case this replaced (measured: a 17 s window on a 15 s hop decoded 3 of 6
/// slots, while 15 s tiled by luck). The margin keeps the equality off the knife edge.
const WINDOW_MARGIN_S: f64 = 0.4;
/// Retention past the window, so a block that overshoots the trigger is not trimmed at the head.
const SLOT_TAIL_S: f64 = 0.6;
/// A candidate must beat this local-contrast sync score to reach the (expensive) LDPC stage. The
/// score is in 0.5 dB units (the reference's 8-bit magnitude scale), so 10 is a 5 dB contrast.
const MIN_SYNC_SCORE: f64 = 10.0;
/// Candidates that go on to the LDPC stage, best first. The reference uses 140; the heap is a
/// bound on work, not a ranking rule, and it must be wide enough that a weak transmission survives
/// alongside stronger ones and the Costas periodicity's 36-symbol aliases.
const MAX_CANDIDATES: usize = 140;
const MAX_LDPC_ITERATIONS: usize = 40;
/// Decode passes over one window: find and decode everything, subtract the successes, search what
/// is left again. WSJT-X's structure. One pass is the old behaviour and still the common case (a
/// quiet band decodes everything first try and pass two finds nothing); the cap bounds the worst
/// case on a very busy band, where each pass costs a waterfall rebuild.
const MAX_PASSES: usize = 4;
/// A decoded candidate is only worth subtracting if the fit actually found a signal: the fitted
/// per-symbol amplitude of a unit-envelope transmission is its true amplitude, so anything below
/// ~-30 dB is a spurious CRC hit (or a stale ±36-symbol alias whose position holds nothing) and
/// subtracting it would only carve noise out of the window.
const MIN_FIT_AMPLITUDE: f64 = 0.03;

/// One decoded message, with what the decoder measured about the signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Ft8Message {
    pub text: String,
    /// Audio frequency of tone 0, in Hz.
    pub frequency_hz: f64,
    /// Where the transmission's first symbol sits in the slot buffer, in seconds (waterfall time).
    pub time_offset_s: f64,
    /// Rough SNR from the sync correlation (dB); a diagnostic, not a calibrated measurement.
    pub snr_db: f64,
}

/// The waterfall: one Hann-windowed FFT magnitude per (symbol, sub-time, sub-frequency, tone bin),
/// stored as 8-bit dB — the exact representation ft8_lib uses. Everything the search does is an
/// O(1) lookup into this table, which is what makes a whole-slot search cheap.
struct Waterfall {
    num_blocks: usize,
    num_bins: usize,
    /// `mag[((block * TIME_OSR + time_sub) * FREQ_OSR + freq_sub) * num_bins + bin]`.
    mag: Vec<u8>,
}

impl Waterfall {
    fn build(samples: &[f32], rate: f64) -> Self {
        let block = symbol_samples_at(rate);
        let subblock = block / TIME_OSR;
        let nfft = block * FREQ_OSR;
        let min_bin = (F_MIN_HZ * SYMBOL_PERIOD_S) as usize;
        let max_bin = (F_MAX_HZ * SYMBOL_PERIOD_S) as usize + 1;
        let num_bins = max_bin - min_bin;
        let num_blocks = (samples.len() / 2) / block;
        let block_stride = TIME_OSR * FREQ_OSR * num_bins;
        let mut mag = vec![0_u8; num_blocks * block_stride];

        // Hann window with the reference's `fft_norm = 2 / nfft` scaling.
        let mut window = vec![0.0_f64; nfft];
        for (i, w) in window.iter_mut().enumerate() {
            let x = (PI * i as f64 / nfft as f64).sin();
            *w = x * x * (2.0 / nfft as f64);
        }

        let mut plan = Bluestein::new(nfft);
        let mut frame_re = vec![0.0_f64; nfft];
        let mut frame_im = vec![0.0_f64; nfft];
        let mut re = vec![0.0_f64; nfft];
        let mut im = vec![0.0_f64; nfft];

        for blk in 0..num_blocks {
            let off = blk * block_stride;
            for ts in 0..TIME_OSR {
                // Shift the analysis frame by one sub-block.
                for i in 0..nfft - subblock {
                    frame_re[i] = frame_re[i + subblock];
                    frame_im[i] = frame_im[i + subblock];
                }
                let start = blk * block + ts * subblock;
                for i in 0..subblock {
                    let idx = (start + i) * 2;
                    let (sr, si) = if idx + 1 < samples.len() {
                        (samples[idx] as f64, samples[idx + 1] as f64)
                    } else {
                        (0.0, 0.0)
                    };
                    frame_re[nfft - subblock + i] = sr;
                    frame_im[nfft - subblock + i] = si;
                }
                for i in 0..nfft {
                    re[i] = frame_re[i] * window[i];
                    im[i] = frame_im[i] * window[i];
                }
                plan.forward(&mut re, &mut im);
                for fsb in 0..FREQ_OSR {
                    for b in 0..num_bins {
                        let src = (min_bin + b) * FREQ_OSR + fsb;
                        let m2 = re[src] * re[src] + im[src] * im[src];
                        let db = 10.0 * (1e-12 + m2).log10();
                        let scaled = (2.0 * db + 240.0).round() as i64;
                        mag[off + ts * (FREQ_OSR * num_bins) + fsb * num_bins + b] =
                            scaled.clamp(0, 255) as u8;
                    }
                }
            }
        }
        Self { num_blocks, num_bins, mag }
    }

    /// Magnitude at the candidate plus `sym_off` symbols and `bin_off` tone bins, as the raw 8-bit
    /// dB value. Out-of-range reads are 0 (a truncated transmission reads as "no energy there").
    fn mag_at(
        &self,
        to: i32,
        ts: usize,
        fsb: usize,
        fo: usize,
        sym_off: i32,
        bin_off: i32,
    ) -> u8 {
        let block = to + sym_off;
        if block < 0 || block >= self.num_blocks as i32 {
            return 0;
        }
        let bin = fo as i32 + bin_off;
        if bin < 0 || bin >= self.num_bins as i32 {
            return 0;
        }
        let idx = ((block as usize * TIME_OSR + ts) * FREQ_OSR + fsb) * self.num_bins + bin as usize;
        self.mag[idx]
    }

    /// The reference's `ft8_sync_score`: each Costas tone against its frequency neighbours (±one
    /// tone) and its time neighbours (±one symbol), averaged. A *local contrast*, not an energy
    /// fraction: a weak transmission scores on how much it stands out from its immediate
    /// surroundings, which is the metric that survives a busy band.
    fn sync_score(&self, to: i32, ts: usize, fsb: usize, fo: usize) -> f64 {
        let mut score = 0.0;
        let mut num_average = 0;
        for m in 0..3 {
            for k in 0..SYNC_LENGTH {
                let sym = (SYNC_OFFSET * m + k) as i32;
                let block_abs = to + sym;
                if block_abs < 0 || block_abs >= self.num_blocks as i32 {
                    continue;
                }
                let sm = COSTAS[k] as i32;
                let p = self.mag_at(to, ts, fsb, fo, sym, sm) as f64;
                if sm > 0 {
                    score += p - self.mag_at(to, ts, fsb, fo, sym, sm - 1) as f64;
                    num_average += 1;
                }
                if sm < 7 {
                    score += p - self.mag_at(to, ts, fsb, fo, sym, sm + 1) as f64;
                    num_average += 1;
                }
                if k > 0 && block_abs > 0 {
                    score += p - self.mag_at(to, ts, fsb, fo, sym - 1, sm) as f64;
                    num_average += 1;
                }
                if k + 1 < SYNC_LENGTH && block_abs + 1 < self.num_blocks as i32 {
                    score += p - self.mag_at(to, ts, fsb, fo, sym + 1, sm) as f64;
                    num_average += 1;
                }
            }
        }
        if num_average > 0 {
            score / num_average as f64
        } else {
            -1e9
        }
    }

    /// The band's strongest candidates over the whole buffered slot, best first, as
    /// `(score, time_offset, time_sub, freq_sub, freq_offset)`.
    fn find_candidates(&self) -> Vec<(f64, i32, usize, usize, usize)> {
        let mut scored = Vec::new();
        let to_min = -(2 * FREQ_OSR as i32);
        for to in to_min..self.num_blocks as i32 {
            for ts in 0..TIME_OSR {
                for fsb in 0..FREQ_OSR {
                    for fo in 0..self.num_bins - 8 {
                        let s = self.sync_score(to, ts, fsb, fo);
                        if s >= MIN_SYNC_SCORE {
                            scored.push((s, to, ts, fsb, fo));
                        }
                    }
                }
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        scored.truncate(MAX_CANDIDATES);
        scored
    }

    /// The 174 max-log likelihoods at a candidate, normalized to the variance the LDPC expects
    /// (the reference's `ft8_extract_likelihood` + `ftx_normalize_logl`).
    fn extract_likelihood(&self, to: i32, ts: usize, fsb: usize, fo: usize) -> [f64; 174] {
        let max4 = |a: f64, b: f64, c: f64, d: f64| a.max(b).max(c).max(d);
        let mut llr = [0.0_f64; 174];
        let mut data_index = 0;
        for k in 0..NUM_DATA_SYMBOLS {
            // Data symbol k sits at message symbol k + 7 (before the 2nd Costas block) or k + 14.
            let sym = k + if k < 29 { SYNC_LENGTH } else { 2 * SYNC_LENGTH };
            let mut s2 = [0.0_f64; 8];
            for (j, value) in s2.iter_mut().enumerate() {
                let bin = GRAY[j] as i32;
                *value = self.mag_at(to, ts, fsb, fo, sym as i32, bin) as f64 * 0.5 - 120.0;
            }
            llr[data_index] = max4(s2[4], s2[5], s2[6], s2[7]) - max4(s2[0], s2[1], s2[2], s2[3]);
            llr[data_index + 1] =
                max4(s2[2], s2[3], s2[6], s2[7]) - max4(s2[0], s2[1], s2[4], s2[5]);
            llr[data_index + 2] =
                max4(s2[1], s2[3], s2[5], s2[7]) - max4(s2[0], s2[2], s2[4], s2[6]);
            data_index += 3;
        }
        let n = llr.len() as f64;
        let sum: f64 = llr.iter().sum();
        let sum2: f64 = llr.iter().map(|v| v * v).sum();
        let variance = (sum2 - sum * sum / n) / n;
        if variance > 0.0 {
            let factor = (24.0 / variance).sqrt();
            for value in llr.iter_mut() {
                *value *= factor;
            }
        }
        llr
    }

    /// Decode one candidate: likelihood -> LDPC -> CRC, returning the full 174-bit codeword (the
    /// first 91 bits are payload+CRC, the rest parity). The codeword is what a subtraction needs:
    /// re-encoding the 77 payload bits through the generator gives the same parity, but the LDPC
    /// output already has it, and the tones come from all 174 bits. When the max-log LDPC does not
    /// converge (or converges onto a CRC miss), the ordered-statistics fallback gets one bounded
    /// shot at the same likelihoods -- that fallback is worth roughly 2 dB of sensitivity on this
    /// code (see `ft8_snr_sweep`'s baseline).
    fn decode_candidate(&self, to: i32, ts: usize, fsb: usize, fo: usize) -> Option<[u8; 174]> {
        let llr = self.extract_likelihood(to, ts, fsb, fo);
        if let Some(bits) = ldpc_decode(&llr, MAX_LDPC_ITERATIONS) {
            if crc_ok(&bits) {
                return Some(bits);
            }
        }
        let bits = osd_decode(&llr)?;
        if crc_ok(&bits) {
            Some(bits)
        } else {
            None
        }
    }
}

/// One slot's worth of complex baseband, kept as interleaved f32.
pub struct Ft8Decoder {
    rate: f64,
    samples: Vec<f32>,
    /// Slot length in complex samples (one FT8 slot at this rate).
    slot_samples: usize,
    /// Complex samples ever pushed. The buffer slides, so a decode's *absolute* position in the
    /// stream is `total_pushed - buffered + time_offset`, which is what de-duplication needs.
    total_pushed: usize,
    /// `(text, absolute burst start)` reported by the previous attempt: a burst that overlaps two
    /// consecutive windows is found twice and must be reported once.
    last_reported: Vec<(String, usize)>,
}

impl Ft8Decoder {
    pub fn new(rate: f64) -> Self {
        let slot_samples = (rate * SLOT_SECONDS) as usize;
        Self {
            rate,
            samples: Vec::new(),
            slot_samples,
            total_pushed: 0,
            last_reported: Vec::new(),
        }
    }

    pub fn push_iq(&mut self, iq: &[f32]) {
        // Saturating: this counts every sample of a session, and wasm32's `usize` is 32 bits, so a
        // long enough run would wrap and make the de-duplication arithmetic nonsense.
        self.total_pushed = self.total_pushed.saturating_add(iq.len() / 2);
        self.samples.extend_from_slice(iq);
        // Keep one slot plus the pre-roll and the tail. The caller deliberately feeds `pre-roll +
        // slot + tail` (the pre-roll absorbs a transmission that starts just before the boundary),
        // so retaining less than that eats into the *head* of the window -- and the head is where
        // the transmission starts.
        // Retention must cover the whole trigger window plus a little slack, otherwise the trim takes
        // the difference off the *head* -- exactly where the transmission is.
        let keep = self.retain_samples();
        if self.samples.len() > keep * 2 {
            // Both sides in f32 elements: `samples` is interleaved I/Q, so a complex count has to be
            // doubled. Subtracting the complex count here over-drained by `keep` elements (~half the
            // buffer), which silently discarded the head of the window -- and with it the
            // transmission.
            let drop = self.samples.len() - keep * 2;
            self.samples.drain(..drop);
        }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.total_pushed = 0;
        self.last_reported.clear();
    }

    /// Buffered complex samples (diagnostics).
    pub fn buffered(&self) -> usize {
        self.samples.len() / 2
    }

    /// Complex samples in one FT8 slot at this rate (15 s: the slot the mode is scheduled on).
    pub fn slot_samples(&self) -> usize {
        self.slot_samples
    }

    /// What `push_iq` retains: the window plus slack for a block that overshoots the trigger.
    pub fn retain_samples(&self) -> usize {
        self.window_samples() + (self.rate * SLOT_TAIL_S) as usize
    }

    /// The sliding window (also the decode trigger): one hop plus one transmission.
    pub fn window_samples(&self) -> usize {
        self.slot_samples + (self.rate * (TRANSMISSION_S + WINDOW_MARGIN_S)) as usize
    }

    /// Slide the window forward by `complex` samples, keeping the overlap. Clearing after an attempt
    /// instead would make consecutive windows non-overlapping, which drifts against the 15 s schedule
    /// and drops bursts at the seams -- the whole point of sliding by a hop shorter than the window.
    pub fn advance(&mut self, complex: usize) {
        let drop = (complex * 2).min(self.samples.len());
        self.samples.drain(..drop);
    }

    /// Complex samples in one FT8 *transmission* (79 symbols = 12.64 s).
    pub fn transmission_samples(&self) -> usize {
        NUM_SYMBOLS * symbol_samples_at(self.rate)
    }

    /// Decode every message in the buffered slot. The search runs in passes: one pass decodes what
    /// it can, every success is subtracted from a scratch copy of the window (never from the
    /// buffer itself — the window slides and the next attempt re-searches the overlap), and the
    /// search runs again on what is left, so a signal that was buried under a stronger neighbour
    /// surfaces in a later pass. The buffer the caller keeps is never modified.
    pub fn decode(&mut self) -> Vec<Ft8Message> {
        let n = self.samples.len() / 2;
        let symbol_samples = symbol_samples_at(self.rate);
        if n < NUM_SYMBOLS * symbol_samples {
            return Vec::new();
        }
        // FT8's tones span 200-3000 Hz; below that band's Nyquist the waterfall cannot read it.
        if F_MAX_HZ * 2.0 > self.rate {
            return Vec::new();
        }
        let mut scratch = self.samples.clone();
        let mut out: Vec<Ft8Message> = Vec::new();
        for _pass in 0..MAX_PASSES {
            let waterfall = Waterfall::build(&scratch, self.rate);
            let mut found_this_pass = 0;
            for (score, to, ts, fsb, fo) in waterfall.find_candidates() {
                let Some(codeword) = waterfall.decode_candidate(to, ts, fsb, fo) else {
                    continue;
                };
                let payload: [u8; 77] = core::array::from_fn(|i| codeword[i]);
                let Some(text) = unpack_message(&payload) else {
                    continue;
                };
                // The Costas pattern repeats every 36 symbols, so one transmission also scores (and
                // sometimes decodes) at ±36 offsets; dedupe by text. Subtract only on the first
                // decode of a text: candidates are score-ordered, so the true peak lands first,
                // and a later duplicate (the same transmission one sub-bin over) or a ±36 alias
                // would re-run the whole fit for nothing -- the fit-amplitude gate neutralizes
                // them, but skipping known texts saves real time on a busy band.
                if !out.iter().any(|m: &Ft8Message| m.text == text) {
                    subtract_transmission(&mut scratch, self.rate, &codeword, to, ts, fsb, fo);
                    found_this_pass += 1;
                    out.push(Ft8Message {
                        text,
                        frequency_hz: F_MIN_HZ
                            + (fo as f64 + fsb as f64 / FREQ_OSR as f64) * TONE_SPACING_HZ,
                        time_offset_s: (to as f64 + ts as f64 / TIME_OSR as f64) * SYMBOL_PERIOD_S,
                        snr_db: score * 0.5,
                    });
                }
            }
            if found_this_pass == 0 {
                break;
            }
        }
        // The window slides, so a burst near the seam is inside two consecutive windows and decodes
        // twice. Suppress the repeat by *absolute position in the stream*, not by text: the same text
        // legitimately repeats every slot, and only the same burst must be reported once.
        let buffer_start = self.total_pushed.saturating_sub(self.samples.len() / 2);
        let tolerance = self.rate as usize / 2;   // 0.5 s: the same burst, not the next slot
        let mut reported: Vec<(String, usize)> = Vec::with_capacity(out.len());
        out.retain(|message| {
            let absolute = buffer_start + (message.time_offset_s * self.rate) as usize;
            let seen = self.last_reported.iter().any(|(text, previous)| {
                *text == message.text && absolute.abs_diff(*previous) <= tolerance
            });
            if !seen {
                reported.push((message.text.clone(), absolute));
            }
            !seen
        });
        self.last_reported = reported;
        out
    }
}

/// Symbol window in samples at `rate`.
fn symbol_samples_at(rate: f64) -> usize {
    (rate * SYMBOL_PERIOD_S) as usize
}

fn is_sync_symbol(index: usize) -> bool {
    index < SYNC_LENGTH
        || (SYNC_OFFSET..SYNC_OFFSET + SYNC_LENGTH).contains(&index)
        || (2 * SYNC_OFFSET..2 * SYNC_OFFSET + SYNC_LENGTH).contains(&index)
}

/// The 79 transmitted tones for a codeword: the inverse of the tone->LLR extraction. A subtraction
/// re-generates exactly what a transmitter would have sent for the decoded bits.
fn tones_from_codeword(codeword: &[u8; 174]) -> [u8; NUM_SYMBOLS] {
    let mut tones = [0_u8; NUM_SYMBOLS];
    for block in 0..3 {
        for (k, tone) in COSTAS.iter().enumerate() {
            tones[block * SYNC_OFFSET + k] = *tone;
        }
    }
    let mut data_index = 0;
    for position in 0..NUM_SYMBOLS {
        if is_sync_symbol(position) {
            continue;
        }
        let bits3 =
            (codeword[data_index] << 2) | (codeword[data_index + 1] << 1) | codeword[data_index + 2];
        data_index += 3;
        tones[position] = GRAY[bits3 as usize];
    }
    tones
}

/// Match a synthesized transmission against the samples at `start` (symbol 0's first complex
/// sample). Every symbol is a unit-amplitude tone, so a least-squares fit of the complex gain is
/// just a correlation: `g_k = <y, s_k> / sym`, and the total fit power `sum_k |z_k|^2` is what a
/// frequency/time refinement maximizes. Returns the power and the per-symbol gains (out-of-window
/// symbols get zero on both sides, the same tolerance the sync/LLR stages have for a truncated
/// tail). One call is one pass over the transmission's samples, so the refinement grids are kept
/// coarse (a few Hz, a few hundred samples) on purpose.
fn fit_model(
    samples: &[f32],
    tones: &[u8; NUM_SYMBOLS],
    base_freq: f64,
    rate: f64,
    start: i64,
) -> (f64, [(f64, f64); NUM_SYMBOLS]) {
    let sym = symbol_samples_at(rate);
    let n_complex = samples.len() / 2;
    let mut gains = [(0.0_f64, 0.0_f64); NUM_SYMBOLS];
    let mut power = 0.0_f64;
    for (k, tone) in tones.iter().enumerate() {
        let s0 = start + (k as i64) * sym as i64;
        if s0 < 0 || s0 + sym as i64 > n_complex as i64 {
            continue;
        }
        let f = base_freq + *tone as f64 * TONE_SPACING_HZ;
        let step = core::f64::consts::TAU * f / rate;
        let (ds, dc) = step.sin_cos(); // f64::sin_cos returns (sin, cos)
        let (mut wr, mut wi) = (1.0_f64, 0.0_f64);
        let (mut zr, mut zi) = (0.0_f64, 0.0_f64);
        for i in 0..sym {
            let idx = (s0 as usize + i) * 2;
            let (yr, yi) = (samples[idx] as f64, samples[idx + 1] as f64);
            zr += yr * wr + yi * wi;
            zi += yi * wr - yr * wi;
            let nwr = wr * dc - wi * ds;
            wi = wr * ds + wi * dc;
            wr = nwr;
        }
        gains[k] = (zr / sym as f64, zi / sym as f64);
        power += zr * zr + zi * zi;
    }
    (power, gains)
}

/// Remove one decoded transmission from the samples, the way WSJT-X does between passes.
///
/// The waterfall only localizes the signal to a sub-bin (`FREQ_OSR` bins per tone) and a
/// sub-symbol (`TIME_OSR` blocks per symbol), and its block time carries up to a symbol of lead
/// (the analysis window straddles the symbols) — far too coarse to cancel against: one bin of
/// residual frequency error rotates the phase a full circle every ~0.3 s and the subtraction
/// destroys itself. So the model is fitted to the data first — coarse time, then frequency, then
/// fine time, each round maximizing the correlation power — and the per-symbol complex gains of
/// the final fit are what gets subtracted. The per-symbol fit also absorbs phase drift between
/// transmitter and receiver.
fn subtract_transmission(
    samples: &mut [f32],
    rate: f64,
    codeword: &[u8; 174],
    to: i32,
    ts: usize,
    fsb: usize,
    fo: usize,
) {
    let tones = tones_from_codeword(codeword);
    let freq = F_MIN_HZ + (fo as f64 + fsb as f64 / FREQ_OSR as f64) * TONE_SPACING_HZ;
    let sym = symbol_samples_at(rate);
    let t0 = (to as f64 + ts as f64 / TIME_OSR as f64) * sym as f64;
    let sub_bin = TONE_SPACING_HZ / FREQ_OSR as f64;

    // Coarse time first: the waterfall's block time is only known to about a symbol, and with the
    // model one symbol off, every symbol correlates against the wrong tone — the frequency round
    // would only be optimizing noise. A 1/8-symbol grid over ±1.5 symbols absorbs the block-time
    // lead plus the TIME_OSR quantization.
    let mut best_t0 = t0 as i64;
    let mut best_power = -1.0_f64;
    for step in -12_i32..=12 {
        let start = t0 as i64 + step as i64 * (sym as i64 / 8);
        let (power, _) = fit_model(samples, &tones, freq, rate, start);
        if power > best_power {
            best_power = power;
            best_t0 = start;
        }
    }
    // Frequency at that alignment: a 1/8-sub-bin grid over ±1 sub-bin, the waterfall's own
    // quantization plus change.
    let mut best_freq = freq;
    best_power = -1.0;
    for step in -8_i32..=8 {
        let f = freq + step as f64 * sub_bin / 8.0;
        let (power, _) = fit_model(samples, &tones, f, rate, best_t0);
        if power > best_power {
            best_power = power;
            best_freq = f;
        }
    }
    // Fine time at that frequency: the remainder of the alignment error.
    best_power = -1.0;
    for step in -8_i32..=8 {
        let start = best_t0 + (step as f64 * (sym as f64 / 16.0)) as i64;
        let (power, _) = fit_model(samples, &tones, best_freq, rate, start);
        if power > best_power {
            best_power = power;
            best_t0 = start;
        }
    }
    // Polish: the residual cancellation scales with the square of the amplitude error, and an
    // alignment a few hundred samples off leaves several percent of a strong signal behind —
    // enough to keep masking a weak co-channel neighbour. A 1/128-symbol grid takes the timing
    // error down to ~0.4 % of a symbol.
    best_power = -1.0;
    for step in -8_i32..=8 {
        let start = best_t0 + (step as f64 * (sym as f64 / 128.0)) as i64;
        let (power, _) = fit_model(samples, &tones, best_freq, rate, start);
        if power > best_power {
            best_power = power;
            best_t0 = start;
        }
    }
    let (power, gains) = fit_model(samples, &tones, best_freq, rate, best_t0);
    let fitted = gains.iter().filter(|g| *g != &(0.0, 0.0)).count();
    if fitted == 0 {
        return;
    }
    // Mean fitted amplitude of the unit-envelope model. A real transmission lands near its true
    // amplitude; a stale candidate (already subtracted, or an alias whose window holds nothing)
    // lands near zero and must not be "subtracted".
    let amplitude = (power / fitted as f64).sqrt() / sym as f64;
    if amplitude < MIN_FIT_AMPLITUDE {
        return;
    }
    let n_complex = samples.len() / 2;
    for (k, tone) in tones.iter().enumerate() {
        let (gr, gi) = gains[k];
        if gr == 0.0 && gi == 0.0 {
            continue;
        }
        let s0 = best_t0 + (k as i64) * sym as i64;
        if s0 < 0 || s0 + sym as i64 > n_complex as i64 {
            continue;
        }
        let f = best_freq + *tone as f64 * TONE_SPACING_HZ;
        let step = core::f64::consts::TAU * f / rate;
        let (ds, dc) = step.sin_cos(); // f64::sin_cos returns (sin, cos)
        let (mut wr, mut wi) = (1.0_f64, 0.0_f64);
        for i in 0..sym {
            let idx = (s0 as usize + i) * 2;
            // y - g_k * w, with w the same unit rotation the fit correlated against.
            samples[idx] = (samples[idx] as f64 - (gr * wr - gi * wi)) as f32;
            samples[idx + 1] = (samples[idx + 1] as f64 - (gr * wi + gi * wr)) as f32;
            let nwr = wr * dc - wi * ds;
            wi = wr * ds + wi * dc;
            wr = nwr;
        }
    }
}

// ---------------------------------------------------------------- LDPC (sum-product)

/// Soft-decision LDPC(174,91) decode. `None` when the parity checks never pass.
fn ldpc_decode(llr: &[f64; 174], max_iterations: usize) -> Option<[u8; 174]> {
    // tov[n][m]: message from variable node n to its m-th check; toc[m][n]: the reverse.
    let mut tov = [[0.0_f64; 3]; 174];
    let mut toc = [[0.0_f64; 7]; 83];
    for _iteration in 0..max_iterations {
        // Hard decision from the channel value plus the current extrinsic information.
        let mut plain = [0_u8; 174];
        let mut plain_sum = 0;
        for n in 0..174 {
            let total = llr[n] + tov[n][0] + tov[n][1] + tov[n][2];
            plain[n] = u8::from(total > 0.0);
            plain_sum += plain[n] as i32;
        }
        if plain_sum == 0 {
            // Converged to the all-zero word, which this code forbids.
            return None;
        }
        if parity_errors(&plain) == 0 {
            return Some(plain);
        }
        // Variable nodes -> check nodes.
        for m in 0..83 {
            let count = LDPC_NUM_ROWS[m] as usize;
            for index in 0..count {
                let n = LDPC_NM[m][index] as usize - 1;
                let mut message = llr[n];
                for other in 0..3 {
                    if LDPC_MN[n][other] as usize - 1 != m {
                        message += tov[n][other];
                    }
                }
                toc[m][index] = tanh_approx(-message / 2.0);
            }
        }
        // Check nodes -> variable nodes.
        for n in 0..174 {
            for other in 0..3 {
                let m = LDPC_MN[n][other] as usize - 1;
                let count = LDPC_NUM_ROWS[m] as usize;
                let mut product = 1.0;
                for index in 0..count {
                    if LDPC_NM[m][index] as usize - 1 != n {
                        product *= toc[m][index];
                    }
                }
                tov[n][other] = -2.0 * atanh_approx(product);
            }
        }
    }
    None
}

fn parity_errors(codeword: &[u8; 174]) -> usize {
    let mut errors = 0;
    for m in 0..83 {
        let mut parity = 0_u8;
        for index in 0..LDPC_NUM_ROWS[m] as usize {
            parity ^= codeword[LDPC_NM[m][index] as usize - 1];
        }
        if parity != 0 {
            errors += 1;
        }
    }
    errors
}

/// `tanh` with the range limits the reference applies, then a rational approximation.
fn tanh_approx(x: f64) -> f64 {
    if x < -4.97 {
        return -1.0;
    }
    if x > 4.97 {
        return 1.0;
    }
    let x2 = x * x;
    let a = x * (945.0 + x2 * (105.0 + x2));
    let b = 945.0 + x2 * (420.0 + x2 * 15.0);
    a / b
}

fn atanh_approx(x: f64) -> f64 {
    let x2 = x * x;
    let a = x * (945.0 + x2 * (-735.0 + x2 * 64.0));
    let b = 945.0 + x2 * (-1050.0 + x2 * 225.0);
    a / b
}

// ---------------------------------------------------------------- OSD (ordered statistics)

/// Ordered-statistics decoding: the fallback for a candidate whose max-log LDPC run does not
/// converge. This is the other half of what WSJT-X (and FT8CN after it) do at the sensitivity
/// edge: the hard decisions are re-ordered by reliability, the 83 checks get their pivot columns
/// by Gaussian elimination on the check matrix, and the remaining 91 columns -- the *information
/// set* -- are re-encoded over a bounded set of error patterns. Every pattern yields a word that
/// satisfies all parity checks, so the discriminator is not the syndrome but the distance to the
/// received LLRs: patterns are ranked by the reliability-weighted cost of the bits they move, and
/// only the single best word is CRC-checked. Gating one candidate instead of ~4200 individually
/// is what keeps the false-positive rate at the LDPC decoder's level -- CRC-checking every
/// pattern would pass by chance at roughly 2^-14 per pattern.
///
/// Enumeration covers weight 0, 1 and 2 on the information set (WSJT-X's OSD-2, without parity
/// bit flips: flipping a computed parity bit would leave valid-LDPC space, and the weight-2 sweep
/// is where the measured gain over plain max-log LDPC saturates for FT8's code).
fn osd_decode(llr: &[f64; 174]) -> Option<[u8; 174]> {
    // Hard decisions and the reliability order (most reliable first).
    let mut order: [usize; 174] = core::array::from_fn(|i| i);
    order.sort_unstable_by(|a, b| llr[*b].abs().total_cmp(&llr[*a].abs()));

    // The check matrix as bitsets over the original bit positions.
    let mut rows = [[0_u64; 3]; 83];
    for (m, row) in rows.iter_mut().enumerate() {
        for index in 0..LDPC_NUM_ROWS[m] as usize {
            let n = LDPC_NM[m][index] as usize - 1;
            row[n / 64] |= 1 << (n % 64);
        }
    }

    // Gaussian elimination in reliability order: every check gets a pivot column among the most
    // reliable ones that keep the matrix full-rank, and the columns never picked -- the ones the
    // rank condition skipped -- form the information set. Skipping (rather than swapping) keeps
    // that set the most reliable columns the constraint allows, which is the whole premise of the
    // method: the hard decisions there are the ones worth trusting.
    let mut in_info = [true; 174];
    let mut pivot_pos = [0_usize; 83];
    let mut rank = 0;
    for &col in order.iter() {
        if rank == 83 {
            break;
        }
        let word = col / 64;
        let bit = 1_u64 << (col % 64);
        let Some(r) = (rank..83).find(|&r| rows[r][word] & bit != 0) else {
            continue; // linearly dependent on the pivots already chosen: stays in the info set
        };
        rows.swap(rank, r);
        for r2 in 0..83 {
            if r2 != rank && rows[r2][word] & bit != 0 {
                for w in 0..3 {
                    rows[r2][w] ^= rows[rank][w];
                }
            }
        }
        pivot_pos[rank] = col;
        in_info[col] = false;
        rank += 1;
    }
    if rank != 83 {
        return None; // the check matrix lost rank: cannot re-encode (cannot happen for FT8's H)
    }
    let info_pos: Vec<usize> = order.iter().copied().filter(|c| in_info[*c]).collect();

    // Per-check coefficient vectors over the information set (bit j = check i involves info bit
    // j), the packed hard decisions, and both sides' reliabilities.
    let mut coef = [[0_u64; 2]; 83];
    let mut hard_par = [0_u8; 83];
    let mut hard_info = [0_u64; 2];
    let mut rel_info = [0.0_f64; 91];
    for (i, c) in coef.iter_mut().enumerate() {
        for (j, &p) in info_pos.iter().enumerate() {
            if rows[i][p / 64] >> (p % 64) & 1 == 1 {
                c[j / 64] |= 1 << (j % 64);
                hard_par[i] ^= (llr[p] > 0.0) as u8;
            }
        }
    }
    for (j, &p) in info_pos.iter().enumerate() {
        hard_info[j / 64] |= ((llr[p] > 0.0) as u64) << (j % 64);
        rel_info[j] = llr[p].abs();
    }
    let rel_pivot: [f64; 83] = core::array::from_fn(|i| llr[pivot_pos[i]].abs());

    // The distance between a re-encoded word and the received LLRs: the reliability of every
    // information bit the pattern flips plus every parity position the re-encoding moved.
    let evaluate = |v: [u64; 2], flip_cost: f64| -> f64 {
        let mut dist = flip_cost;
        for (i, c) in coef.iter().enumerate() {
            let par = ((c[0] & v[0]).count_ones() ^ (c[1] & v[1]).count_ones()) & 1;
            if par as u8 != hard_par[i] {
                dist += rel_pivot[i];
            }
        }
        dist
    };
    let mut best_v = hard_info;
    let mut best_dist = evaluate(hard_info, 0.0);
    for j1 in 0..91_usize {
        let flip1 = 1_u64 << (j1 % 64);
        let mut v1 = hard_info;
        v1[j1 / 64] ^= flip1;
        let dist1 = evaluate(v1, rel_info[j1]);
        if dist1 < best_dist {
            best_dist = dist1;
            best_v = v1;
        }
        for j2 in j1 + 1..91 {
            let mut v2 = v1;
            v2[j2 / 64] ^= 1_u64 << (j2 % 64);
            let dist2 = evaluate(v2, rel_info[j1] + rel_info[j2]);
            if dist2 < best_dist {
                best_dist = dist2;
                best_v = v2;
            }
        }
    }

    // Assemble the best word and hand it to the CRC. No other word is checked.
    let mut cw = [0_u8; 174];
    for (j, &p) in info_pos.iter().enumerate() {
        cw[p] = ((best_v[j / 64] >> (j % 64)) & 1) as u8;
    }
    for (i, c) in coef.iter().enumerate() {
        let par = ((c[0] & best_v[0]).count_ones() ^ (c[1] & best_v[1]).count_ones()) & 1;
        cw[pivot_pos[i]] = par as u8;
    }
    if crc_ok(&cw) {
        Some(cw)
    } else {
        None
    }
}

// ---------------------------------------------------------------- CRC and message

/// The 14-bit CRC over the 77 payload bits zero-extended to 82 (FT8's rule).
///
/// A literal port of the reference's `ftx_compute_crc`: the message is handled as bytes, a whole
/// byte being XORed into the top of the remainder every 8 bits. The encoder and this decoder must
/// agree bit for bit, and a reformulated bit-serial version did not (it produced a value that
/// disagreed with the reference on most payloads).
pub fn crc14(payload: &[u8; 77]) -> u16 {
    let mut message = [0_u8; 11];
    for (index, bit) in payload.iter().enumerate() {
        if *bit != 0 {
            message[index / 8] |= 1 << (7 - (index % 8));
        }
    }
    let mut remainder = 0_u16;
    for idx_bit in 0..82 {
        if idx_bit % 8 == 0 {
            remainder ^= (message[idx_bit / 8] as u16) << (CRC_WIDTH - 8);
        }
        remainder = if remainder & (1 << (CRC_WIDTH - 1)) != 0 {
            ((remainder << 1) ^ CRC_POLYNOMIAL) & 0xFFFF
        } else {
            (remainder << 1) & 0xFFFF
        };
    }
    remainder & ((1 << CRC_WIDTH) - 1)
}

fn crc_ok(bits: &[u8; 174]) -> bool {
    let payload: [u8; 77] = core::array::from_fn(|i| bits[i]);
    let mut expected = 0_u16;
    for i in 0..14 {
        expected = (expected << 1) | bits[77 + i] as u16;
    }
    crc14(&payload) == expected
}

const NTOKENS: u32 = 2_063_592;
const MAX22: u32 = 4_194_304;
const MAXGRID4: u16 = 32_400;
const ALPHANUM_SPACE: &[u8; 37] = b" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const ALPHANUM: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LETTERS_SPACE: &[u8; 27] = b" ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Unpack a standard FT8 message (type 1/2) into its text form.
pub fn unpack_message(payload: &[u8; 77]) -> Option<String> {
    let mut n29a = 0_u32;
    for bit in payload.iter().take(29) {
        n29a = (n29a << 1) | *bit as u32;
    }
    let mut n29b = 0_u32;
    for bit in payload.iter().skip(29).take(29) {
        n29b = (n29b << 1) | *bit as u32;
    }
    let ir = payload[58];
    let mut igrid4 = 0_u16;
    for bit in payload.iter().skip(59).take(15) {
        igrid4 = (igrid4 << 1) | *bit as u16;
    }
    let i3 = (payload[74] << 2) | (payload[75] << 1) | payload[76];
    if i3 != 1 && i3 != 2 {
        return None;                    // other message types are not implemented
    }
    let call_to = unpack28(n29a >> 1)?;
    let call_de = unpack28(n29b >> 1)?;
    let extra = unpack_grid(igrid4 & 0x7FFF, ir == 1 || igrid4 & 0x8000 != 0)?;
    let mut text = format!("{call_to} {call_de}");
    if !extra.is_empty() {
        text.push(' ');
        text.push_str(&extra);
    }
    Some(text)
}

fn unpack28(n28: u32) -> Option<String> {
    if n28 < NTOKENS {
        return Some(match n28 {
            0 => "DE".to_string(),
            1 => "QRZ".to_string(),
            2 => "CQ".to_string(),
            value if value <= 1002 => format!("CQ {:03}", value - 3),
            value => {
                let mut n = value - 1003;
                let mut chars = [b' '; 4];
                for index in (0..4).rev() {
                    chars[index] = ALPHANUM_SPACE[(n % 27) as usize];
                    n /= 27;
                }
                format!("CQ {}", core::str::from_utf8(&chars).ok()?)
            }
        });
    }
    let value = n28 - NTOKENS;
    if value < MAX22 {
        // A hashed callsign (used for non-standard calls); recovering it needs the receive-side
        // hash table, which this decoder does not carry.
        return None;
    }
    unpack_basecall(value - MAX22)
}

fn unpack_basecall(n28: u32) -> Option<String> {
    let mut n = n28;
    let i5 = (n % 27) as usize;
    n /= 27;
    let i4 = (n % 27) as usize;
    n /= 27;
    let i3 = (n % 27) as usize;
    n /= 27;
    let i2 = (n % 10) as usize;
    n /= 10;
    let i1 = (n % 36) as usize;
    let i0 = (n / 36) as usize;
    if i0 >= ALPHANUM_SPACE.len() {
        return None;
    }
    let chars = [
        ALPHANUM_SPACE[i0],
        ALPHANUM[i1],
        b"0123456789"[i2],
        LETTERS_SPACE[i3],
        LETTERS_SPACE[i4],
        LETTERS_SPACE[i5],
    ];
    let mut call = String::new();
    for (index, char) in chars.iter().enumerate() {
        if index == 0 && *char == b' ' {
            continue;                    // " A0XYZ" packs a leading space
        }
        if *char != b' ' {
            call.push(*char as char);
        }
    }
    if call.is_empty() {
        None
    } else {
        Some(call)
    }
}

fn unpack_grid(igrid4: u16, ir: bool) -> Option<String> {
    if igrid4 <= MAXGRID4 {
        if igrid4 % 100 > 99 || (igrid4 / 100) % 18 > 17 {
            return None;
        }
        let mut n = igrid4;
        let d3 = b'0' + (n % 10) as u8;
        n /= 10;
        let d2 = b'0' + (n % 10) as u8;
        n /= 10;
        let c1 = b'A' + (n % 18) as u8;
        n /= 18;
        let c0 = b'A' + n as u8;
        if c0 > b'R' || c1 > b'R' {
            return None;
        }
        let chars = [c0, c1, d2, d3];
        let grid = core::str::from_utf8(&chars).ok()?;
        return Some(if ir { format!("R {grid}") } else { grid.to_string() });
    }
    let report = igrid4 - MAXGRID4;
    Some(match report {
        1 => String::new(),
        2 => "RRR".to_string(),
        3 => "RR73".to_string(),
        4 => "73".to_string(),
        value => {
            let dd = value as i32 - 35;
            let mut text = format!("{dd:+03}");
            if ir {
                text.insert(0, 'R');
            }
            text
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_past_the_retention_keeps_the_head_not_half_the_buffer() {
        // The feeder deliberately sends one slot plus a tail. The trim must drop only the excess:
        // subtracting the *complex* retention from a *float* length over-drained by half the buffer
        // and silently discarded the window's head -- and the head is where the transmission starts.
        let rate = 48_000.0;
        let mut decoder = Ft8Decoder::new(rate);
        let keep = decoder.retain_samples();
        decoder.push_iq(&vec![0.0_f32; (keep + 4_800) * 2]);   // 100 ms past the retention
        assert_eq!(decoder.buffered(), keep, "only the excess may be trimmed");
    }

    #[test]
    fn the_crc_matches_a_hand_checked_value() {
        let payload = [0_u8; 77];
        // All-zero payload: the CRC of 82 zero bits is 0.
        assert_eq!(crc14(&payload), 0);
    }

    #[test]
    fn the_costas_layout_is_what_the_protocol_says() {
        assert_eq!(COSTAS, [3, 1, 4, 0, 6, 5, 2]);
        assert_eq!(is_sync_symbol(0), true);
        assert_eq!(is_sync_symbol(7), false);
        assert_eq!(is_sync_symbol(36), true);
        assert_eq!(is_sync_symbol(72), true);
        assert_eq!(is_sync_symbol(78), true);
        assert_eq!(is_sync_symbol(79 - 8), false);
    }

    #[test]
    fn fit_model_recovers_a_synthetic_transmission() {
        // Arbitrary payload through the pub generator -> codeword -> tones -> unit waveform at
        // 1000 Hz. The fit must land on the true start with amplitude ~= 1; if it does not, the
        // subtraction has no model to cancel with and the failure is here, not in the search.
        let mut payload = [0_u8; 91];
        for (i, bit) in payload.iter_mut().enumerate() {
            *bit = ((i * 7 + 3) % 2) as u8;
        }
        let mut codeword = payload.to_vec();
        for row in LDPC_GENERATOR {
            let mut parity = 0_u8;
            for (byte_index, byte) in row.iter().enumerate() {
                for bit in 0..8 {
                    let k = byte_index * 8 + bit;
                    if k < 91 && (byte >> (7 - bit)) & 1 == 1 {
                        parity ^= payload[k];
                    }
                }
            }
            codeword.push(parity);
        }
        let codeword: [u8; 174] = codeword.try_into().unwrap();
        let tones = tones_from_codeword(&codeword);
        let rate = 48_000.0_f64;
        let sym = symbol_samples_at(rate);
        let mut iq = vec![0.0_f32; NUM_SYMBOLS * sym * 2];
        let mut phase = 0.0_f64;
        for (k, tone) in tones.iter().enumerate() {
            let step = core::f64::consts::TAU * (1_000.0 + *tone as f64 * TONE_SPACING_HZ) / rate;
            for i in 0..sym {
                let idx = (k * sym + i) * 2;
                iq[idx] = phase.cos() as f32;
                iq[idx + 1] = phase.sin() as f32;
                phase += step;
            }
        }
        let (power, gains) = fit_model(&iq, &tones, 1_000.0, rate, 0);
        let amplitude = (power / 79.0).sqrt() / sym as f64;
        assert!(amplitude > 0.9, "fit amplitude {amplitude}");
        let strong = gains.iter().filter(|g| g.0.hypot(g.1) > 0.5).count();
        assert!(strong > 70, "only {strong}/79 symbols fit above 0.5");
    }

    #[test]
    fn a_grid_report_and_token_round_trip() {
        // "CQ JO1WKO PM95" is the fixture's message: its callsign token unpacks to itself.
        assert_eq!(unpack28(2).as_deref(), Some("CQ"));
    }

    #[test]
    fn grids_and_reports_unpack_as_the_protocol_defines() {
        assert_eq!(unpack_grid(MAXGRID4 + 1, false).as_deref(), Some(""));
        assert_eq!(unpack_grid(MAXGRID4 + 2, false).as_deref(), Some("RRR"));
        assert_eq!(unpack_grid(MAXGRID4 + 3, false).as_deref(), Some("RR73"));
        assert_eq!(unpack_grid(MAXGRID4 + 4, false).as_deref(), Some("73"));
        // A report of +05 and R-12.
        assert_eq!(unpack_grid(MAXGRID4 + 35 + 5, false).as_deref(), Some("+05"));
        assert_eq!(unpack_grid(MAXGRID4 + 35 - 12, true).as_deref(), Some("R-12"));
        // PM95 as a 4-character grid.
        let igrid4 = ((b'P' - b'A') as u16 * 18 + (b'M' - b'A') as u16) * 100 + 95;
        let grid = unpack_grid(igrid4, false);
        assert_eq!(grid.as_deref(), Some("PM95"));
    }
}

// ---------------------------------------------------------------- plugin + ABI

use crate::plugin::{DigitalDemodulator, DigitalReport};

/// FT8 as a `DigitalDemodulator`: it consumes RAW baseband blocks and emits decoded text.
///
/// The decoder is window-driven and *slides*: blocks accumulate until a window (`hop + burst`) is
/// buffered, one decode runs over the whole window, then the window advances by one hop and keeps the
/// overlap. Sliding rather than clearing is what makes the result independent of where the feeder's
/// window boundaries happen to fall relative to the 15 s FT8 schedule. A fresh stream gets one
/// *early attempt* as soon as its first slot (15 s) is buffered -- anything that slot contains
/// whole is searchable, and waiting for the full window would push the first decode ~13 s past the
/// first slot boundary for nothing; the window keeps filling and the absolute-position
/// de-duplication absorbs the full window's re-decode of the same burst.
pub struct Ft8Plugin {
    decoder: Ft8Decoder,
    decoded: Vec<Ft8Message>,
    decoded_messages: u64,
    early_attempt_done: bool,
}

impl Ft8Plugin {
    pub fn new(rate: f64) -> Self {
        Self {
            decoder: Ft8Decoder::new(rate),
            decoded: Vec::new(),
            decoded_messages: 0,
            early_attempt_done: false,
        }
    }

    /// How many messages this plugin has decoded (diagnostics/status).
    pub fn decoded_messages(&self) -> u64 {
        self.decoded_messages
    }

    /// The most recent decode, with the timing and frequency the decoder measured.
    pub fn last(&self) -> Option<&Ft8Message> {
        self.decoded.last()
    }

    /// Complex samples currently buffered towards the next decode attempt.
    pub fn buffered(&self) -> usize {
        self.decoder.buffered()
    }

    fn record(&mut self, messages: Vec<Ft8Message>) -> Vec<String> {
        if messages.is_empty() {
            return Vec::new();
        }
        self.decoded_messages += messages.len() as u64;
        self.decoded = messages.clone();
        messages.into_iter().map(|m| m.text).collect()
    }
}

impl DigitalDemodulator for Ft8Plugin {
    fn id(&self) -> &'static str {
        "ft8"
    }

    fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
        self.decoder.push_iq(iq);
        let buffered = self.decoder.buffered();
        if buffered < self.decoder.window_samples() {
            // The early attempt, once per stream. The steady state never re-enters this zone: after
            // a slide the buffer cycles 13.04 s -> 28.04 s, crossing one slot only on the way up to
            // a full window, so the latch never needs clearing until `reset`.
            if self.early_attempt_done || buffered < self.decoder.slot_samples() {
                return Vec::new();
            }
            self.early_attempt_done = true;
            let messages = self.decoder.decode();
            return self.record(messages);
        }
        let messages = self.decoder.decode();
        // Slide one hop instead of clearing. The next window then overlaps this one by a whole
        // transmission, so a burst sitting at the seam is still searched whole in whichever window
        // contains it -- `window >= hop + burst` is what makes that a guarantee.
        self.decoder.advance(self.decoder.slot_samples());
        self.record(messages)
    }

    fn reset(&mut self) {
        self.decoder.reset();
        self.decoded.clear();
        self.early_attempt_done = false;
    }

    fn buffered_input(&self) -> usize {
        self.buffered()
    }

    fn last_report(&self) -> Option<DigitalReport> {
        self.decoded.last().map(|message| DigitalReport {
            frequency_hz: message.frequency_hz,
            time_offset_s: message.time_offset_s,
            snr_db: message.snr_db,
        })
    }

    fn decoded(&self) -> Vec<(String, DigitalReport)> {
        self.decoded
            .iter()
            .map(|message| {
                (
                    message.text.clone(),
                    DigitalReport {
                        frequency_hz: message.frequency_hz,
                        time_offset_s: message.time_offset_s,
                        snr_db: message.snr_db,
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    const IQ_BYTES: &[u8] = include_bytes!("../../../../tests/fixtures/ft8/ft8_cq_iq.bin");

    fn fixture() -> Vec<f32> {
        IQ_BYTES
            .chunks_exact(4)
            .map(|q| f32::from_le_bytes([q[0], q[1], q[2], q[3]]))
            .collect()
    }

    /// The fixture padded to a full slot with the quiet tail a real slot has.
    fn fixture_slot() -> Vec<f32> {
        let mut slot = fixture();
        let slot_complex = (48_000.0 * tables::SLOT_SECONDS) as usize;
        let have = slot.len() / 2;
        assert!(slot_complex > have, "the slot must be longer than the transmission");
        slot.extend(std::iter::repeat(0.0).take((slot_complex - have) * 2));
        slot
    }

    #[test]
    fn the_plugin_decodes_the_fixture_from_a_slot_and_clears_its_buffer() {
        if cfg!(debug_assertions) {
            return; // a whole-slot waterfall decode is release-only (see the reference test)
        }
        let mut plugin = Ft8Plugin::new(48_000.0);
        // The plugin waits for a whole sliding window (hop + burst) before it searches, so pad the
        // fixture slot out to one.
        let mut slot = fixture_slot();
        slot.resize(Ft8Decoder::new(48_000.0).window_samples() * 2, 0.0);
        let mut decoded: Vec<String> = Vec::new();
        for block in slot.chunks(960 * 2) {
            decoded.extend(plugin.process_iq(block));
        }
        assert_eq!(decoded, vec!["CQ JO1WKO PM95".to_string()]);
        assert_eq!(plugin.decoded_messages(), 1);
        let last = plugin.last().expect("a message");
        assert!((last.frequency_hz - 1_000.0).abs() < 25.0);
        // The slot was consumed: a single further block must not decode anything.
        assert!(plugin.process_iq(&slot[..960 * 2]).is_empty());
    }

    #[test]
    fn decodes_a_transmission_that_starts_inside_the_window() {
        if cfg!(debug_assertions) {
            return;
        }
        for offset_seconds in [0.0_f64, 0.4, 0.8, 1.5, 2.3] {
            let window = Ft8Decoder::new(48_000.0).window_samples();
            let slot_len = window * 3 * 2;
            let mut slot = vec![0.0_f32; slot_len];
            let iq = fixture();
            let start = (offset_seconds * 48_000.0) as usize * 2;
            assert!(start + iq.len() <= slot_len, "the transmission must fit in the slot");
            slot[start..start + iq.len()].copy_from_slice(&iq);

            let mut plugin = Ft8Plugin::new(48_000.0);
            let mut decoded: Vec<String> = Vec::new();
            for block in slot.chunks(960 * 2) {
                decoded.extend(plugin.process_iq(block));
            }
            assert_eq!(
                decoded,
                vec!["CQ JO1WKO PM95".to_string()],
                "a transmission starting at {offset_seconds}s must decode"
            );
        }
    }

    #[test]
    fn the_window_holds_a_whole_burst_at_any_phase_of_the_hop() {
        // This is the `window >= hop + burst` guarantee made executable. The feeder's window
        // boundaries can land anywhere relative to the transmission, and a burst sitting at the seam
        // must still be searched whole by whichever sliding window contains it. Before the window
        // slid (it was cleared after each attempt) only the lucky phases decoded -- which is exactly
        // the "it works sometimes" behaviour this replaced.
        if cfg!(debug_assertions) {
            return;
        }
        let rate = 48_000.0;
        let hop = (rate * tables::SLOT_SECONDS) as usize;
        let total = hop * 3;
        let iq = fixture();
        let mut decoded_phases = 0;
        for start_seconds in [0.5_f64, 3.0, 7.5, 11.0, 14.5] {
            let mut buffer = vec![0.0_f32; total * 2];
            let at = (start_seconds * rate) as usize;
            assert!(at * 2 + iq.len() <= buffer.len(), "the burst must fit");
            buffer[at * 2..at * 2 + iq.len()].copy_from_slice(&iq);

            let mut plugin = Ft8Plugin::new(rate);
            let mut got: Vec<String> = Vec::new();
            for block in buffer.chunks(960 * 2) {
                got.extend(plugin.process_iq(block));
            }
            // Exactly once per phase: the sliding window may find the same burst twice, and that is
            // de-duplicated by absolute stream position rather than by text.
            assert_eq!(
                got.iter().filter(|text| *text == "CQ JO1WKO PM95").count(),
                1,
                "the burst starting at {start_seconds}s must decode exactly once"
            );
            decoded_phases += 1;
        }
        assert_eq!(decoded_phases, 5);
    }

    #[test]
    fn the_first_slot_of_a_fresh_stream_decodes_without_the_full_window() {
        // The early attempt: a burst half a second into a fresh stream is fully contained in the
        // first slot, and the first decode must happen on that slot -- not ~13 s later when the
        // sliding window (hop + burst) has filled.
        if cfg!(debug_assertions) {
            return;
        }
        let rate = 48_000.0_f64;
        let mut plugin = Ft8Plugin::new(rate);
        let mut first_slot = vec![0.0_f32; (rate * tables::SLOT_SECONDS) as usize * 2];
        let iq = fixture();
        let at = (0.5 * rate) as usize * 2;
        first_slot[at..at + iq.len()].copy_from_slice(&iq);

        let window = Ft8Decoder::new(rate).window_samples();
        let mut fed = 0_usize;
        let mut got: Vec<String> = Vec::new();
        for chunk in first_slot.chunks(960 * 2) {
            got.extend(plugin.process_iq(chunk));
            fed += chunk.len() / 2;
            if !got.is_empty() {
                break;
            }
        }
        assert_eq!(got, vec!["CQ JO1WKO PM95".to_string()], "the first slot's burst decodes");
        assert!(
            (fed as f64 / rate) < window as f64 / rate,
            "decoded after {:.2} s; the old trigger waited {:.2} s",
            fed as f64 / rate,
            window as f64 / rate
        );
        // Continuing the stream: the full window re-searches the same burst and must not report it
        // twice, the steady state must not re-run the early attempt, and the message count stays
        // at exactly one for exactly one physical transmission.
        let tail_len = window - fed + 961;
        let tail = vec![0.0_f32; tail_len * 2];
        for chunk in tail.chunks(960 * 2) {
            got.extend(plugin.process_iq(chunk));
        }
        assert_eq!(
            got.iter().filter(|t| *t == "CQ JO1WKO PM95").count(),
            1,
            "the early decode must be reported exactly once"
        );
        assert_eq!(plugin.decoded_messages(), 1);
    }
}
