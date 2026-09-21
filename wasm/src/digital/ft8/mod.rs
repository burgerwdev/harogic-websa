//! FT8: the first digital demodulator plugin.
//!
//! FT8 is an 8-FSK mode: 79 symbols of 0.160 s, tones spaced 6.25 Hz, with three 7-symbol Costas
//! sync blocks at 0, 36 and 72. The 58 data symbols carry 174 bits, which are an LDPC(174,91)
//! codeword: 91 payload bits (77 of message plus a 14-bit CRC) and 83 parity bits.
//!
//! Decoding is the reverse: find the signal (Costas correlation over time/frequency), measure the
//! eight tone energies per symbol, turn them into log-likelihood ratios, run the LDPC sum-product
//! decoder, check the CRC, and unpack the message.
//!
//! This module is on the **digital path**: it reads the RAW complex baseband and never touches the
//! audio chain (`pipeline` cannot hand it an `AnalogPcm`). A voice denoiser on an FT8 tone would
//! destroy the information this decoder needs.
//!
//! The protocol tables are ported from ft8_lib (MIT) — see `tables.rs` and the provenance table in
//! `docs/*/ARCHITECTURE.md`. The search below is deliberately narrow (it expects the signal inside
//! the current slot, which is what a slot-driven receiver has): it is a real decoder, not a
//! wideband skimmer, and the limitation is recorded rather than hidden.

pub mod tables;

use tables::*;

/// Frequency refinement: 8 steps of 1/8 tone spacing each, so +/- one coarse step (+/- 6.25 Hz) is
/// covered at a resolution of 0.78 Hz.
///
/// The resolution is what matters, not the range. FT8 tones are 6.25 Hz apart and the energies are
/// measured over one symbol window, so an estimate half a tone away (the old step of TONE_SPACING/2 =
/// 3.125 Hz, which is exactly half) sits between two tones: each tone's energy leaks into its
/// neighbour, the soft decisions come out wrong, and the LDPC never converges. That is invisible in
/// the sync correlation - which only measures the seven Costas symbols - and it is why a weak signal
/// that a phone decodes was found by the search and then lost. The reference refinements are finer
/// than half a tone for this reason.
const FREQ_OFFSET_STEPS: i32 = 8;
const FREQ_OFFSET_STEP_HZ: f64 = TONE_SPACING_HZ / 8.0;
/// Time search: +/- 8 steps of 1/5 symbol each = +/- 0.256 s. FT8 transmissions are slot-aligned
/// (operators are clock-disciplined), so the receiver searches the slot edge rather than the whole
/// slot: a full-slot scan would cost ~15x more for a case a slot-driven receiver does not have.
const TIME_OFFSET_STEPS: i32 = 8;
const MAX_LDPC_ITERATIONS: usize = 40;
/// A tone must stand this far above the symbol's mean tone energy to be trusted for the search.
const SYNC_MARGIN: f64 = 1.0;

/// One decoded message, with what the decoder measured about the signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Ft8Message {
    pub text: String,
    /// Audio frequency of tone 0, in Hz.
    pub frequency_hz: f64,
    /// Start of the transmission relative to the slot buffer, in seconds.
    pub time_offset_s: f64,
    /// Rough SNR from the sync correlation (dB); a diagnostic, not a calibrated measurement.
    pub snr_db: f64,
}

/// One slot's worth of complex baseband, kept as interleaved f32.
pub struct Ft8Decoder {
    rate: f64,
    samples: Vec<f32>,
    /// Slot length in complex samples (one FT8 slot at this rate).
    slot_samples: usize,
}

impl Ft8Decoder {
    pub fn new(rate: f64) -> Self {
        let slot_samples = (rate * SLOT_SECONDS) as usize;
        Self { rate, samples: Vec::new(), slot_samples }
    }

    pub fn push_iq(&mut self, iq: &[f32]) {
        self.samples.extend_from_slice(iq);
        // Keep at most one slot (plus the first symbol, so a slot boundary is never half-missing):
        // FT8 decodes per slot, and an unbounded buffer would grow without limit in a long session.
        let keep = self.slot_samples + symbol_samples_at(self.rate);
        if self.samples.len() > keep * 2 {
            let drop = self.samples.len() - keep;
            self.samples.drain(..drop);
        }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Buffered complex samples (diagnostics).
    pub fn buffered(&self) -> usize {
        self.samples.len() / 2
    }

    /// Complex samples in one FT8 slot at this rate (15 s: the slot the mode is scheduled on).
    pub fn slot_samples(&self) -> usize {
        self.slot_samples
    }

    /// Complex samples in one FT8 *transmission* (79 symbols = 12.64 s).
    pub fn transmission_samples(&self) -> usize {
        NUM_SYMBOLS * symbol_samples_at(self.rate)
    }

    /// Drop the oldest `samples` complex samples (the window advances after a failed attempt).
    pub fn discard_oldest(&mut self, samples: usize) {
        let drop = (samples * 2).min(self.samples.len());
        self.samples.drain(..drop);
    }

    /// Try to decode the buffered slot. `None` when nothing passes the CRC (the common case: most
    /// slots contain no signal, and CRC is what stops noise from being reported as a message).
    pub fn decode(&mut self) -> Option<Ft8Message> {
        let n = self.samples.len() / 2;
        let symbol_samples = symbol_samples_at(self.rate);
        if n < NUM_SYMBOLS * symbol_samples {
            return None;
        }

        // The band's strongest sync candidates, best first: the coarse scan is cheap, and the
        // expensive stages (fine search, LDPC, CRC) only run on the few that could be a transmission.
        for (candidate_time, candidate_hz) in self.candidates(symbol_samples) {
            let Some((best_time, best_freq, snr)) =
                self.find_sync_at(symbol_samples, candidate_hz, candidate_time)
            else {
                continue;
            };
            let energies = self.tone_energies(best_time, best_freq, symbol_samples, candidate_hz);
            let llr = symbols_to_llr(&energies);
            let Some(bits) = ldpc_decode(&llr, MAX_LDPC_ITERATIONS) else {
                continue;
            };
            if !crc_ok(&bits) {
                continue;
            }
            let payload: [u8; 77] = core::array::from_fn(|i| bits[i]);
            let Some(text) = unpack_message(&payload) else {
                continue;
            };
            return Some(Ft8Message {
                text,
                frequency_hz: candidate_hz + best_freq,
                time_offset_s: best_time as f64 / self.rate,
                snr_db: snr,
            });
        }
        None
    }

    /// Debug: every coarse candidate and what the fine search made of it, as
    /// `(frequency_hz, correlation, start_seconds)`. Not part of the product ABI; it exists so a
    /// known-good signal (see `tests/ft8_real_slot.rs`) can be asked *where* the decoder lost it.
    pub fn debug_sync(&self) -> Vec<(f64, f64, f64)> {
        let symbol_samples = symbol_samples_at(self.rate);
        self.candidates(symbol_samples)
            .into_iter()
            .filter_map(|(time, hz)| {
                self.find_sync_at(symbol_samples, hz, time).map(|(best_time, offset, correlation)| {
                    (hz + offset, correlation, best_time as f64 / self.rate)
                })
            })
            .collect()
    }

    /// The band's strongest Costas candidates over the buffered slot, as `(time, base frequency)`.
    ///
    /// A decimated correlation (every 8th sample: the tones survive, the noise aliases) swept over the
    /// slot and over the band. One symbol of time resolution is enough because the fine search covers
    /// +/-0.128 s around whatever this picks.
    fn candidates(&self, symbol_samples: usize) -> Vec<(usize, f64)> {
        let total = self.samples.len() / 2;
        let span = NUM_SYMBOLS * symbol_samples;
        if total < span {
            return Vec::new();
        }
        let limit = total - span;
        let time_step = symbol_samples.max(1);
        let mut scored: Vec<(f64, usize, f64)> = Vec::new();
        let mut base = BAND_LOW_HZ;
        while base <= BAND_HIGH_HZ {
            let mut time = 0;
            while time <= limit {
                let mut score = 0.0;
                for (symbol_index, expected) in COSTAS.iter().enumerate() {
                    let start = time + symbol_index * symbol_samples;
                    let frequency = base + *expected as f64 * TONE_SPACING_HZ;
                    score += self.tone_energy_decimated(start, symbol_samples, frequency, 8);
                }
                scored.push((score, time, base));
                time += time_step;
            }
            base += BAND_STEP_HZ;
        }
        if scored.is_empty() {
            return Vec::new();
        }
        let mean = scored.iter().map(|entry| entry.0).sum::<f64>() / scored.len() as f64;
        // Candidates are the *local maxima* along the frequency axis at each time step, not the global
        // top few. A transmission is a narrow peak in the band, so it is a local maximum whether or not
        // it is among the strongest things on the air - and on a busy band it is not: a signal the phone
        // decoded at -73 dB sat in a band with several stronger traces, and a global top-8 (the old
        // rule) kept only those stronger ones, so the weak signal was never handed to the fine search.
        // This is the ranking the reference decoders use; the cap is only to bound the work.
        const BASE_INDEX: f64 = BAND_STEP_HZ;
        let mut local: Vec<(f64, usize, f64)> = scored
            .iter()
            .filter(|(score, time, base)| {
                let neighbours = scored.iter().filter(|(_, t, _)| t == time).filter(|(_, _, b)| {
                    (b - base).abs() <= BASE_INDEX + 1e-9 && (*b - *base).abs() > 1e-9
                });
                neighbours.into_iter().all(|(other, _, _)| other < score)
            })
            .cloned()
            .collect();
        if local.is_empty() {
            local = scored.clone();
        }
        local.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(core::cmp::Ordering::Equal));
        local.truncate(MAX_CANDIDATES);
        // An empty slot is rejected here, in the cheap stage, instead of in the expensive one; a real
        // transmission's candidate is above the average of the whole scan.
        let floor = mean * CANDIDATE_MARGIN;
        local
            .into_iter()
            .filter(|(score, _, _)| *score > floor)
            .map(|(_, time, hz)| (time, hz))
            .collect()
    }

    /// Costas correlation over (time, frequency) — the signal has to be found before it is read.
    ///
    /// Two stages: a cheap decimated sweep locates the burst anywhere in the buffered window (the
    /// worker has no slot clock), then this fine search covers the slot edge (+/- 0.128 s) around
    /// that location together with the frequency offsets. FT8 transmissions are slot-synchronised,
    /// so once the burst is found the fine window is exactly the jitter a receiver must tolerate.
    fn find_sync_at(
        &self,
        symbol_samples: usize,
        base_hz: f64,
        coarse: usize,
    ) -> Option<(usize, f64, f64)> {
        let mut best: Option<(usize, f64, f64)> = None;
        let step = FREQ_OFFSET_STEP_HZ;
        let time_step = symbol_samples / (TIME_OFFSET_STEPS as usize + 1);
        // A candidate is only usable when a whole transmission fits after it: the symbol loop below
        // `break`s on a short window, and a base that never got that far would still be recorded (its
        // correlation is 0, which beats an empty `best`) - the reader then indexed past the buffer and
        // the decoder worker died silently on the first real band (measured: `dsp_pushes` frozen with
        // its buffer left 65 samples short of a slot). Skipping unusable candidates is also the honest
        // answer: there is no transmission there to find.
        let span = NUM_SYMBOLS * symbol_samples;
        let total = self.samples.len() / 2;
        for time_index in -(TIME_OFFSET_STEPS as i64)..=(TIME_OFFSET_STEPS as i64) {
            let base = (coarse as i64 + time_index * time_step as i64).max(0) as usize;
            if base + span > total {
                continue;
            }
            for freq_index in -FREQ_OFFSET_STEPS..=FREQ_OFFSET_STEPS {
                let offset = freq_index as f64 * step;
                let mut sync = 0.0;
                let mut total = 0.0;
                for (symbol_index, expected) in COSTAS.iter().enumerate() {
                    let start = base + symbol_index * symbol_samples;
                    if start + symbol_samples > self.samples.len() / 2 {
                        break;
                    }
                    let mut energies = [0.0_f64; 8];
                    for (tone, energy) in energies.iter_mut().enumerate() {
                        let frequency = base_hz + offset + tone as f64 * TONE_SPACING_HZ;
                        // Decimated 4x, and only for *scoring*: this search ranks (time, frequency)
                        // offsets, while the tone energies that feed the LLRs are read at full rate
                        // afterwards (`tone_energies`). Full rate here cost ~25 s per slot with the
                        // finer frequency grid below, which does not fit a 15 s slot; a 4x-decimated
                        // correlation still ranks correctly.
                        *energy = self.tone_energy_decimated(start, symbol_samples, frequency, 4);
                    }
                    let sum: f64 = energies.iter().sum();
                    sync += energies[*expected as usize];
                    total += sum;
                    if total <= 0.0 {
                        break;
                    }
                }
                let correlation = if total > 0.0 { sync / total } else { 0.0 };
                let better = match best {
                    None => true,
                    Some((_, _, best_correlation)) => correlation > best_correlation,
                };
                if better {
                    best = Some((base, offset, correlation));
                }
            }
        }
        let (base, offset, correlation) = best?;
        // A Costas block is 7 of 56 tones: a signal puts ~1/3 of the energy in the 7 expected tones,
        // noise puts ~1/8. Requiring more than the chance level keeps pure noise from reaching the
        // (expensive) LDPC stage.
        if correlation < SYNC_MARGIN / 8.0 {
            return None;
        }
        Some((base, offset, 10.0 * (correlation / (1.0 - correlation).max(1e-9)).log10()))
    }

    /// Magnitude² of one tone over one symbol window, sampled every `decimate` samples.
    ///
    /// The coarse pass only ranks offsets, so it can afford this: an 8x decimated DFT is ~8x
    /// cheaper and still peaks at the right alignment.
    fn tone_energy_decimated(&self, start: usize, length: usize, frequency: f64, decimate: usize) -> f64 {
        self.tone_energy_rotating(start, length, frequency, decimate)
    }

    /// Magnitude² of one tone over one symbol window.
    ///
    /// The local oscillator is a recursive rotator: the frequency is fixed for the call, so the
    /// per-sample twiddle is one complex multiply instead of a `sin_cos()` pair. Those transcendentals
    /// were the entire cost of the search - a slot decode took 9.3 s through the shipped artifact on a
    /// busy band (against 0.6 s natively), which is long enough that the decoder fell behind the live
    /// stream. The rotation is exact to f64 rounding over one symbol window.
    fn tone_energy(&self, start: usize, length: usize, frequency: f64) -> f64 {
        self.tone_energy_rotating(start, length, frequency, 1)
    }

    /// `tone_energy` with an optional decimation of the *summed* samples (the search's cheap stage).
    fn tone_energy_rotating(
        &self,
        start: usize,
        length: usize,
        frequency: f64,
        decimate: usize,
    ) -> f64 {
        let step = -2.0 * core::f64::consts::PI * frequency / self.rate * decimate.max(1) as f64;
        let (step_sin, step_cos) = step.sin_cos();
        let (mut rot_re, mut rot_im) = (1.0_f64, 0.0_f64);
        let (mut re, mut im) = (0.0_f64, 0.0_f64);
        let mut k = 0;
        while k < length {
            let index = (start + k) * 2;
            if index + 1 >= self.samples.len() {
                break;
            }
            let (i, q) = (self.samples[index] as f64, self.samples[index + 1] as f64);
            re += i * rot_re - q * rot_im;
            im += i * rot_im + q * rot_re;
            let next_re = rot_re * step_cos - rot_im * step_sin;
            let next_im = rot_re * step_sin + rot_im * step_cos;
            rot_re = next_re;
            rot_im = next_im;
            k += decimate.max(1);
        }
        re * re + im * im
    }

    /// The eight tone energies of every symbol, at the located (time, frequency).
    fn tone_energies(
        &self,
        base: usize,
        offset: f64,
        symbol_samples: usize,
        base_hz: f64,
    ) -> Vec<[f64; 8]> {
        let mut out = Vec::with_capacity(NUM_SYMBOLS);
        for symbol_index in 0..NUM_SYMBOLS {
            let start = base + symbol_index * symbol_samples;
            let mut energies = [0.0_f64; 8];
            for (tone, energy) in energies.iter_mut().enumerate() {
                let frequency = base_hz + offset + tone as f64 * TONE_SPACING_HZ;
                *energy = self.tone_energy(start, symbol_samples, frequency);
            }
            out.push(energies);
        }
        out
    }
}

/// Symbol window in samples at `rate`.
fn symbol_samples_at(rate: f64) -> usize {
    (rate * SYMBOL_PERIOD_S) as usize
}

/// The audio band FT8 occupies, in Hz.
///
/// A receiver that searched only around one nominal frequency (1 kHz) decoded the committed fixture -
/// which happens to sit exactly there - and nothing on a real band: reported from the bench after a
/// phone app decoded a transmission this decoder could not see. The tones are anywhere in this band.
const BAND_LOW_HZ: f64 = 200.0;
const BAND_HIGH_HZ: f64 = 3_000.0;
/// Spacing of the coarse candidates. The fine search covers +/-25 Hz around each, so 50 Hz leaves no
/// gap in the band.
const BAND_STEP_HZ: f64 = 12.5;
/// Candidates that go on to the fine search and the CRC. Each costs a full sync search, and only one
/// of them can be the transmission (a 24-bit CRC decides), so this is a cost/false-positive trade.
/// A bound on the work, not a ranking rule: the fine search is linear in this, and the candidates are
/// the band's local maxima (a busy band has many). The fine search is decimated, so this stays inside
/// a slot's budget.
const MAX_CANDIDATES: usize = 12;
/// A candidate must beat the whole scan's average score by this factor.
///
/// Most slots carry nothing, and without a gate every empty slot paid six full sync searches plus six
/// LDPC attempts (measured: a noise slot took seconds). A transmission's Costas block lifts its
/// candidate well above the band's average; noise does not lift any of them.
const CANDIDATE_MARGIN: f64 = 1.1;
/// LLRs for the 174 data bits from the per-symbol tone energies.
///
/// The sign convention is the reference's (`decode.c`: "log likelihood log(p(1)/p(0))"), and the
/// LDPC stage's hard decision is `total > 0 ? 1 : 0`, so a positive value must favour bit 1. The
/// first version returned log(p0/p1) — inverted — which showed up as 24 failing parity checks and a
/// decoder that never converged, on a signal whose tones were detected perfectly.
fn symbols_to_llr(energies: &[[f64; 8]]) -> [f64; 174] {
    let mut llr = [0.0_f64; 174];
    let mut data_index = 0;
    for symbol_index in 0..NUM_SYMBOLS {
        if is_sync_symbol(symbol_index) {
            continue;
        }
        let symbol = &energies[symbol_index];
        for bit_index in 0..3 {
            let mut zero = 0.0;
            let mut one = 0.0;
            for (pattern, tone) in GRAY.iter().enumerate() {
                let bit = (pattern >> (2 - bit_index)) & 1;
                if bit == 0 {
                    zero += symbol[*tone as usize];
                } else {
                    one += symbol[*tone as usize];
                }
            }
            llr[data_index] = ((one + 1e-12) / (zero + 1e-12)).ln();
            data_index += 1;
        }
    }
    // The reference scales the soft information and clips it (`ft8_lib`'s `ft8_decode`: `llr[i] *= 2.83f`
    // with a clamp). It matters for weak signals: the log-ratio of two noise-like tone energies is a
    // small number, and a small number leaves the sum-product iterations nearly indifferent, so they do
    // not converge on a signal the phone decodes (measured: this real slot's Costas correlation is 0.193
    // against a chance level of 0.125). The scale changes nothing about the sign - the hard decision -
    // only how much the iterations are willing to move.
    for value in llr.iter_mut() {
        *value = (*value * 2.83).clamp(-20.0, 20.0);
    }
    llr
}

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

    #[test]
    fn a_noise_only_slot_decodes_to_nothing() {
        // CRC is the gate: a buffer with no signal must not produce a message.
        let mut decoder = Ft8Decoder::new(48_000.0);
        let mut seed = 12345_u32;
        let mut noise = Vec::with_capacity(48_000 * 2);
        for _ in 0..48_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            noise.push(((seed >> 8) as f64 / 16_777_216.0 - 0.5) as f32 * 0.01);
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            noise.push(((seed >> 8) as f64 / 16_777_216.0 - 0.5) as f32 * 0.01);
        }
        decoder.push_iq(&noise);
        assert!(decoder.decode().is_none(), "noise must not decode to a message");
    }
}


// ---------------------------------------------------------------- plugin + ABI

use crate::plugin::{DigitalDemodulator, DigitalReport};

/// FT8 as a `DigitalDemodulator`: it consumes RAW baseband blocks and emits decoded text.
///
/// The decoder is slot-driven (see the module docs): blocks accumulate until a slot is buffered,
/// then one decode attempt runs. A successful decode clears the buffer, because the next slot is a
/// different transmission and re-decoding the same samples would report the same message again.
pub struct Ft8Plugin {
    decoder: Ft8Decoder,
    last: Option<Ft8Message>,
    decoded_messages: u64,
}

impl Ft8Plugin {
    pub fn new(rate: f64) -> Self {
        Self { decoder: Ft8Decoder::new(rate), last: None, decoded_messages: 0 }
    }

    /// How many messages this plugin has decoded (diagnostics/status).
    pub fn decoded_messages(&self) -> u64 {
        self.decoded_messages
    }

    /// The most recent decode, with the timing and frequency the decoder measured.
    pub fn last(&self) -> Option<&Ft8Message> {
        self.last.as_ref()
    }

    /// Complex samples currently buffered towards the next decode attempt.
    ///
    /// Diagnostics with a purpose: "the decoder is running" and "the decoder is 90 % of the way
    /// through a transmission" look identical from the outside otherwise, and a buffer that keeps
    /// restarting is the difference between a broken stream and a quiet band.
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
        // A SLOT, not a transmission: the live stream is a rolling buffer, so decoding a
        // transmission-sized window and then discarding exactly that much would land on the same
        // phase every time and never align with a transmission that starts at a slot boundary
        // (measured in the browser: 1100 blocks of real fixture IQ, `dsp_buffered` stuck around
        // 540k, no decode). Advancing by a whole slot makes the windows slot-aligned, which is what
        // FT8's 15 s schedule is.
        let slot = self.decoder.slot_samples();
        if self.decoder.buffered() < slot {
            return Vec::new();
        }
        match self.decoder.decode() {
            Some(message) => {
                self.decoded_messages += 1;
                let text = message.text.clone();
                self.last = Some(message);
                // The slot has been consumed: the next decode starts from a fresh buffer.
                self.decoder.reset();
                vec![text]
            }
            None => {
                // Nothing there. The window has to *advance*, not just be cleared.
                //
                // One attempt can only examine the positions where a whole transmission fits: with a
                // 15 s buffer and a 12.6 s transmission that is the first ~2.4 s of the window (the
                // candidate scan skips start times without room for a transmission). Clearing the
                // buffer therefore examined the same 2.4 s for ever and a signal anywhere else in the
                // slot was never seen - which is what "waiting for a decode" while a phone app
                // decodes the same audio looked like.
                //
                // Dropping a whole transmission advances the window by `slot - transmission` (about
                // 2.4 s), so a handful of attempts cover every offset in a slot. The remainder is
                // trimmed to just under a slot as well: otherwise a buffer that had grown stayed
                // above the threshold and *every* following block started another attempt, each
                // costing seconds while the stream delivers 120 blocks a second (measured: the
                // decoder worker stopped reporting with its buffer just short of a slot).
                let slot = self.decoder.slot_samples();
                let transmission = self.decoder.transmission_samples();
                let buffered = self.decoder.buffered();
                let keep = slot.saturating_sub(transmission).min(buffered);
                self.decoder.discard_oldest(buffered.saturating_sub(keep));
                Vec::new()
            }
        }
    }

    fn reset(&mut self) {
        self.decoder.reset();
        self.last = None;
    }

    fn buffered_input(&self) -> usize {
        self.buffered()
    }

    fn last_report(&self) -> Option<DigitalReport> {
        self.last.as_ref().map(|message| DigitalReport {
            frequency_hz: message.frequency_hz,
            time_offset_s: message.time_offset_s,
            snr_db: message.snr_db,
        })
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
    ///
    /// The plugin decodes slots (FT8 is a 15 s slot mode and the live stream is a rolling buffer),
    /// so a test that feeds only the 12.64 s transmission would wait forever.
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
        let mut plugin = Ft8Plugin::new(48_000.0);
        // One SLOT of input (the transmission is 12.64 s of a 15 s slot).
        let slot = fixture_slot();
        // Feed it in 20 ms blocks, the way the worker does.
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
        // The live stream is a rolling buffer with no slot clock, so a window generally does NOT
        // start at a transmission - and the browser path decoded nothing until this worked
        // (`dsp_buffered` cycling through slots with `ft8=0`). The decoder has to find the burst.
        // A 12.64 s transmission fits in a 15 s slot only if it starts within the first 2.36 s,
        // which is the range a rolling buffer can land on.
        for offset_seconds in [0.0_f64, 0.4, 0.8, 1.5, 2.3] {
            let slot_len = (48_000.0 * tables::SLOT_SECONDS) as usize * 2;
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
}

#[cfg(test)]
mod zz_probe {
    use super::*;

    /// Diagnostic only (temporary): where does a real 40 m capture fail?
    #[test]
    fn probe_real_capture() {
        let bytes = std::fs::read("/tmp/real_ft8.iq").expect("capture");
        let samples: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let rate = 48_814.77_f64;
        let mut decoder = Ft8Decoder::new(rate);
        // One slot (15 s) from the middle, so the buffer is exactly a slot.
        let slot = (rate * 15.0) as usize;
        let start = (samples.len() / 2).saturating_sub(slot) / 2;
        decoder.push_iq(&samples[start * 2..(start + slot) * 2]);
        let symbol_samples = symbol_samples_at(rate);
        let mean_signal = (decoder.samples.iter().map(|v| (*v as f64).powi(2)).sum::<f64>()
            / decoder.samples.len() as f64)
            .sqrt();
        println!("probe: buffer {} complex, rms {:.5}, symbol {} samples", decoder.buffered(), mean_signal, symbol_samples);
        // A coarse scan, printed with its scores: is the signal's candidate there at all?
        let mut best: Vec<(f64, usize, f64)> = Vec::new();
        let mut base = BAND_LOW_HZ;
        while base <= BAND_HIGH_HZ {
            let mut time = 0;
            let limit = decoder.buffered() - NUM_SYMBOLS * symbol_samples;
            while time <= limit {
                let mut score = 0.0;
                for (symbol_index, expected) in COSTAS.iter().enumerate() {
                    let s0 = time + symbol_index * symbol_samples;
                    score += decoder.tone_energy_decimated(
                        s0, symbol_samples, base + *expected as f64 * TONE_SPACING_HZ, 8);
                }
                best.push((score, time, base));
                time += symbol_samples;
            }
            base += BAND_STEP_HZ;
        }
        let count = best.len() as f64;
        let mean = best.iter().map(|e| e.0).sum::<f64>() / count;
        best.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        println!("probe: {} coarse entries, mean {:.3e}, gate {:.3e}", best.len(), mean, mean * CANDIDATE_MARGIN);
        for (score, time, base) in best.iter().take(14) {
            println!("  candidate {:.1} Hz at {:.3} s: score {:.3e} (x{:.2} mean)", base, *time as f64 / rate, score, score / mean);
        }
        let nearest = best
            .iter()
            .filter(|(_, _, hz)| (*hz - 800.0).abs() < 1.0)
            .map(|(score, time, _)| (score / mean, *time))
            .next();
        println!("probe: the 800 Hz candidate (the signal the phone app decoded at 792 Hz): {:?}", nearest);
        // Fine search on the top candidates: does the correlation pass, and what does the CRC say?
        for (i, (_score, time, base)) in best.iter().take(14).enumerate() {
            let result = decoder.find_sync_at(symbol_samples, *base, *time);
            match result {
                Some((t, f, snr)) => {
                    let energies = decoder.tone_energies(t, f, symbol_samples, *base);
                    let llr = symbols_to_llr(&energies);
                    let ldpc = ldpc_decode(&llr, MAX_LDPC_ITERATIONS);
                    let crc = ldpc.as_ref().map(|b| crc_ok(b)).unwrap_or(false);
                    println!("  fine #{i}: {:.1} Hz @ {:.3} s snr {:.1} -> ldpc {} crc {}", base + f, t as f64 / rate, snr, ldpc.is_some(), crc);
                }
                None => println!("  fine #{i}: {:.1} Hz @ {:.3} s -> no sync", base, *time as f64 / rate),
            }
        }
    }
}
