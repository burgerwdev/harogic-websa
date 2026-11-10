//! FT8 digital demodulator.
//!
//! FT8 is an 8-FSK mode: 79 symbols of 0.160 s, tones spaced 6.25 Hz, with three 7-symbol Costas
//! sync blocks at 0, 36 and 72. The 58 data symbols carry 174 bits, which are an LDPC(174,91)
//! codeword: 91 payload bits (77 of message plus a 14-bit CRC) and 83 parity bits.
//!
//! Decoding is the reverse, and follows the reference decoders (ft8_lib, WSJT-X): build a
//! *waterfall* (a Hann-windowed FFT per symbol), score candidates by a *local-contrast* sync metric
//! over the Costas tones, rank them with a heap, and for each run the max-log likelihood -> LDPC ->
//! CRC -> unpack. A bounded weak-signal fallback refines complex Costas timing/frequency and
//! forms coherent one-, two- and three-symbol metrics with BP-assisted OSD. It assumes no
//! payload bits. Decoding is *multi-pass* the way WSJT-X does it: everything one pass finds is
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

/// FFT bins per 6.25 Hz tone. Zero padding gives a 1.5625 Hz grid without changing the
/// analysis window: extending the window itself would mix more adjacent FSK symbols.
const FREQ_OSR: usize = 4;
/// Two-symbol Hann analysis window with sample-time normalization.
const WINDOW_SYMBOLS: usize = 2;
/// Time oversampling: sub-symbol subdivisions per symbol.
const TIME_OSR: usize = 2;
/// Positive-frequency FT8 search band. The 100 Hz lower edge accommodates
/// receiver/transmitter clock offsets near the band edge. F_MIN_HZ defines
/// waterfall bin zero and the corresponding reported frequency origin.
const F_MIN_HZ: f64 = 100.0;
const F_MAX_HZ: f64 = 3_000.0;
/// One FT8 transmission: 79 symbols of 160 ms.
const TRANSMISSION_S: f64 = 12.64;
/// The analysis window must contain a slot hop plus a complete transmission:
/// `window >= hop + burst + margin`. This covers every burst phase relative
/// to the slot schedule without requiring input alignment.
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
/// Additional matched-filter attempts per pass, after the coarse search. This
/// budget is per receiver job; it cannot create more jobs or acquisition work.
const MAX_REFINED_CANDIDATES: usize = 24;
/// Maximum search/cancellation passes per window. Each pass searches the
/// residual after successful decodes; an empty pass terminates the search.
const MAX_PASSES: usize = 4;
/// Minimum fitted amplitude for subtraction of the unit-envelope model.
/// This gates cancellation only; the SNR meter can use the fitted timing and
/// frequency at any IQ amplitude scale.
const MIN_FIT_AMPLITUDE: f64 = 0.03;

/// One decoded message, with what the decoder measured about the signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Ft8Message {
    pub text: String,
    /// Audio frequency of tone 0, in Hz.
    pub frequency_hz: f64,
    /// Where the transmission's first symbol sits in the slot buffer, in seconds (waterfall time).
    pub time_offset_s: f64,
    /// S/(N0 * 2500 Hz), estimated on original IQ after CRC success. NaN when
    /// unavailable (legacy numeric ABI); native JSON serializes this as null.
    pub snr_db: f64,
    pub snr: SnrEstimate,
}

mod snr;
pub use snr::{measure_decoded_snr, SnrEstimate};
use snr::estimate_snr;

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
        let window_samples = block * WINDOW_SYMBOLS;
        let min_bin = (F_MIN_HZ * SYMBOL_PERIOD_S) as usize;
        let max_bin = (F_MAX_HZ * SYMBOL_PERIOD_S) as usize + 1;
        let num_bins = max_bin - min_bin;
        let num_blocks = (samples.len() / 2) / block;
        let block_stride = TIME_OSR * FREQ_OSR * num_bins;
        let mut mag = vec![0_u8; num_blocks * block_stride];

        // Normalize by the actual window length; FFT padding adds no signal energy.
        let mut window = vec![0.0_f64; window_samples];
        for (i, w) in window.iter_mut().enumerate() {
            let x = (PI * i as f64 / window_samples as f64).sin();
            *w = x * x * (2.0 / window_samples as f64);
        }

        let mut plan = Bluestein::new(nfft);
        let mut frame_re = vec![0.0_f64; window_samples];
        let mut frame_im = vec![0.0_f64; window_samples];
        let mut re = vec![0.0_f64; nfft];
        let mut im = vec![0.0_f64; nfft];

        for blk in 0..num_blocks {
            let off = blk * block_stride;
            for ts in 0..TIME_OSR {
                // Shift the analysis frame by one sub-block.
                for i in 0..window_samples - subblock {
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
                    frame_re[window_samples - subblock + i] = sr;
                    frame_im[window_samples - subblock + i] = si;
                }
                for i in 0..window_samples {
                    re[i] = frame_re[i] * window[i];
                    im[i] = frame_im[i] * window[i];
                }
                re[window_samples..].fill(0.0);
                im[window_samples..].fill(0.0);
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
        let to_min = -(2 * WINDOW_SYMBOLS as i32);
        for to in to_min..self.num_blocks as i32 {
            // BP/OSD must determine 91 information bits from the observed data;
            // CRC is checked only after that decode. Fewer observations cannot
            // distinguish the LDPC codewords, even with noiseless likelihoods.
            // In particular, a few tail sync symbols have a high-variance mean
            // score and must not displace decodable words from this fixed budget.
            let data_symbols = (0..NUM_DATA_SYMBOLS).filter(|&k| {
                let symbol = k + if k < 29 { SYNC_LENGTH } else { 2 * SYNC_LENGTH };
                let block = to + symbol as i32;
                block >= 0 && block < self.num_blocks as i32
            }).count();
            if data_symbols * 3 < LDPC_K {
                continue;
            }
            for ts in 0..TIME_OSR {
                for fsb in 0..FREQ_OSR {
                    // The final base bin fits eight tones exactly at freq_sub=0.
                    // Fractional offsets there would put the last tone past F_MAX_HZ.
                    let end = self.num_bins - 7 - usize::from(fsb > 0);
                    for fo in 0..end {
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
    /// shot at the same likelihoods. Its generator-codeword and false-positive regressions
    /// verify that path; sensitivity depends on the complete signal and noise scenario.
    fn decode_candidate(&self, to: i32, ts: usize, fsb: usize, fo: usize) -> Option<[u8; 174]> {
        let llr = self.extract_likelihood(to, ts, fsb, fo);
        if let Some(bits) = ldpc_decode(&llr, MAX_LDPC_ITERATIONS) {
            if crc_ok(&bits) {
                return Some(bits);
            }
        }
        osd_decode(&llr)
    }
}

/// Mix one coarse candidate to baseband and integrate small sample groups. A
/// symbol retains at least 64 complex samples; the eight FSK tones are then
/// resolved by a full-symbol rectangular matched filter instead of the coarse
/// two-symbol Hann window, which mixes adjacent symbols at low SNR.
fn refined_decode(samples: &[f32], rate: f64, frequency: f64, coarse_time: f64) -> Option<[u8; 174]> {
    let decimation = (symbol_samples_at(rate) / 64).max(1);
    let step = 2.0 * PI * frequency / rate;
    let (ds, dc) = step.sin_cos();
    let (mut wr, mut wi) = (1.0, 0.0);
    let mut baseband = Vec::with_capacity(samples.len() / (2 * decimation));
    for group in samples.chunks_exact(2 * decimation) {
        let (mut re, mut im) = (0.0, 0.0);
        for pair in group.chunks_exact(2) {
            re += f64::from(pair[0]) * wr + f64::from(pair[1]) * wi;
            im += f64::from(pair[1]) * wr - f64::from(pair[0]) * wi;
            let next = wr * dc - wi * ds;
            wi = wr * ds + wi * dc;
            wr = next;
        }
        baseband.push((re / decimation as f64, im / decimation as f64));
    }
    let fs = rate / decimation as f64;
    let symbol = rate * SYMBOL_PERIOD_S / decimation as f64;
    let n = symbol.round() as usize;
    let correlate = |start: i64, frequency: f64| {
        if start < 0 || start as usize + n > baseband.len() { return (0.0, 0.0); }
        let (ds, dc) = (2.0 * PI * frequency / fs).sin_cos();
        let (mut wr, mut wi) = (1.0, 0.0);
        let (mut re, mut im) = (0.0, 0.0);
        for &(yr, yi) in &baseband[start as usize..start as usize + n] {
            re += yr * wr + yi * wi;
            im += yi * wr - yr * wi;
            let next = wr * dc - wi * ds;
            wi = wr * ds + wi * dc;
            wr = next;
        }
        (re, im)
    };
    // The streaming Hann frame's center lags its block label by half a
    // symbol. Converting that center to the symbol's beginning adds another
    // half symbol. Refine only the known Costas tones, without payload guesses.
    let mut start = (coarse_time * fs - symbol) as i64;
    let sync_power = |start: i64, offset: f64| {
        let mut power = 0.0;
        for k in 0..NUM_SYMBOLS {
            if !is_sync_symbol(k) { continue; }
            let tone = COSTAS[k % SYNC_OFFSET] as f64;
            let (re, im) = correlate(start + (k as f64 * symbol).round() as i64,
                                     offset + tone * TONE_SPACING_HZ);
            power += re * re + im * im;
        }
        power
    };
    let initial = start;
    let mut best = -1.0;
    for dt in -8..=8 {
        let trial = initial + (f64::from(dt) * symbol / 16.0).round() as i64;
        let power = sync_power(trial, 0.0);
        if power > best { best = power; start = trial; }
    }
    let mut offset = 0.0;
    best = -1.0;
    for df in -4..=4 {
        let trial = f64::from(df) * TONE_SPACING_HZ / (4 * FREQ_OSR) as f64;
        let power = sync_power(start, trial);
        if power > best { best = power; offset = trial; }
    }
    let initial = start;
    best = -1.0;
    for dt in -3..=3 {
        let trial = initial + (f64::from(dt) * symbol / 64.0).round() as i64;
        let power = sync_power(trial, offset);
        if power > best { best = power; start = trial; }
    }
    let mut hard_sync = 0;
    for k in 0..NUM_SYMBOLS {
        if !is_sync_symbol(k) { continue; }
        let time = start + (k as f64 * symbol).round() as i64;
        let mut powers = [0.0; 8];
        for tone in 0..8 {
            let (re, im) = correlate(time, offset + tone as f64 * TONE_SPACING_HZ);
            powers[tone] = re * re + im * im;
        }
        let strongest = (0..8).max_by(|&a, &b| powers[a].total_cmp(&powers[b])).unwrap();
        hard_sync += usize::from(strongest == COSTAS[k % SYNC_OFFSET] as usize);
    }
    if hard_sync < 8 { return None; }
    // Magnitude-only synchronization can tolerate a 10 ms timing error;
    // coherent symbol sums cannot (the highest tone rotates almost half a
    // cycle). Peak the complex Costas correlation within each seven-symbol
    // block before forming adjacent-symbol likelihoods.
    let coherent_sync = |start: i64, offset: f64| {
        let mut power = 0.0;
        for block in 0..3 {
            let (mut re, mut im) = (0.0, 0.0);
            for (j, &tone) in COSTAS.iter().enumerate() {
                let time = start + ((block * SYNC_OFFSET + j) as f64 * symbol).round() as i64;
                let (r, i) = correlate(time, offset + f64::from(tone) * TONE_SPACING_HZ);
                let (s, c) = (-2.0 * PI * offset * time as f64 / fs).sin_cos();
                re += r * c - i * s;
                im += r * s + i * c;
            }
            power += re * re + im * im;
        }
        power
    };
    let (initial_start, initial_offset) = (start, offset);
    best = -1.0;
    for dt in -8..=8 {
        let trial_time = initial_start + (f64::from(dt) * symbol / 32.0).round() as i64;
        for df in -4..=4 {
            let trial_offset = initial_offset + f64::from(df) * TONE_SPACING_HZ / 64.0;
            let power = coherent_sync(trial_time, trial_offset);
            if power > best { best = power; start = trial_time; offset = trial_offset; }
        }
    }
    // Rebuild tone correlations at the coherent alignment.
    let mut tones = [[(0.0, 0.0); 8]; NUM_SYMBOLS];
    for k in 0..NUM_SYMBOLS {
        let time = start + (k as f64 * symbol).round() as i64;
        let (s, c) = (-2.0 * PI * offset * time as f64 / fs).sin_cos();
        for tone in 0..8 {
            let (re, im) = correlate(time, offset + tone as f64 * TONE_SPACING_HZ);
            tones[k][tone] = (re * c - im * s, re * s + im * c);
        }
    }
    // Coherently combine one, two and three symbols. The maximum over all
    // tone hypotheses supplies each bit metric; no callsigns or CQ bits are
    // assumed. CRC is still checked only after BP/OSD chooses a codeword.
    for group in 1..=3 {
        let mut llr = [0.0_f64; 174];
        for half in 0..2 {
            for first in (0..29).step_by(group) {
                let count = group.min(29 - first);
                let mut maxima = [[0.0_f64; 2]; 9];
                for word in 0..1_usize << (3 * count) {
                    let (mut re, mut im) = (0.0, 0.0);
                    for j in 0..count {
                        let bits = (word >> (3 * (count - 1 - j))) & 7;
                        let (r, i) = tones[7 + half * 36 + first + j][GRAY[bits] as usize];
                        re += r; im += i;
                    }
                    let metric = (re * re + im * im).sqrt();
                    for bit in 0..3 * count {
                        let value = (word >> (3 * count - 1 - bit)) & 1;
                        maxima[bit][value] = maxima[bit][value].max(metric);
                    }
                }
                for bit in 0..3 * count {
                    llr[half * 87 + first * 3 + bit] = maxima[bit][1] - maxima[bit][0];
                }
            }
        }
        let variance = llr.iter().map(|x| x * x).sum::<f64>() / 174.0;
        if variance <= 0.0 { continue; }
        // Amplitude-difference metrics use variance 8 (unlike the coarse
        // log-power metrics' 24). Overconfident messages impede BP convergence.
        let factor = (8.0 / variance).sqrt();
        for value in &mut llr { *value *= factor; }
        let mut posterior = llr;
        if let Some(bits) = ldpc_decode_with_posterior(&llr, MAX_LDPC_ITERATIONS, &mut posterior).filter(crc_ok) {
            return Some(bits);
        }
        if let Some(bits) = osd_decode(&posterior) { return Some(bits); }
        if let Some(bits) = osd_decode(&llr) { return Some(bits); }
    }
    None
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

    /// Append complete interleaved I/Q pairs, discarding an unmatched trailing float per call.
    pub fn push_iq(&mut self, iq: &[f32]) {
        let iq = &iq[..iq.len() / 2 * 2];
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
            let mut refined = Vec::<(f64, f64)>::new();
            for (_score, to, ts, fsb, fo) in waterfall.find_candidates() {
                let frequency = F_MIN_HZ + (fo as f64 + fsb as f64 / FREQ_OSR as f64) * TONE_SPACING_HZ;
                let time = (to as f64 + ts as f64 / TIME_OSR as f64) * SYMBOL_PERIOD_S;
                let Some(codeword) = waterfall.decode_candidate(to, ts, fsb, fo).or_else(|| {
                    if refined.len() >= MAX_REFINED_CANDIDATES || refined.iter().any(|&(f,t)|
                        (f-frequency).abs() < TONE_SPACING_HZ / 2.0 && (t-time).abs() < SYMBOL_PERIOD_S) {
                        return None;
                    }
                    refined.push((frequency, time));
                    refined_decode(&scratch, self.rate, frequency, time)
                }) else {
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
                    let model = subtract_transmission(&mut scratch, self.rate, &codeword, to, ts, fsb, fo);
                    // Cancellation is only a search aid. Always measure the
                    // original received IQ, never the noise-carved residual.
                    let snr = model.as_ref().map(|m| estimate_snr(&self.samples, self.rate,
                        &tones_from_codeword(&codeword), m)).unwrap_or_default();
                    found_this_pass += 1;
                    out.push(Ft8Message {
                        text,
                        frequency_hz: F_MIN_HZ
                            + (fo as f64 + fsb as f64 / FREQ_OSR as f64) * TONE_SPACING_HZ,
                        time_offset_s: (to as f64 + ts as f64 / TIME_OSR as f64) * SYMBOL_PERIOD_S,
                        snr_db: snr.value.unwrap_or(f64::NAN),
                        snr,
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

struct CancellationModel {
    frequency: f64,
    start: i64,
    gains: [(f64, f64); NUM_SYMBOLS],
}

fn symbol_phases(tones: &[u8; NUM_SYMBOLS], frequency: f64, rate: f64) -> [f64; NUM_SYMBOLS] {
    let duration = symbol_samples_at(rate) as f64 / rate;
    let mut phase = 0.0;
    core::array::from_fn(|index| {
        let previous = phase;
        phase = (phase
            + 2.0 * PI * (frequency + f64::from(tones[index]) * TONE_SPACING_HZ) * duration)
            .rem_euclid(2.0 * PI);
        previous
    })
}

/// Squared complex residual over the same complete observation for either model.
/// Summing the residual explicitly avoids cancellation of two almost equal powers
/// when a noiseless waveform fits to floating-point precision.
fn cancellation_residual(
    samples: &[f32],
    rate: f64,
    tones: &[u8; NUM_SYMBOLS],
    model: &CancellationModel,
) -> f64 {
    let symbol = symbol_samples_at(rate);
    let count = samples.len() / 2;
    let mut consumed = 0;
    let mut residual = 0.0;
    for (k, &tone) in tones.iter().enumerate() {
        let start = model.start + (k * symbol) as i64;
        if start < 0 || start + symbol as i64 > count as i64 {
            continue;
        }
        let start = start as usize;
        for sample in &samples[consumed * 2..start * 2] {
            residual += f64::from(*sample).powi(2);
        }
        let step = 2.0 * PI * (model.frequency + f64::from(tone) * TONE_SPACING_HZ) / rate;
        let (ds, dc) = step.sin_cos();
        let (mut wr, mut wi) = (1.0, 0.0);
        let (gr, gi) = model.gains[k];
        for index in start..start + symbol {
            let re = f64::from(samples[index * 2]) - (gr * wr - gi * wi);
            let im = f64::from(samples[index * 2 + 1]) - (gr * wi + gi * wr);
            residual += re * re + im * im;
            let next_re = wr * dc - wi * ds;
            wi = wr * ds + wi * dc;
            wr = next_re;
        }
        consumed = start + symbol;
    }
    for sample in &samples[consumed * 2..] {
        residual += f64::from(*sample).powi(2);
    }
    residual
}

/// A phase-continuous burst can use one complex gain instead of 79 independent
/// gains. This avoids fitting noise or a weaker co-channel station separately
/// in every symbol. Keep the independent-gain model when its improvement pays
/// for those additional parameters under the Gaussian AIC comparison below.
fn select_cancellation_model(
    samples: &[f32],
    rate: f64,
    tones: &[u8; NUM_SYMBOLS],
    original: CancellationModel,
) -> CancellationModel {
    let symbol = symbol_samples_at(rate);
    let phases = symbol_phases(tones, original.frequency, rate);
    let mut previous: Option<f64> = None;
    let mut unwrapped = 0.0;
    let (mut weight, mut x, mut y, mut xx, mut xy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (k, &(gr, gi)) in original.gains.iter().enumerate() {
        let reliability = gr * gr + gi * gi;
        if !reliability.is_finite() || reliability == 0.0 {
            continue;
        }
        let (s, c) = phases[k].sin_cos();
        let angle = (gi * c - gr * s).atan2(gr * c + gi * s);
        unwrapped = if let Some(last) = previous {
            unwrapped + (angle - last + PI).rem_euclid(2.0 * PI) - PI
        } else {
            angle
        };
        previous = Some(angle);
        let time = k as f64 * symbol as f64 / rate;
        weight += reliability;
        x += reliability * time;
        y += reliability * unwrapped;
        xx += reliability * time * time;
        xy += reliability * time * unwrapped;
    }
    let denominator = weight * xx - x * x;
    if denominator <= 0.0 || !denominator.is_finite() {
        return original;
    }
    let correction = (weight * xy - x * y) / denominator / (2.0 * PI);
    // Phase unwrapping may fail on an incoherent/vanishing signal. The original
    // search already localized the frequency to this sub-bin neighbourhood.
    if !correction.is_finite() || correction.abs() > TONE_SPACING_HZ / FREQ_OSR as f64 {
        return original;
    }
    let frequency = original.frequency + correction;
    let phases = symbol_phases(tones, frequency, rate);
    let count = samples.len() / 2;
    let mut best_power = -1.0;
    let mut best = None;
    for step in -8_i32..=8 {
        let start = original.start + (f64::from(step) * symbol as f64 / 128.0) as i64;
        let (_, gains) = fit_model(samples, tones, frequency, rate, start);
        let (mut real, mut imaginary, mut fitted) = (0.0, 0.0, 0_usize);
        for (k, &(gr, gi)) in gains.iter().enumerate() {
            let first = start + (k * symbol) as i64;
            if first < 0 || first + symbol as i64 > count as i64 {
                continue;
            }
            let (s, c) = phases[k].sin_cos();
            real += gr * c + gi * s;
            imaginary += gi * c - gr * s;
            fitted += 1;
        }
        if fitted == 0 {
            continue;
        }
        let power = (real * real + imaginary * imaginary) / fitted as f64;
        if power <= best_power || !power.is_finite() {
            continue;
        }
        best_power = power;
        real /= fitted as f64;
        imaginary /= fitted as f64;
        let gains = core::array::from_fn(|k| {
            let first = start + (k * symbol) as i64;
            if first < 0 || first + symbol as i64 > count as i64 {
                return (0.0, 0.0);
            }
            let (s, c) = phases[k].sin_cos();
            (real * c - imaginary * s, real * s + imaginary * c)
        });
        best = Some(CancellationModel {
            frequency,
            start,
            gains,
        });
    }
    let Some(coherent) = best else {
        return original;
    };
    let independent_error = cancellation_residual(samples, rate, tones, &original);
    let coherent_error = cancellation_residual(samples, rate, tones, &coherent);
    let fitted = (0..NUM_SYMBOLS)
        .filter(|k| {
            let first = original.start + (k * symbol) as i64;
            first >= 0 && first + symbol as i64 <= count as i64
        })
        .count();
    // Circular complex Gaussian noise gives 2*N real observations. The gains
    // have 2 vs 2*fitted real parameters; frequency, start and noise variance
    // are estimated in both models and cancel from the parameter difference.
    // AIC = 2*k + 2*N*ln(RSS/(2*N)) + common terms (Akaike, 1974,
    // https://doi.org/10.1109/TAC.1974.1100705). No empirically tuned gain threshold.
    let prefer_coherent = coherent_error == 0.0
        || (independent_error > 0.0
            && (samples.len() as f64) * (coherent_error / independent_error).ln()
                <= 4.0 * fitted.saturating_sub(1) as f64);
    if prefer_coherent {
        coherent
    } else {
        original
    }
}

/// Remove one decoded transmission from the samples, the way WSJT-X does between passes.
///
/// The waterfall only localizes the signal to a sub-bin (`FREQ_OSR` bins per tone) and a
/// sub-symbol (`TIME_OSR` blocks per symbol), and its block time carries up to a symbol of lead
/// (the analysis window straddles the symbols) — far too coarse to cancel against: one bin of
/// residual frequency error rotates the phase repeatedly within a burst and the subtraction
/// destroys itself. So the model is fitted to the data first — coarse time, then frequency, then
/// fine time, each round maximizing the correlation power. A continuous-phase fit then refines
/// the carrier and uses one burst gain when its residual supports that model. Otherwise the
/// per-symbol gains retain amplitude and phase variation between transmitter and receiver.
fn subtract_transmission(
    samples: &mut [f32],
    rate: f64,
    codeword: &[u8; 174],
    to: i32,
    ts: usize,
    fsb: usize,
    fo: usize,
) -> Option<CancellationModel> {
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
    let fine_center = best_t0;
    for step in -8_i32..=8 {
        let start = fine_center + (step as f64 * (sym as f64 / 16.0)) as i64;
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
    let polish_center = best_t0;
    for step in -8_i32..=8 {
        let start = polish_center + (step as f64 * (sym as f64 / 128.0)) as i64;
        let (power, _) = fit_model(samples, &tones, best_freq, rate, start);
        if power > best_power {
            best_power = power;
            best_t0 = start;
        }
    }
    let (power, gains) = fit_model(samples, &tones, best_freq, rate, best_t0);
    let fitted = gains.iter().filter(|g| *g != &(0.0, 0.0)).count();
    if fitted == 0 {
        return None;
    }
    // Mean fitted amplitude of the unit-envelope model. A real transmission lands near its true
    // amplitude; a stale candidate (already subtracted, or an alias whose window holds nothing)
    // lands near zero and must not be "subtracted".
    let amplitude = (power / fitted as f64).sqrt() / sym as f64;
    let model = select_cancellation_model(samples, rate, &tones,
        CancellationModel { frequency:best_freq, start:best_t0, gains });
    // The search's absolute cancellation-amplitude gate is not a metrology
    // gate: a perfectly valid SNR must survive a common IQ scale change.
    if amplitude < MIN_FIT_AMPLITUDE { return Some(model); }
    let CancellationModel { frequency: best_freq, start: best_t0, gains } = model;
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
    Some(CancellationModel {frequency:best_freq,start:best_t0,gains})
}

// ---------------------------------------------------------------- LDPC (sum-product)

/// Soft-decision LDPC(174,91) decode. `None` when the parity checks never pass.
fn ldpc_decode(llr: &[f64; 174], max_iterations: usize) -> Option<[u8; 174]> {
    let mut posterior = *llr;
    ldpc_decode_with_posterior(llr, max_iterations, &mut posterior)
}

fn ldpc_decode_with_posterior(llr: &[f64; 174], max_iterations: usize, posterior: &mut [f64; 174]) -> Option<[u8; 174]> {
    // Retain the most consistent BP iteration for ordered-statistics ranking.
    // The channel LLRs remain a separate fallback; no payload bits are fixed.
    // tov[n][m]: message from variable node n to its m-th check; toc[m][n]: the reverse.
    let mut tov = [[0.0_f64; 3]; 174];
    let mut toc = [[0.0_f64; 7]; 83];
    let mut best_errors = usize::MAX;
    for _iteration in 0..max_iterations {
        // Hard decision from the channel value plus the current extrinsic information.
        let mut plain = [0_u8; 174];
        let mut plain_sum = 0;
        let mut reliability = [0.0; 174];
        for n in 0..174 {
            let total = llr[n] + tov[n][0] + tov[n][1] + tov[n][2];
            reliability[n] = total;
            plain[n] = u8::from(total > 0.0);
            plain_sum += plain[n] as i32;
        }
        if plain_sum == 0 {
            // Converged to the all-zero word, which this code forbids.
            return None;
        }
        let errors = parity_errors(&plain);
        if errors < best_errors {
            best_errors = errors;
            *posterior = reliability;
        }
        if errors == 0 {
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
/// converge. The 83 check pivots use the least reliable independent columns, leaving the most
/// reliable 91-bit information basis for a bounded error-pattern search. This is the parity-check
/// dual of generator-based OSD (Fossorier and Lin, IEEE TIT 41(5), 1995,
/// https://doi.org/10.1109/18.412683). Every pattern yields a word that
/// satisfies all parity checks, so the discriminator is not the syndrome but the distance to the
/// received LLRs: patterns are ranked by the reliability-weighted cost of the bits they move, and
/// only the single best word is CRC-checked. Gating one candidate instead of ~4200 individually
/// bounds the fallback to one CRC trial per candidate.
///
/// Enumeration covers weight 0, 1 and 2 on the information set. Parity bits follow from that
/// choice; independently flipping a computed parity bit would leave valid-LDPC space.
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

    // Pivot the least reliable independent columns of H. Their complement is the most
    // reliable information basis: order-0 re-encodes its hard decisions, and orders 1/2
    // correct a bounded number of its errors. Pivot decisions still contribute to the
    // full received-word distance.
    let mut in_info = [true; 174];
    let mut pivot_pos = [0_usize; 83];
    let mut rank = 0;
    for &col in order.iter().rev() {
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
        hard_par[i] = (llr[pivot_pos[i]] > 0.0) as u8;
        for (j, &p) in info_pos.iter().enumerate() {
            if rows[i][p / 64] >> (p % 64) & 1 == 1 {
                c[j / 64] |= 1 << (j % 64);
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
    let call_to = unpack28(n29a >> 1, n29a & 1 != 0, i3)?;
    let call_de = unpack28(n29b >> 1, n29b & 1 != 0, i3)?;
    let extra = unpack_grid(igrid4 & 0x7FFF, ir == 1 || igrid4 & 0x8000 != 0)?;
    let mut text = format!("{call_to} {call_de}");
    if !extra.is_empty() {
        text.push(' ');
        text.push_str(&extra);
    }
    Some(text)
}

fn unpack28(n28: u32, suffix: bool, kind: u8) -> Option<String> {
    if n28 < NTOKENS {
        return Some(match n28 {
            0 => "DE".to_string(),
            1 => "QRZ".to_string(),
            2 => "CQ".to_string(),
            value if value <= 1002 => format!("CQ {:03}", value - 3),
            value if value <= 532_443 => {
                let mut n = value - 1003;
                let mut chars = [b' '; 4];
                for index in (0..4).rev() {
                    chars[index] = LETTERS_SPACE[(n % 27) as usize];
                    n /= 27;
                }
                format!("CQ {}", core::str::from_utf8(&chars).ok()?.trim_start())
            }
            _ => return None,
        });
    }
    let value = n28 - NTOKENS;
    if value < MAX22 {
        // A hashed callsign (used for non-standard calls); recovering it needs the receive-side
        // hash table, which this decoder does not carry.
        return None;
    }
    let mut call = unpack_basecall(value - MAX22)?;
    // Token fields have no suffix; this bit belongs only to standard callsigns.
    if suffix {
        call.push_str(if kind == 1 { "/R" } else { "/P" });
    }
    Some(call)
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
    // Protocol compression of standard prefixes, inverse of ft8_lib pack_basecall.
    if chars.starts_with(b"3D0") && chars[3] != b' ' {
        call.insert(2, 'A');
    } else if chars[0] == b'Q' && chars[1].is_ascii_uppercase() {
        call.replace_range(..1, "3X");
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

    fn transmitter_fixture_values(field: &str) -> Vec<u8> {
        let manifest = include_str!("../../../../../tests/fixtures/ft8/manifest.json");
        let key = format!("\"{field}\":");
        manifest.split(&key).nth(1).unwrap()
            .split('[').nth(1).unwrap().split(']').next().unwrap().split(',')
            .map(|value| value.trim().parse().unwrap()).collect()
    }


    #[test]
    fn osd_recovers_one_and_two_unreliable_bits_against_a_generator_codeword() {
        // Encode from the protocol generator, independently of the decoder's
        // check-matrix elimination and distance calculation.
        let mut expected = [0_u8; 174];
        for (index, bit) in expected[..77].iter_mut().enumerate() {
            *bit = ((index * 13 + index / 3) % 2) as u8;
        }
        let payload: [u8; 77] = expected[..77].try_into().unwrap();
        let crc = crc14(&payload);
        for index in 0..14 { expected[77 + index] = ((crc >> (13 - index)) & 1) as u8; }
        for (row_index, row) in LDPC_GENERATOR.iter().enumerate() {
            let parity = (0..91).fold(0, |acc, bit|
                acc ^ (expected[bit] & ((row[bit / 8] >> (7 - bit % 8)) & 1)));
            expected[91 + row_index] = parity;
        }
        assert_eq!(parity_errors(&expected), 0);
        assert!(crc_ok(&expected));
        for errors in [&[0][..], &[40], &[91], &[173], &[0, 173], &[40, 91]] {
            let mut hard = expected;
            let llr = core::array::from_fn(|index| {
                let weak = errors.contains(&index);
                if weak { hard[index] ^= 1; }
                (if hard[index] == 0 { -1.0 } else { 1.0 }) * if weak { 0.25 } else { 12.0 }
            });
            assert!(parity_errors(&hard) > 0, "the received hard word has errors");
            // The known codeword differs only at the weak positions (cost <=0.5).
            // Every other valid word must differ at a strong position (cost >=12):
            // independently check all alternatives confined to the weak positions.
            for mask in 1..1usize << errors.len() {
                let mut alternate = expected;
                for (bit, &index) in errors.iter().enumerate() {
                    if mask >> bit & 1 != 0 { alternate[index] ^= 1; }
                }
                assert!(parity_errors(&alternate) > 0);
            }
            assert_eq!(osd_decode(&llr), Some(expected), "unreliable positions {errors:?}");
        }
    }

    #[test]
    fn osd_reencodes_the_most_reliable_information_basis() {
        // The transmitter fixture supplies the independent systematic codeword.
        // Its first 91 bits determine every parity bit, without elimination on H.
        let expected: [u8; 174] = transmitter_fixture_values("codeword_bits").try_into().unwrap();
        for unreliable in [3, 7, 83] {
            let llr = core::array::from_fn(|index| {
                let sign = if expected[index] == 0 { -1.0 } else { 1.0 };
                if (91..91 + unreliable).contains(&index) { -sign * 0.01 } else { sign * 12.0 }
            });
            // All systematic bits are correct and independently reliable. The
            // expected codeword costs <=0.83; every other codeword must change
            // at least one of these bits and costs >=12. It is the unique ML
            // answer and is already in the order-0 most-reliable-basis list.
            assert!(unreliable as f64 * 0.01 < 12.0);
            assert_eq!(osd_decode(&llr), Some(expected), "{unreliable} weak parity bits");
        }
    }

    #[test]
    fn odd_pushes_preserve_iq_pairs_and_sample_positions() {
        let mut decoder = Ft8Decoder::new(48_000.0);
        decoder.push_iq(&[99.0]);
        decoder.push_iq(&[98.0]);
        assert_eq!(decoder.buffered(), 0);
        decoder.push_iq(&[1.0, -1.0, 97.0]);
        decoder.push_iq(&[2.0, -2.0, 96.0]);
        assert_eq!(decoder.buffered(), 2);
        assert_eq!(decoder.total_pushed, 2);
        assert_eq!(decoder.samples, [1.0, -1.0, 2.0, -2.0]);
        decoder.advance(1);
        assert_eq!(decoder.samples, [2.0, -2.0]);
        assert_eq!(decoder.total_pushed - decoder.buffered(), 1);
    }

    #[test]
    fn unsupported_message_types_and_unknown_hashes_are_rejected() {
        for message_type in [0_u8, 3, 4, 5, 6, 7] {
            let mut payload = [0_u8; 77];
            payload[74] = message_type >> 2;
            payload[75] = (message_type >> 1) & 1;
            payload[76] = message_type & 1;
            assert_eq!(unpack_message(&payload), None);
        }
        for call_offset in [0, 29] {
            for hash in [NTOKENS, NTOKENS + MAX22 - 1] {
                let mut payload = [0_u8; 77];
                payload[76] = 1;
                let packed = hash << 1;
                for bit in 0..29 {
                    payload[call_offset + bit] = ((packed >> (28 - bit)) & 1) as u8;
                }
                assert_eq!(unpack_message(&payload), None);
            }
        }
    }

    #[test]
    fn seeded_noise_does_not_decode_a_message() {
        if cfg!(debug_assertions) {
            return; // Full waterfall searches require a release build.
        }
        let mut decoder = Ft8Decoder::new(6_000.0);
        let mut state = 0x48ab_19f3_u32;
        let iq: Vec<f32> = (0..decoder.retain_samples() * 2)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
            })
            .collect();
        decoder.push_iq(&iq);
        assert!(decoder.decode().is_empty());
        assert_eq!(decoder.buffered(), decoder.retain_samples());
    }

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
    fn ft8_review_subtraction_cancels_exact_known_timing_grid_points() {
        // Independent transmitter symbols and codeword from the committed Python
        // fixture. A noiseless signal on every refinement grid has an exact
        // least-squares solution: subtracting it must leave only f32 roundoff.
        let word: [u8; 174] = transmitter_fixture_values("codeword_bits").try_into().unwrap();
        let tones = transmitter_fixture_values("tones");
        let rate = 6_000.0;
        let symbol = 960;
        for offset in [0, 15, 75, 135, 255, 495, 735, 945] {
            let start = 2 * symbol + offset;
            let mut iq = vec![0.0_f32; (start + 81 * symbol) * 2];
            let mut phase = 0.0_f64;
            for (k, tone) in tones.iter().enumerate() {
                let step = 2.0 * PI * (800.0 + f64::from(*tone) * 6.25) / rate;
                for sample in 0..symbol {
                    let index = (start + k * symbol + sample) * 2;
                    iq[index] = phase.cos() as f32;
                    iq[index + 1] = phase.sin() as f32;
                    phase += step;
                }
            }
            subtract_transmission(&mut iq, rate, &word, 2, 0, 0, 112);
            let residual = iq.iter().map(|value| f64::from(*value).powi(2)).sum::<f64>()
                / (tones.len() * symbol) as f64;
            assert!(residual < 1e-9, "exact timing offset {offset}: residual power {residual}");
        }
    }

    #[test]
    fn coherent_subtraction_refines_frequency_between_search_grid_points() {
        let word: [u8; 174] = transmitter_fixture_values("codeword_bits")
            .try_into()
            .unwrap();
        let tones = transmitter_fixture_values("tones");
        let symbol = 960;
        let start = 2 * symbol;
        // At 6001 Hz each tone advances a non-integer number of cycles per
        // 960-sample symbol, so the carrier alone cannot represent its phase.
        for rate in [6_000.0, 6_001.0] {
            for frequency in [800.03125, 799.9375, 800.103] {
                let mut iq = vec![0.0_f32; (start + 81 * symbol) * 2];
                let mut phase = 0.37_f64;
                for (k, tone) in tones.iter().enumerate() {
                    let step = 2.0 * PI * (frequency + f64::from(*tone) * 6.25) / rate;
                    for index in 0..symbol {
                        let offset = (start + k * symbol + index) * 2;
                        iq[offset] = phase.cos() as f32;
                        iq[offset + 1] = phase.sin() as f32;
                        phase += step;
                    }
                }
                // A single phase-continuous transmitter has an exact constant-gain
                // model even between the coarse frequency search points. The known
                // transmitter waveform is independent of the fitting implementation.
                subtract_transmission(&mut iq, rate, &word, 2, 0, 0, 112);
                let residual = iq
                    .iter()
                    .map(|value| f64::from(*value).powi(2))
                    .sum::<f64>()
                    / (tones.len() * symbol) as f64;
                assert!(
                    residual < 1e-9,
                    "rate {rate}, frequency {frequency}: residual power {residual}"
                );
            }
        }
    }

    #[test]
    fn coherent_subtraction_preserves_partial_boundary_symbols() {
        let word: [u8; 174] = transmitter_fixture_values("codeword_bits")
            .try_into()
            .unwrap();
        let tones = transmitter_fixture_values("tones");
        let rate = 6_001.0;
        let symbol = 960_usize;
        let start = -(symbol as i64 / 2);
        let count = 78 * symbol;
        let mut iq = vec![0.0_f32; count * 2];
        let mut phase = 0.37_f64;
        for (k, tone) in tones.iter().enumerate() {
            let step = 2.0 * PI * (800.03125 + f64::from(*tone) * 6.25) / rate;
            for sample in 0..symbol {
                let absolute = start + (k * symbol + sample) as i64;
                if (0..count as i64).contains(&absolute) {
                    iq[absolute as usize * 2] = phase.cos() as f32;
                    iq[absolute as usize * 2 + 1] = phase.sin() as f32;
                }
                phase += step;
            }
        }
        let original = iq.clone();
        subtract_transmission(&mut iq, rate, &word, -1, 1, 0, 112);
        // The fit observes complete symbols only. Missing samples cannot be
        // replaced by zeros or cause a partial boundary symbol to be subtracted.
        let first_complete = symbol / 2;
        let last_complete_end = count - symbol / 2;
        assert_eq!(&iq[..first_complete * 2], &original[..first_complete * 2]);
        assert_eq!(
            &iq[last_complete_end * 2..],
            &original[last_complete_end * 2..]
        );
        let residual = iq[first_complete * 2..last_complete_end * 2]
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            / (last_complete_end - first_complete) as f64;
        assert!(residual < 1e-9, "complete-symbol residual power {residual}");
    }

    #[test]
    fn coherent_subtraction_preserves_the_fit_of_varying_symbol_gains() {
        let word: [u8; 174] = transmitter_fixture_values("codeword_bits")
            .try_into()
            .unwrap();
        let tones = transmitter_fixture_values("tones");
        let rate = 6_000.0;
        let symbol = 960;
        let start = 2 * symbol;
        let mut iq = vec![0.0_f32; (start + 81 * symbol) * 2];
        let mut phase = 0.37_f64;
        for (k, tone) in tones.iter().enumerate() {
            let step = 2.0 * PI * (800.0 + f64::from(*tone) * 6.25) / rate;
            let amplitude = 0.4 + (k % 5) as f64 * 0.15;
            let channel_phase = ((k * 13) % 7) as f64 * 0.2;
            for index in 0..symbol {
                let offset = (start + k * symbol + index) * 2;
                iq[offset] = (amplitude * (phase + channel_phase).cos()) as f32;
                iq[offset + 1] = (amplitude * (phase + channel_phase).sin()) as f32;
                phase += step;
            }
        }
        // These independent gain changes are part of the desired transmitter.
        // A constant burst gain must not replace a better per-symbol fit.
        subtract_transmission(&mut iq, rate, &word, 2, 0, 0, 112);
        let residual = iq
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            / (tones.len() * symbol) as f64;
        assert!(residual < 1e-9, "varying-gain residual power {residual}");
    }

    #[test]
    fn ft8_frequency_grid_matches_a_fixed_two_symbol_dft() {
        // Independently sum the complex DFT on a two-symbol Hann window.
        // Changing frequency granularity must not change the physical window,
        // its normalization, or which input samples a waterfall time denotes.
        let rate = 6_000.0;
        let symbol = 960_usize;
        let window_samples = 2 * symbol;
        let mut iq = Vec::new();
        for sample in 0..6 * symbol {
            let time = sample as f64 / rate;
            let envelope = if sample < 2_300 { 1.0 } else { 0.4 };
            iq.push((envelope * ((2.0 * PI * 802.34375 * time).cos()
                + 0.3 * (2.0 * PI * 1376.5625 * time).cos())) as f32);
            iq.push((envelope * ((2.0 * PI * 802.34375 * time).sin()
                + 0.3 * (2.0 * PI * 1376.5625 * time).sin())) as f32);
        }
        let waterfall = Waterfall::build(&iq, rate);
        for (block, time_sub) in [(0, 0), (1, 1), (3, 0), (5, 1)] {
            let end = (block * 2 + time_sub + 1) * (symbol / 2);
            for bin in [112, 204] {
                for frequency_sub in 0..4 {
                    let frequency = 100.0 + (bin as f64 + frequency_sub as f64 / 4.0) * 6.25;
                    let mut real = 0.0;
                    let mut imaginary = 0.0;
                    for index in 0..window_samples {
                        let input = end as isize - window_samples as isize + index as isize;
                        if input < 0 { continue; }
                        let weight = (PI * index as f64 / window_samples as f64).sin().powi(2)
                            * 2.0 / window_samples as f64;
                        let angle = 2.0 * PI * frequency * index as f64 / rate;
                        let re = iq[input as usize * 2] as f64;
                        let im = iq[input as usize * 2 + 1] as f64;
                        real += weight * (re * angle.cos() + im * angle.sin());
                        imaginary += weight * (im * angle.cos() - re * angle.sin());
                    }
                    let expected = (240.0 + 20.0 * (1e-12 + real * real + imaginary * imaginary).log10())
                        .round().clamp(0.0, 255.0) as u8;
                    assert_eq!(waterfall.mag_at(block as i32, time_sub, frequency_sub, bin, 0, 0), expected,
                        "block={block}, time_sub={time_sub}, frequency={frequency}");
                }
            }
        }
    }

    #[test]
    fn ft8_frequency_search_includes_the_last_complete_tone_set() {
        let tones: Vec<usize> = transmitter_fixture_values("tones").into_iter().map(usize::from).collect();
        let num_blocks = 90;
        let num_bins = 64;
        let bin = num_bins - 8;
        let mut waterfall = Waterfall {
            num_blocks, num_bins,
            mag: vec![120; num_blocks * TIME_OSR * FREQ_OSR * num_bins],
        };
        for (symbol, tone) in tones.iter().enumerate() {
            waterfall.mag[(symbol * TIME_OSR * FREQ_OSR) * num_bins + bin + tone] = 144;
        }
        assert!(waterfall.decode_candidate(0, 0, 0, bin).is_some());
        assert!(waterfall.find_candidates().iter().any(|&(_, to, ts, fsb, fo)|
            (to, ts, fsb, fo) == (0, 0, 0, bin)), "all eight tones fit at the upper search edge");
    }

    #[test]
    fn incomplete_sync_only_tails_cannot_exhaust_the_candidate_budget() {
        // A complete, unambiguous protocol word at an ordinary sync contrast.
        // The independently encoded tones are the committed transmitter fixture.
        let tones: Vec<usize> = transmitter_fixture_values("tones").into_iter().map(usize::from).collect();
        let num_blocks = 100;
        let num_bins = 160;
        let mut waterfall = Waterfall {
            num_blocks, num_bins,
            mag: vec![120; num_blocks * TIME_OSR * FREQ_OSR * num_bins],
        };
        let (start, bin) = (5, 20);
        for (symbol, tone) in tones.iter().enumerate() {
            waterfall.mag[((start + symbol) * TIME_OSR * FREQ_OSR) * num_bins + bin + tone] = 144;
        }
        assert!(waterfall.decode_candidate(start as i32, 0, 0, bin).is_some());
        // One final noisy FFT can create more than MAX_CANDIDATES high average
        // sync contrasts, even though those hypotheses carry no data at all.
        for time_sub in 0..TIME_OSR {
            for freq_sub in 0..FREQ_OSR {
                for noise_bin in (1..num_bins).step_by(2) {
                    let index = (((num_blocks - 1) * TIME_OSR + time_sub) * FREQ_OSR + freq_sub)
                        * num_bins + noise_bin;
                    waterfall.mag[index] = 240;
                }
            }
        }
        let candidates = waterfall.find_candidates();
        assert!(candidates.iter().any(|&(_, to, ts, fsb, fo)|
            (to, ts, fsb, fo) == (start as i32, 0, 0, bin)),
            "a decodable word must survive candidates containing only a truncated sync block");
    }

    #[test]
    fn candidate_budget_keeps_messages_missing_only_boundary_sync_symbols() {
        let tones: Vec<usize> = transmitter_fixture_values("tones").into_iter().map(usize::from).collect();
        // Starts remain free to move across the whole receive window. Missing
        // initial/final sync symbols leaves all 174 data bits observable.
        for (start, num_blocks) in [(-4_i32, 80_usize), (50, 122)] {
            let num_bins = 64;
            let mut waterfall = Waterfall {
                num_blocks, num_bins,
                mag: vec![120; num_blocks * TIME_OSR * FREQ_OSR * num_bins],
            };
            for (symbol, tone) in tones.iter().enumerate() {
                let block = start + symbol as i32;
                if block >= 0 && (block as usize) < num_blocks {
                    waterfall.mag[(block as usize * TIME_OSR * FREQ_OSR) * num_bins + 20 + tone] = 144;
                }
            }
            assert!(waterfall.decode_candidate(start, 0, 0, 20).is_some());
            assert!(waterfall.find_candidates().iter().any(|&(_, to, ts, fsb, fo)|
                (to, ts, fsb, fo) == (start, 0, 0, 20)), "start={start}");
        }
    }

    // Field vectors independently transcribed from ft8_lib's message.c pack28/unpack28
    // protocol mapping: https://github.com/kgoba/ft8_lib/blob/master/ft8/message.c.
    // 10_214_965 is the mixed-radix token for " K1ABC"; CQ letters use space+A..Z.
    fn protocol_payload(to: u32, from: u32, suffix_to: bool, suffix_from: bool, kind: u8) -> [u8; 77] {
        let mut payload = [0; 77];
        for (offset, word) in [(0, to * 2 + u32::from(suffix_to)),
                               (29, from * 2 + u32::from(suffix_from))] {
            for bit in 0..29 {
                payload[offset + bit] = ((word >> (28 - bit)) & 1) as u8;
            }
        }
        // Field 32401 means no locator/report. The R bit is independent of suffix bits.
        for bit in 0..15 { payload[59 + bit] = ((32_401_u16 >> (14 - bit)) & 1) as u8; }
        payload[74] = kind >> 2;
        payload[75] = (kind >> 1) & 1;
        payload[76] = kind & 1;
        payload
    }

    #[test]
    fn protocol_message_vectors_preserve_cq_modifiers_and_reject_reserved_tokens() {
        for (token, text) in [
            (0, "DE"), (1, "QRZ"), (2, "CQ"), (3, "CQ 000"), (1_002, "CQ 999"),
            (1_004, "CQ A"), (1_135, "CQ DX"), (22_229, "CQ ABCD"), (532_443, "CQ ZZZZ"),
        ] {
            let payload = protocol_payload(token, 10_214_965, false, false, 1);
            assert_eq!(unpack_message(&payload), Some(format!("{text} K1ABC")), "token {token}");
        }
        for token in [532_444, NTOKENS - 1] {
            for (to, from) in [(token, 10_214_965), (2, token)] {
                assert_eq!(unpack_message(&protocol_payload(to, from, false, false, 1)), None,
                    "reserved token {token}");
            }
        }
    }

    #[test]
    fn protocol_message_vectors_preserve_each_callsign_suffix() {
        for (kind, suffix) in [(1, "/R"), (2, "/P")] {
            for (suffix_to, suffix_from) in [(false, false), (true, false), (false, true), (true, true)] {
                let payload = protocol_payload(10_214_965, 10_214_965, suffix_to, suffix_from, kind);
                let expected = format!("K1ABC{} K1ABC{}",
                    if suffix_to { suffix } else { "" }, if suffix_from { suffix } else { "" });
                assert_eq!(unpack_message(&payload), Some(expected));
            }
        }
    }

    #[test]
    fn protocol_message_vectors_restore_special_callsign_prefixes() {
        // FT8 packs 3DA0XYZ as 3D0XYZ and 3XA0XYZ as QA0XYZ to fit the standard field.
        for (token, text) in [(37_178_403, "3DA0XYZ"), (199_563_153, "3XA0XYZ")] {
            let payload = protocol_payload(2, token, false, false, 1);
            assert_eq!(unpack_message(&payload), Some(format!("CQ {text}")));
        }
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

    /// Lifetime decoded-message count (diagnostics/status), preserved across stream resets.
    pub fn decoded_messages(&self) -> u64 {
        self.decoded_messages
    }

    /// The last message from the most recent process call, with its measured timing and frequency.
    pub fn last(&self) -> Option<&Ft8Message> {
        self.decoded.last()
    }

    /// Complex samples currently buffered towards the next decode attempt.
    pub fn buffered(&self) -> usize {
        self.decoder.buffered()
    }

    fn record(&mut self, messages: Vec<Ft8Message>) -> Vec<String> {
        self.decoded_messages += messages.len() as u64;
        self.decoded = messages;
        self.decoded.iter().map(|m| m.text.clone()).collect()
    }
}

impl DigitalDemodulator for Ft8Plugin {
    fn id(&self) -> &'static str {
        "ft8"
    }

    fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
        self.decoded.clear();
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

    fn prior_message(plugin: &mut Ft8Plugin) {
        plugin.record(vec![Ft8Message {
            text: "CQ JO1WKO PM95".to_string(),
            frequency_hz: 1_000.0,
            time_offset_s: 0.5,
            snr_db: 12.0,
            snr: SnrEstimate::default(),
        }]);
        assert_eq!(plugin.decoded()[0].0, "CQ JO1WKO PM95");
    }

    #[test]
    fn accumulating_process_clears_previous_messages() {
        let mut plugin = Ft8Plugin::new(48_000.0);
        for block in [&[][..], &[0.0, 0.0][..]] {
            prior_message(&mut plugin);
            assert!(plugin.process_iq(block).is_empty());
            assert!(plugin.decoded().is_empty());
            assert!(plugin.last().is_none());
            assert!(plugin.last_report().is_none());
        }
        assert_eq!(plugin.decoded_messages(), 2);
    }

    #[test]
    fn quiet_decode_attempts_clear_previous_messages() {
        if cfg!(debug_assertions) {
            return; // Full waterfall searches require a release build.
        }
        let mut plugin = Ft8Plugin::new(6_000.0);
        let first_slot = plugin.decoder.slot_samples();
        let remaining = plugin.decoder.window_samples() - first_slot;
        for samples in [first_slot, remaining] {
            prior_message(&mut plugin);
            assert!(plugin.process_iq(&vec![0.0; samples * 2]).is_empty());
            assert!(plugin.decoded().is_empty());
            assert!(plugin.last().is_none());
            assert!(plugin.last_report().is_none());
        }
        assert_eq!(plugin.decoded_messages(), 2);
    }

    #[test]
    fn reset_clears_stream_results_but_preserves_lifetime_count() {
        let mut plugin = Ft8Plugin::new(48_000.0);
        plugin.process_iq(&[1.0, -1.0]);
        prior_message(&mut plugin);
        plugin.reset();
        assert!(plugin.decoded().is_empty());
        assert!(plugin.last().is_none());
        assert_eq!(plugin.buffered(), 0);
        assert_eq!(plugin.decoded_messages(), 1);
        prior_message(&mut plugin);
        assert_eq!(plugin.decoded_messages(), 2);
    }

    const IQ_BYTES: &[u8] = include_bytes!("../../../../../tests/fixtures/ft8/ft8_cq_iq.bin");

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
    fn the_plugin_decodes_torn_fixture_blocks_without_repeating_reports() {
        if cfg!(debug_assertions) {
            return; // a whole-slot waterfall decode is release-only (see the reference test)
        }
        let mut plugin = Ft8Plugin::new(48_000.0);
        // Cover the early attempt and the full-window re-search of the same burst.
        let mut slot = fixture_slot();
        slot.resize(Ft8Decoder::new(48_000.0).window_samples() * 2, 0.0);
        let mut decoded: Vec<String> = Vec::new();
        for block in slot.chunks(960 * 2) {
            let mut torn_block = block.to_vec();
            torn_block.push(f32::NAN); // An unmatched I must not become the next block's Q.
            let messages = plugin.process_iq(&torn_block);
            let reports = plugin.decoded();
            assert_eq!(reports.iter().map(|(text, _)| text).collect::<Vec<_>>(), messages.iter().collect::<Vec<_>>());
            decoded.extend(messages);
            for (_, report) in reports {
                assert!((report.frequency_hz - 1_000.0).abs() < 25.0);
            }
        }
        assert_eq!(decoded, vec!["CQ JO1WKO PM95".to_string()]);
        assert_eq!(plugin.decoded_messages(), 1);
        assert!(plugin.decoded().is_empty(), "the final window only re-searches an old burst");
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

#[cfg(test)]
mod snr_measurement_tests {
    use super::*;
    struct Noise(u64);
    impl Noise {
        fn unit(&mut self)->f64 {self.0^=self.0<<13;self.0^=self.0>>7;self.0^=self.0<<17;((self.0>>11) as f64+0.5)/9007199254740992.0}
        fn normal(&mut self)->f64 {(-2.0*self.unit().ln()).sqrt()*(2.0*PI*self.unit()).cos()}
    }
    #[test]
    fn matched_filter_snr_calibration_in_2500hz_with_fading_and_frequency_offsets() {
        let tones=tones_from_codeword(&[0;174]);let mut worst=0.0_f64;
        for rate in [8000.0,12_000.0,48_000.0] {
            let sym=symbol_samples_at(rate);
            for expected in [-24.0,-20.0,-10.0,0.0,9.0] {
                let mut noise=Noise(5678);let mut iq=vec![0.0_f32;2*sym*NUM_SYMBOLS];
                let phases=symbol_phases(&tones,1234.57,rate);
                let sigma=(0.002_f64.powi(2)/10_f64.powf(expected/10.0)*rate/2500.0/2.0).sqrt();
                // Fading keeps mean signal power exactly A²; not a constant-gain
                // assumption and not a denoised-output reference.
                let fades:Vec<_>=(0..NUM_SYMBOLS).map(|k|0.6+0.4*(k as f64*0.4).cos()).collect();
                let norm=(fades.iter().map(|x|x*x).sum::<f64>()/NUM_SYMBOLS as f64).sqrt();
                for (k,&tone) in tones.iter().enumerate() {
                    for i in 0..sym {
                        let phase=phases[k]+2.0*PI*(1234.57+tone as f64*TONE_SPACING_HZ)*i as f64/rate;
                        let a=0.002*fades[k]/norm;
                        iq[2*(k*sym+i)]=(a*phase.cos()+sigma*noise.normal()) as f32;
                        iq[2*(k*sym+i)+1]=(a*phase.sin()+sigma*noise.normal()) as f32;
                    }
                }
                let model=CancellationModel{frequency:1234.57,start:0,gains:[(0.0,0.0);NUM_SYMBOLS]};
                let measured=estimate_snr(&iq,rate,&tones,&model);
                let value=measured.value.unwrap_or_else(||panic!("{rate}/{expected}: {measured:?}"));
                let error=(value-expected).abs();worst=worst.max(error);
                println!("FT8 Fs={rate} B=2500 expected={expected} measured={value:.3} error={error:.3} dB");
                assert!(error<if expected< -20.0 {3.0}else{1.5},"{measured:?}");
            }
        }
        println!("FT8 worst error: {worst:.4} dB");
    }
    #[test]
    fn unresolved_or_truncated_symbols_do_not_report_zero_db() {
        let tones=tones_from_codeword(&[0;174]);let model=CancellationModel{frequency:1000.0,start:0,gains:[(0.0,0.0);NUM_SYMBOLS]};
        assert_eq!(estimate_snr(&vec![0.0;24000],12000.0,&tones,&model).value,None);
        assert_eq!(estimate_snr(&vec![0.0;320000],12000.0,&tones,&model).value,None);
        let constant=estimate_snr(&vec![1.0;320000],12000.0,&tones,&model);
        assert_eq!(constant.value,None);
        assert_eq!(constant.reason,Some("noise_not_resolved"));
        assert_eq!(constant.quality,"unavailable");
    }
}
