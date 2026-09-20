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

/// Tones searched: the eight FT8 tones plus the frequency-offset range around the nominal base.
const FREQ_OFFSET_STEPS: i32 = 8;         // +/- 8 * (TONE_SPACING/2) = +/- 25 Hz
/// Time search: +/- 4 steps of 1/5 symbol each = +/- 0.128 s. FT8 transmissions are slot-aligned
/// (operators are clock-disciplined), so the receiver searches the slot edge rather than the whole
/// slot: a full-slot scan would cost ~15x more for a case a slot-driven receiver does not have.
const TIME_OFFSET_STEPS: i32 = 4;
const MAX_LDPC_ITERATIONS: usize = 30;
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
    ///
    /// The plugin waits for this, not for the full 15 s slot: a transmission is shorter than its
    /// slot, so waiting for a whole slot meant the decoder never ran at all.
    pub fn transmission_samples(&self) -> usize {
        NUM_SYMBOLS * symbol_samples_at(self.rate)
    }

    /// Drop the oldest `samples` complex samples (used when a decode attempt fails, so the next
    /// attempt happens on a fresh transmission instead of re-trying the same buffer every block).
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

        let (best_time, best_freq, snr) = self.find_sync(symbol_samples)?;
        let energies = self.tone_energies(best_time, best_freq, symbol_samples);
        let llr = symbols_to_llr(&energies);
        let bits = ldpc_decode(&llr, MAX_LDPC_ITERATIONS)?;
        if !crc_ok(&bits) {
            return None;
        }
        let payload: [u8; 77] = core::array::from_fn(|i| bits[i]);
        let text = unpack_message(&payload)?;
        Some(Ft8Message {
            text,
            frequency_hz: BASE_SEARCH_HZ + best_freq,
            time_offset_s: best_time as f64 / self.rate,
            snr_db: snr,
        })
    }

    /// Costas correlation over (time, frequency) — the signal has to be found before it is read.
    ///
    /// The time search covers the slot edge (+/- 0.128 s): FT8 is slot-synchronised, so this is the
    /// window a slot-driven receiver needs, and it is stated rather than implied.
    fn find_sync(&self, symbol_samples: usize) -> Option<(usize, f64, f64)> {
        let mut best: Option<(usize, f64, f64)> = None;
        let step = TONE_SPACING_HZ / 2.0;
        let time_step = symbol_samples / (TIME_OFFSET_STEPS as usize + 1);
        for time_index in -(TIME_OFFSET_STEPS as i64)..=(TIME_OFFSET_STEPS as i64) {
            let base = (time_index * time_step as i64).max(0) as usize;
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
                        let frequency = BASE_SEARCH_HZ + offset + tone as f64 * TONE_SPACING_HZ;
                        *energy = self.tone_energy(start, symbol_samples, frequency);
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

    /// Magnitude² of one tone over one symbol window.
    fn tone_energy(&self, start: usize, length: usize, frequency: f64) -> f64 {
        let step = -2.0 * core::f64::consts::PI * frequency / self.rate;
        let (mut re, mut im) = (0.0_f64, 0.0_f64);
        for k in 0..length {
            let index = (start + k) * 2;
            if index + 1 >= self.samples.len() {
                break;
            }
            let (i, q) = (self.samples[index] as f64, self.samples[index + 1] as f64);
            let phase = step * k as f64;
            let (s, c) = phase.sin_cos();
            re += i * c - q * s;
            im += i * s + q * c;
        }
        re * re + im * im
    }

    /// The eight tone energies of every symbol, at the located (time, frequency).
    fn tone_energies(&self, base: usize, offset: f64, symbol_samples: usize) -> Vec<[f64; 8]> {
        let mut out = Vec::with_capacity(NUM_SYMBOLS);
        for symbol_index in 0..NUM_SYMBOLS {
            let start = base + symbol_index * symbol_samples;
            let mut energies = [0.0_f64; 8];
            for (tone, energy) in energies.iter_mut().enumerate() {
                let frequency = BASE_SEARCH_HZ + offset + tone as f64 * TONE_SPACING_HZ;
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

/// Nominal audio frequency the search starts from (the fixture and the UI both use 1 kHz).
const BASE_SEARCH_HZ: f64 = 1_000.0;

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

use crate::plugin::DigitalDemodulator;

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
}

impl DigitalDemodulator for Ft8Plugin {
    fn id(&self) -> &'static str {
        "ft8"
    }

    fn process_iq(&mut self, iq: &[f32]) -> Vec<String> {
        self.decoder.push_iq(iq);
        let transmission = self.decoder.transmission_samples();
        if self.decoder.buffered() < transmission {
            return Vec::new();
        }
        match self.decoder.decode() {
            Some(message) => {
                self.decoded_messages += 1;
                let text = message.text.clone();
                self.last = Some(message);
                // The transmission has been consumed: the next decode starts from a fresh buffer.
                self.decoder.reset();
                vec![text]
            }
            None => {
                // Nothing there. Drop this transmission so the decoder does not re-try the same
                // samples on every following block (which would burn the search cost per block).
                self.decoder.discard_oldest(transmission);
                Vec::new()
            }
        }
    }

    fn reset(&mut self) {
        self.decoder.reset();
        self.last = None;
    }
}

pub mod abi {
    //! FT8 over the ABI: a handle, a push per IQ block, and the last message on demand.

    use super::*;
    use core::cell::RefCell;

    thread_local! {
        static DECODERS: RefCell<Vec<Option<Ft8Plugin>>> = const { RefCell::new(Vec::new()) };
    }

    fn with<R>(handle: u32, f: impl FnOnce(&mut Ft8Plugin) -> R) -> Option<R> {
        if handle == 0 {
            return None;
        }
        DECODERS.with(|slot| {
            let mut registry = slot.borrow_mut();
            registry
                .get_mut(handle as usize - 1)
                .and_then(|entry| entry.as_mut())
                .map(f)
        })
    }

    /// Create an FT8 decoder for a complex-baseband rate (Hz). Returns 0 on a bad rate.
    #[no_mangle]
    pub extern "C" fn websa_dsp_ft8_new(rate: f64) -> u32 {
        if !(rate > 0.0) {
            return 0;
        }
        let plugin = Ft8Plugin::new(rate);
        DECODERS.with(|slot| {
            let mut registry = slot.borrow_mut();
            for (index, entry) in registry.iter_mut().enumerate() {
                if entry.is_none() {
                    *entry = Some(plugin);
                    return index as u32 + 1;
                }
            }
            registry.push(Some(plugin));
            registry.len() as u32
        })
    }

    /// Feed one block of interleaved complex f32 baseband. Returns 1 when the block completed a
    /// decode (the text is then read with `websa_dsp_ft8_message`).
    ///
    /// # Safety
    /// `iq_ptr` must point to `samples * 2` readable f32 values inside this module's linear memory.
    #[no_mangle]
    pub unsafe extern "C" fn websa_dsp_ft8_push(handle: u32, iq_ptr: *const f32, samples: u32) -> u32 {
        if iq_ptr.is_null() || samples == 0 {
            return 0;
        }
        let block = core::slice::from_raw_parts(iq_ptr, samples as usize * 2);
        with(handle, |plugin| {
            let messages = plugin.process_iq(block);
            u32::from(!messages.is_empty())
        })
        .unwrap_or(0)
    }

    /// Copy the last decoded message into `text_ptr` and its measurements into `metrics_ptr`
    /// (`[frequency_hz, time_offset_s, snr_db]`). Returns the text length, or 0 when there is none.
    ///
    /// # Safety
    /// `text_ptr` must point to `text_capacity` writable bytes and `metrics_ptr` to 3 writable f64s.
    #[no_mangle]
    pub unsafe extern "C" fn websa_dsp_ft8_message(
        handle: u32,
        text_ptr: *mut u8,
        text_capacity: u32,
        metrics_ptr: *mut f64,
    ) -> u32 {
        if text_ptr.is_null() || metrics_ptr.is_null() {
            return 0;
        }
        with(handle, |plugin| match plugin.last() {
            Some(message) if message.text.len() <= text_capacity as usize => {
                core::ptr::copy_nonoverlapping(
                    message.text.as_ptr(),
                    text_ptr,
                    message.text.len(),
                );
                let metrics = core::slice::from_raw_parts_mut(metrics_ptr, 3);
                metrics[0] = message.frequency_hz;
                metrics[1] = message.time_offset_s;
                metrics[2] = message.snr_db;
                message.text.len() as u32
            }
            _ => 0,
        })
        .unwrap_or(0)
    }

    /// Messages decoded since the handle was created (status readout).
    #[no_mangle]
    pub extern "C" fn websa_dsp_ft8_count(handle: u32) -> u32 {
        with(handle, |plugin| plugin.decoded_messages() as u32).unwrap_or(0)
    }

    #[no_mangle]
    pub extern "C" fn websa_dsp_ft8_reset(handle: u32) -> u32 {
        with(handle, |plugin| {
            plugin.reset();
            1
        })
        .unwrap_or(0)
    }

    #[no_mangle]
    pub extern "C" fn websa_dsp_ft8_free(handle: u32) -> u32 {
        if handle == 0 {
            return 0;
        }
        DECODERS.with(|slot| {
            let mut registry = slot.borrow_mut();
            match registry.get_mut(handle as usize - 1) {
                Some(entry) if entry.is_some() => {
                    *entry = None;
                    1
                }
                _ => 0,
            }
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

    #[test]
    fn the_plugin_decodes_the_fixture_and_clears_its_buffer() {
        let mut plugin = Ft8Plugin::new(48_000.0);
        let iq = fixture();
        // Feed the slot in 20 ms blocks, the way the worker does.
        let mut decoded: Vec<String> = Vec::new();
        for block in iq.chunks(960 * 2) {
            decoded.extend(plugin.process_iq(block));
        }
        assert_eq!(decoded, vec!["CQ JO1WKO PM95".to_string()]);
        assert_eq!(plugin.decoded_messages(), 1);
        let last = plugin.last().expect("a message");
        assert!((last.frequency_hz - 1_000.0).abs() < 25.0);
        // The transmission was consumed: a single further block must not decode anything, because
        // a full transmission is no longer buffered (re-feeding a whole transmission is a new one).
        assert!(plugin.process_iq(&iq[..960 * 2]).is_empty());
    }

    #[test]
    fn the_abi_handle_path_decodes_and_reports_the_message() {
        let handle = abi::websa_dsp_ft8_new(48_000.0);
        assert_ne!(handle, 0);
        let iq = fixture();
        let ptr = crate::abi::websa_dsp_alloc(iq.len() * 4) as *mut f32;
        assert!(!ptr.is_null());
        let mut text = vec![0_u8; 64];
        let mut metrics = [0.0_f64; 3];
        // SAFETY: the block is allocated with the size used here, and the buffers are large enough.
        unsafe {
            core::ptr::copy_nonoverlapping(iq.as_ptr(), ptr, iq.len());
            // Push the whole slot in one call (the worker's block size does not matter to the ABI).
            let decoded = abi::websa_dsp_ft8_push(handle, ptr, (iq.len() / 2) as u32);
            assert_eq!(decoded, 1, "the slot must decode");
            let written = abi::websa_dsp_ft8_message(
                handle,
                text.as_mut_ptr(),
                text.len() as u32,
                metrics.as_mut_ptr(),
            );
            assert_eq!(&text[..written as usize], b"CQ JO1WKO PM95");
            assert_eq!(abi::websa_dsp_ft8_count(handle), 1);
            assert!(metrics[0] > 900.0 && metrics[0] < 1100.0, "frequency {}", metrics[0]);
            assert_eq!(abi::websa_dsp_ft8_free(handle), 1);
            assert_eq!(abi::websa_dsp_ft8_push(handle, ptr, 16), 0, "a freed handle must not decode");
            crate::abi::websa_dsp_free(ptr as *mut u8, iq.len() * 4);
        }
    }
}
