//! FT8: the first digital demodulator plugin.
//!
//! FT8 is an 8-FSK mode: 79 symbols of 0.160 s, tones spaced 6.25 Hz, with three 7-symbol Costas
//! sync blocks at 0, 36 and 72. The 58 data symbols carry 174 bits, which are an LDPC(174,91)
//! codeword: 91 payload bits (77 of message plus a 14-bit CRC) and 83 parity bits.
//!
//! Decoding is the reverse, and follows the reference decoders (ft8_lib, WSJT-X): build a
//! *waterfall* (a Hann-windowed FFT per symbol), score candidates by a *local-contrast* sync metric
//! over the Costas tones, rank them with a heap, and for each run the max-log likelihood -> LDPC ->
//! CRC -> unpack. The search covers the *whole slot* (not just the first 2.4 s), because a real
//! capture is not slot-aligned: the transmission can start seconds into the buffer, and the tail is
//! then truncated, which the sync/LLR stages tolerate by skipping out-of-range symbols.
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
/// loses ~4 dB), two is what the reference uses and is enough; four buys nothing measurable.
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

    /// Decode one candidate: likelihood -> LDPC -> CRC, returning the 77 payload bits.
    fn decode_candidate(&self, to: i32, ts: usize, fsb: usize, fo: usize) -> Option<[u8; 77]> {
        let llr = self.extract_likelihood(to, ts, fsb, fo);
        let bits = ldpc_decode(&llr, MAX_LDPC_ITERATIONS)?;
        if !crc_ok(&bits) {
            return None;
        }
        Some(core::array::from_fn(|i| bits[i]))
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

    /// Decode every message in the buffered slot. The search covers the whole slot, so this
    /// returns all CRC-passing transmissions (there can be more than one on a busy band).
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
        let waterfall = Waterfall::build(&self.samples, self.rate);
        let mut out: Vec<Ft8Message> = Vec::new();
        for (score, to, ts, fsb, fo) in waterfall.find_candidates() {
            let Some(payload) = waterfall.decode_candidate(to, ts, fsb, fo) else {
                continue;
            };
            let Some(text) = unpack_message(&payload) else {
                continue;
            };
            // The Costas pattern repeats every 36 symbols, so one transmission also scores (and
            // sometimes decodes) at ±36 offsets; dedupe by text.
            if out.iter().any(|m: &Ft8Message| m.text == text) {
                continue;
            }
            out.push(Ft8Message {
                text,
                frequency_hz: F_MIN_HZ + (fo as f64 + fsb as f64 / FREQ_OSR as f64) * TONE_SPACING_HZ,
                time_offset_s: (to as f64 + ts as f64 / TIME_OSR as f64) * SYMBOL_PERIOD_S,
                snr_db: score * 0.5,
            });
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

#[cfg(test)]
fn is_sync_symbol(index: usize) -> bool {
    index < SYNC_LENGTH
        || (SYNC_OFFSET..SYNC_OFFSET + SYNC_LENGTH).contains(&index)
        || (2 * SYNC_OFFSET..2 * SYNC_OFFSET + SYNC_LENGTH).contains(&index)
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
/// window boundaries happen to fall relative to the 15 s FT8 schedule.
pub struct Ft8Plugin {
    decoder: Ft8Decoder,
    decoded: Vec<Ft8Message>,
    decoded_messages: u64,
}

impl Ft8Plugin {
    pub fn new(rate: f64) -> Self {
        Self { decoder: Ft8Decoder::new(rate), decoded: Vec::new(), decoded_messages: 0 }
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
}

impl DigitalDemodulator for Ft8Plugin {
    fn id(&self) -> &'static str {
        "ft8"
    }

    fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
        self.decoder.push_iq(iq);
        if self.decoder.buffered() < self.decoder.window_samples() {
            return Vec::new();
        }
        let messages = self.decoder.decode();
        // Slide one hop instead of clearing. The next window then overlaps this one by a whole
        // transmission, so a burst sitting at the seam is still searched whole in whichever window
        // contains it -- `window >= hop + burst` is what makes that a guarantee.
        self.decoder.advance(self.decoder.slot_samples());
        if messages.is_empty() {
            return Vec::new();
        }
        self.decoded_messages += messages.len() as u64;
        self.decoded = messages.clone();
        messages.into_iter().map(|m| m.text).collect()
    }

    fn reset(&mut self) {
        self.decoder.reset();
        self.decoded.clear();
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
}
