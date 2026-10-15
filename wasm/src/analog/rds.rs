//! The RDS decoder: the 57 kHz data subcarrier of a WFM broadcast (EN 50067).
//!
//! The chain is the cheap one the deployed encoders use (`PiFmRds`, `NanoRDS`):
//!
//! ```text
//! aux - 57 kHz mix - 2.4 kHz LPF - biphase integrate - differential - CRC sync - groups
//!       (3× pilot phase)  (one-pole)   per half bit     (per bit, coherent) (offset A)
//! ```
//!
//! The 57 kHz reference is the pilot NCO's phase tripled, which is how the transmitter derives
//! its subcarrier (the pilot sine's third harmonic as a cosine), so the mix needs no separate
//! PLL. The bit clock is the pilot's 16th subdivision (1187.5 Hz = 19 kHz / 16), so the bit
//! boundaries come from the same phase accumulator and drift with it, never against it.
//! The bit decision is differentially coherent across adjacent bits, so the carrier phase may
//! sit at any angle and rotate — a real chain's reference drifts against the subcarrier, which
//! used to cross the axes and invert the stream (bench: minutes-long decode blackouts).
//!
//! Sync is per group: hunt for a block whose 26 bits check against the block-A offset word, then
//! verify the three following blocks and parse. A station re-sends each group roughly ten times
//! a second, so re-syncing every group costs nothing and cannot stay stuck.

/// One-pole low-pass corner on the mixed subcarrier: the biphase's first null (±2.4 kHz).
const LP_HZ: f64 = 2_400.0;
/// The bandwidth of the pilot phasor that sets the 57 kHz reference phase. Narrow on purpose:
/// the reference's phase is tripled for the mix, so its per-sample noise is tripled with it.
const REF_BW_HZ: f64 = 20.0;
/// Per-bit smoothing of the two axes' energies, for the axis choice.
const AXIS_ALPHA: f64 = 0.05;
/// Bit-phase hunt: how many bits without a decoded group before the half-bit alignment steps
/// (about 2 s), and the step (one eighth of a bit: eight positions cover a full bit).
const HUNT_BITS: u32 = 2_400;
const HUNT_STEP: f64 = 0.125;
/// Bits in one block: 16 information + 10 check.
const BLOCK_BITS: usize = 26;
/// Bits in one group: four blocks.
const GROUP_BITS: usize = 4 * BLOCK_BITS;
/// The checkword generator x^10+x^8+x^7+x^5+x^4+x^3+1 (EN 50067 section 2.1).
const CHECK_POLY: u16 = 0x5B9;
/// The block offset words, added to the checkword of blocks A, B, C, C' and D.
const OFFSET_A: u16 = 0x0FC;
const OFFSET_B: u16 = 0x198;
const OFFSET_C: u16 = 0x168;
const OFFSET_CP: u16 = 0x350;
const OFFSET_D: u16 = 0x1B4;
/// RadioText characters; the programme service name has eight.
pub const RT_CHARS: usize = 64;

/// The decoded RDS state the UI reads: station identity, name and radio text.
#[derive(Debug, Clone, PartialEq)]
pub struct RdsState {
    /// At least one group decoded (the badge's RDS half).
    pub locked: bool,
    /// Groups decoded since reset.
    pub groups: u32,
    /// The programme identification code (block A of every group).
    pub pi: u16,
    /// The eight-character programme service name.
    pub ps: [u8; 8],
    /// The radio text, space-padded.
    pub rt: [u8; RT_CHARS],
}

impl Default for RdsState {
    fn default() -> Self {
        let rt = [b' '; RT_CHARS];
        Self { locked: false, groups: 0, pi: 0, ps: [b' '; 8], rt }
    }
}

impl RdsState {
    /// The programme service name as a string (the codes are ASCII in the printable range).
    pub fn ps_str(&self) -> String {
        self.ps.iter().map(|&c| c as char).collect()
    }

    /// The radio text as a string, trailing spaces trimmed.
    pub fn rt_str(&self) -> String {
        let text: String = self.rt.iter().map(|&c| c as char).collect();
        text.trim_end().to_string()
    }
}

/// One decoder instance per active WFM demodulator. `step` runs once per aux sample.
pub struct RdsDecoder {
    /// One-pole low-pass coefficient on the mixed subcarrier, one filter per axis.
    a_lp: f64,
    lp_i: f64,
    lp_q: f64,
    /// Narrow pilot phasor: the carrier reference. It must be far narrower than the pilot
    /// detector the stereo badge uses, because the reference phase is TRIPLED for the 57 kHz
    /// mix and a wide phasor carries per-sample phase noise that the tripling multiplies by
    /// three. On a real capture a 300 Hz reference left the biphase unreadable; 20-25 Hz locks.
    a_ref: f64,
    ref_i: f64,
    ref_q: f64,
    /// The unit pilot phasor cubed, conjugate: the carrier phase held across one bit.
    n3_i: f64,
    n3_q: f64,
    /// The pilot phase of the previous sample (a wrap is a downward step).
    prev_phase: f64,
    /// The bit-clock phase (0..1), one sixteenth of the pilot's advance per sample.
    bit_phase: f64,
    /// Half-bit integrators of the shaped biphase, one pair per axis.
    acc_i_first: f64,
    acc_i_second: f64,
    acc_q_first: f64,
    acc_q_second: f64,
    /// Smoothed energy per axis (bench diagnostics; the decision no longer picks an axis).
    axis_i: f64,
    axis_q: f64,
    /// The previous bit's complex symbol difference, for the differentially-coherent decision.
    /// The previous bit's complex symbol difference, for the differentially-coherent decision.
    prev_symbol_i: f64,
    prev_symbol_q: f64,
    /// The bit-boundary phase offset the hunt steers (cycles of a bit, -0.5..0.5). The chain's
    /// residual delay (the DDC's own, the detector's) shifts the pilot-derived clock by an
    /// unknown fraction of a bit, and a half-bit-off boundary turns the biphase integrals into
    /// their neighbours' mix — measured, a 25% BER with every other part of the chain healthy.
    bit_shift: f64,
    bits_since_group: u32,
    last_groups: u32,
    /// The group under assembly and its fill level.
    group: [u32; GROUP_BITS],
    group_len: usize,
    /// Bits ended since reset (bench: the decoder's own bit clock, measured over a window).
    bits: u64,
    /// The assembly holds a verified A block, so the next 78 bits are B, C and D.
    in_group: bool,
    /// The parsed state.
    pub state: RdsState,
}

impl RdsDecoder {
    pub fn new(aux_fs: f64) -> Self {
        let a_lp = 1.0 - (-2.0 * core::f64::consts::PI * LP_HZ / aux_fs).exp();
        let a_ref = 1.0 - (-2.0 * core::f64::consts::PI * REF_BW_HZ / aux_fs).exp();
        Self {
            a_lp,
            lp_i: 0.0,
            lp_q: 0.0,
            a_ref,
            ref_i: 0.0,
            ref_q: 0.0,
            n3_i: 0.0,
            n3_q: 0.0,
            prev_phase: 0.0,
            bit_phase: 0.0,
            acc_i_first: 0.0,
            acc_i_second: 0.0,
            acc_q_first: 0.0,
            acc_q_second: 0.0,
            axis_i: 0.0,
            axis_q: 0.0,
            prev_symbol_i: 0.0,
            prev_symbol_q: 0.0,
            bit_shift: 0.0,
            bits_since_group: 0,
            last_groups: 0,
            group: [0; GROUP_BITS],
            group_len: 0,
            bits: 0,
            in_group: false,
            state: RdsState::default(),
        }
    }

    /// One aux sample. `pilot_phase` is the pilot NCO's phase in cycles (0..1).
    pub fn step(&mut self, v: f64, pilot_phase: f64) {
        // The pilot's own phase, and its third harmonic by the triple-angle identity: the
        // transmitter's subcarrier is the pilot sine's third harmonic as a cosine.
        let th = core::f64::consts::TAU * pilot_phase;
        let (c, s) = (th.cos(), th.sin());
        let c3 = c * (4.0 * c * c - 3.0);
        let s3 = s * (3.0 - 4.0 * s * s);
        // The narrow pilot phasor: mix the pilot down to DC and average it long enough that its
        // phase noise stops mattering after the tripling.
        self.ref_i += self.a_ref * (v * c - self.ref_i);
        self.ref_q += self.a_ref * (-v * s - self.ref_q);

        // 57 kHz mix. Both axes run: the device's processing delay leaves a constant rotation
        // between the pilot and the subcarrier that no phase lock can know in advance.
        let bb_i = v * (self.n3_i * c3 + self.n3_q * s3);
        let bb_q = v * (self.n3_q * c3 - self.n3_i * s3);
        self.lp_i += self.a_lp * (bb_i - self.lp_i);
        self.lp_q += self.a_lp * (bb_q - self.lp_q);

        // Bit clock: 1187.5 Hz is the pilot divided by sixteen. Measure the pilot's advance
        // (its wrap makes the delta go negative), integrate one sixteenth of it into a bit
        // phase, and end a bit when that phase crosses one. The clock then follows the pilot
        // exactly, with no counter that a missed wrap could shift.
        let mut delta = pilot_phase - self.prev_phase;
        if delta < 0.0 {
            delta += 1.0;
        }
        self.prev_phase = pilot_phase;
        self.bit_phase += delta / 16.0;
        if self.bit_phase >= 1.0 {
            self.bit_phase -= 1.0;
            self.bits += 1;
            // The decision is differentially coherent: the carrier phase may sit at ANY angle
            // and drift across the axes (the recovered-pilot reference rotates slowly against
            // the subcarrier on a real chain — measured on the bench, the axis energy crossed
            // over every few minutes and the decode died until it crossed back). The phase is
            // constant across two ADJACENT bits, so the product of one half-pair difference
            // with the previous one carries the data alone, whatever the angle.
            // The decision is differentially coherent: the carrier phase may sit at ANY angle
            // and drift across the axes (the recovered-pilot reference rotates slowly against
            // the subcarrier on a real chain — measured on the bench, the axis energy crossed
            // over every few minutes and the decode died until it crossed back). The phase is
            // constant across two ADJACENT bits, so the product of one half-pair difference
            // with the previous one carries the data alone, whatever the angle.
            // Differentially coherent: the carrier phase may sit at any angle and rotate (a
            // real chain's reference drifts against the subcarrier; the axis-picking decision
            // inverted its stream each time the rotation crossed a quadrant).
            let di = self.acc_i_first - self.acc_i_second;
            let dq = self.acc_q_first - self.acc_q_second;
            let product = di * self.prev_symbol_i + dq * self.prev_symbol_q;
            self.prev_symbol_i = di;
            self.prev_symbol_q = dq;
            self.axis_i += AXIS_ALPHA * (di.abs() - self.axis_i);
            self.axis_q += AXIS_ALPHA * (dq.abs() - self.axis_q);
            let b = if product < 0.0 { 1 } else { 0 };
            self.push_bit(b);
            self.bits_since_group += 1;
            if self.state.groups != self.last_groups {
                self.last_groups = self.state.groups;
                self.bits_since_group = 0;
            } else if self.bits_since_group > HUNT_BITS {
                // No group for ~2 s: try the next half-bit alignment. The wrong alignment is a
                // constant of the session (the chain's geometry), so a slow sweep finds it and
                // a decoding stream freezes it.
                self.bit_shift = if self.bit_shift + HUNT_STEP > 0.5 {
                    self.bit_shift + HUNT_STEP - 1.0
                } else {
                    self.bit_shift + HUNT_STEP
                };
                self.bits_since_group = 0;
            }
            self.acc_i_first = 0.0;
            self.acc_i_second = 0.0;
            self.acc_q_first = 0.0;
            self.acc_q_second = 0.0;
            let mag = (self.ref_i * self.ref_i + self.ref_q * self.ref_q).sqrt();
            if mag > 1.0e-9 {
                let (a, w) = (self.ref_i / mag, self.ref_q / mag);
                self.n3_i = a * a * a - 3.0 * a * w * w;
                self.n3_q = w * w * w - 3.0 * a * a * w;
            }
        }
        // The biphase symbol is positive in the bit's first half and negative in its second —
        // at the alignment the hunt currently holds.
        if self.bit_phase + self.bit_shift < 0.5 {
            self.acc_i_first += self.lp_i;
            self.acc_q_first += self.lp_q;
        } else {
            self.acc_i_second += self.lp_i;
            self.acc_q_second += self.lp_q;
        }
    }

    /// Feed one differentially-decoded bit into the group assembler.
    fn push_bit(&mut self, bit: u32) {
        if !self.in_group {
            // Hunt: shift a 26-bit window and check it against the block-A offset word.
            self.group.copy_within(1..BLOCK_BITS, 0);
            self.group[BLOCK_BITS - 1] = bit;
            if self.check_block(0) == OFFSET_A {
                self.in_group = true;
                self.group_len = BLOCK_BITS;
            }
            return;
        }
        self.group[self.group_len] = bit;
        self.group_len += 1;
        if self.group_len == GROUP_BITS {
            self.finish_group();
        }
    }

    /// The syndrome of the 26-bit block at `at`: equals the offset word when the block is valid.
    ///
    /// The reduction is the encoder's own long division (the same loop, the same bit order), so
    /// the two cannot drift apart.
    fn check_block(&self, at: usize) -> u16 {
        let mut value = 0_u32;
        for k in at..at + BLOCK_BITS {
            value = (value << 1) | self.group[k] as u32;
        }
        for bit in (10..=25).rev() {
            if value & (1 << bit) != 0 {
                value ^= (CHECK_POLY as u32) << (bit - 10);
            }
        }
        (value & 0x3FF) as u16
    }

    /// The 16-bit information word of the block at `at`, MSB first.
    fn info_word(&self, at: usize) -> u16 {
        (at..at + 16).fold(0_u16, |acc, k| (acc << 1) | self.group[k] as u16)
    }

    /// Verify B, C and D against their offset words, then parse the group.
    fn finish_group(&mut self) {
        self.in_group = false;
        self.group_len = 0;
        let b = self.check_block(BLOCK_BITS);
        let c = self.check_block(2 * BLOCK_BITS);
        let d = self.check_block(3 * BLOCK_BITS);
        if b != OFFSET_B || d != OFFSET_D || (c != OFFSET_C && c != OFFSET_CP) {
            return;
        }
        self.state.pi = self.info_word(0);
        let block_b = self.info_word(BLOCK_BITS);
        let group_type = (block_b >> 12) as usize;
        let version_a = (block_b >> 11) & 1 == 0;
        match (group_type, version_a) {
            // 0A: two programme-service characters in block D, addressed by two segment bits.
            (0, true) => {
                // The two segment bits are block B's lowest bits (EN 50067 figure 8).
                let segment = (block_b & 0x3) as usize;
                let word = self.info_word(3 * BLOCK_BITS);
                // Block D packs the pair high byte first: codes[0] << 8 | codes[1].
                self.state.ps[segment * 2] = (word >> 8) as u8;
                self.state.ps[segment * 2 + 1] = (word & 0xFF) as u8;
                self.state.locked = true;
                self.state.groups = self.state.groups.wrapping_add(1);
            }
            // 2A: four radio-text characters in blocks C and D, addressed by four segment bits.
            (2, true) => {
                let segment = (block_b & 0xF) as usize;
                let word_c = self.info_word(2 * BLOCK_BITS);
                let word_d = self.info_word(3 * BLOCK_BITS);
                let bytes = [
                    (word_c >> 8) as u8,
                    (word_c & 0xFF) as u8,
                    (word_d >> 8) as u8,
                    (word_d & 0xFF) as u8,
                ];
                for (k, &byte) in bytes.iter().enumerate() {
                    let idx = segment * 4 + k;
                    if idx < RT_CHARS {
                        self.state.rt[idx] = byte;
                    }
                }
                self.state.locked = true;
                self.state.groups = self.state.groups.wrapping_add(1);
            }
            // Other group types (clock, alternative frequencies) carry nothing the pane shows.
            _ => {
                self.state.locked = true;
                self.state.groups = self.state.groups.wrapping_add(1);
            }
        }
    }

    /// Bench diagnostics: the reference phasor's magnitude, the two axes' smoothed energies,
    /// and the cumulative bit count.
    pub fn diag(&self) -> [f64; 4] {
        let mag = (self.ref_i * self.ref_i + self.ref_q * self.ref_q).sqrt();
        [mag, self.axis_i, self.axis_q, self.bits as f64]
    }

    pub fn reset(&mut self) {
        self.lp_i = 0.0;
        self.lp_q = 0.0;
        self.ref_i = 0.0;
        self.ref_q = 0.0;
        self.n3_i = 0.0;
        self.n3_q = 0.0;
        self.prev_phase = 0.0;
        self.bit_phase = 0.0;
        self.acc_i_first = 0.0;
        self.acc_i_second = 0.0;
        self.acc_q_first = 0.0;
        self.acc_q_second = 0.0;
        self.axis_i = 0.0;
        self.axis_q = 0.0;
        self.prev_symbol_i = 0.0;
        self.prev_symbol_q = 0.0;
        self.bit_shift = 0.0;
        self.bits_since_group = 0;
        self.last_groups = 0;
        self.group_len = 0;
        self.bits = 0;
        self.in_group = false;
        self.state = RdsState::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder's checkword: divide the word and ten zeros by the generator, add the offset.
    fn checkword(info: u16, offset: u16) -> u16 {
        let mut code = (info as u32) << 10;
        for bit in (10..=25).rev() {
            if code & (1 << bit) != 0 {
                code ^= (CHECK_POLY as u32) << (bit - 10);
            }
        }
        ((code & 0x3FF) as u16) ^ offset
    }

    /// One 26-bit block as MSB-first bits.
    fn block(info: u16, offset: u16) -> Vec<u32> {
        let value = ((info as u32) << 10) | checkword(info, offset) as u32;
        (0..BLOCK_BITS).map(|k| (value >> (25 - k)) & 1).collect()
    }

    /// One 0A group: the PI, block B (type 0, version A, TP, PTY 1, segment), no-AF block C,
    /// and the two-character name in block D.
    fn group_0a(pi: u16, segment: usize, chars: [u8; 2]) -> Vec<u32> {
        let block_b = (0 << 12) | (0 << 11) | (1 << 10) | (1 << 5) | segment as u16;
        let block_c = 0xCDCD_u16;
        let block_d = ((chars[0] as u16) << 8) | chars[1] as u16;
        [
            block(pi, OFFSET_A),
            block(block_b, OFFSET_B),
            block(block_c, OFFSET_C),
            block(block_d, OFFSET_D),
        ]
        .concat()
    }

    /// One 2A group: the radio text's four characters in blocks C and D.
    fn group_2a(pi: u16, segment: usize, chars: [u8; 4]) -> Vec<u32> {
        let block_b = (2 << 12) | (0 << 11) | (1 << 10) | (1 << 5) | segment as u16;
        let word_c = ((chars[0] as u16) << 8) | chars[1] as u16;
        let word_d = ((chars[2] as u16) << 8) | chars[3] as u16;
        [
            block(pi, OFFSET_A),
            block(block_b, OFFSET_B),
            block(word_c, OFFSET_C),
            block(word_d, OFFSET_D),
        ]
        .concat()
    }

    #[test]
    fn decodes_ps_name_from_a_clean_bit_stream() {
        let mut decoder = RdsDecoder::new(144_000.0);
        let name: [u8; 8] = *b"HAROGIC ";
        // Two full cycles so every segment arrives, as a station repeats its groups.
        for _ in 0..2 {
            for (segment, pair) in name.chunks(2).enumerate() {
                for &bit in &group_0a(0x1234, segment, [pair[0], pair[1]]) {
                    decoder.push_bit(bit);
                }
            }
        }
        assert!(decoder.state.locked, "RDS must lock");
        assert_eq!(decoder.state.pi, 0x1234);
        assert_eq!(decoder.state.ps_str(), "HAROGIC ");
    }

    #[test]
    fn decodes_radio_text_from_a_clean_bit_stream() {
        let mut decoder = RdsDecoder::new(144_000.0);
        let text: [u8; 8] = *b"hello   ";
        for (segment, quad) in text.chunks(4).enumerate() {
            let mut four = [b' '; 4];
            four[..quad.len()].copy_from_slice(quad);
            for &bit in &group_2a(0xABCD, segment, four) {
                decoder.push_bit(bit);
            }
        }
        assert!(decoder.state.locked);
        assert_eq!(decoder.state.pi, 0xABCD);
        assert_eq!(decoder.state.rt_str(), "hello");
    }

    #[test]
    fn recovers_bits_through_the_biphase_chain() {
        // The full per-sample path: a synthetic biphase subcarrier of one known group through
        // step(), with the pilot phase driving both the 57 kHz mix and the bit clock, at the
        // aux rate (7.58 samples per pilot cycle, 121.26 per bit — a real device's geometry).
        let aux_fs = 144_000.0_f64;
        let mut decoder = RdsDecoder::new(aux_fs);
        let pilot_hz = 19_000.0_f64;
        let mut phase = 0.0_f64;
        // The group from the clean-stream test, differentially encoded the way the transmitter
        // sends it: d_n = b_0 xor ... xor b_n.
        // A station repeats its groups; the decoder's narrow carrier reference needs a few bits
        // to converge, so one lone group would be a test of nothing.
        let group = group_0a(0x1234, 0, *b"HA");
        let groups = [group.clone(), group.clone(), group.clone(), group].concat();
        let bits: Vec<u32> = groups
            .iter()
            .scan(0_u32, |carry, &b| {
                *carry ^= b;
                Some(*carry)
            })
            .collect();
        // The injection level of a real station: 2 kHz of 75 kHz deviation ≈ 2.7% of full scale.
        let level = 0.027;
        let step_pilot = pilot_hz / aux_fs;
        let samples_per_bit = aux_fs / (pilot_hz / 16.0);
        let mut bit_index = 0_usize;
        let mut pilot_cycles = 0_u32;
        let mut samples = 0_usize;
        // Run one extra bit period past the last bit: the decoder decides a bit at its END
        // boundary, and a live stream always supplies those samples.
        while bit_index < bits.len() + 1 && samples < (4.0 * aux_fs) as usize {
            // The biphase baseband: a 1 is positive in the bit's first half, negative in the
            // second; a 0 is the mirror.
            let frac = samples as f64 / samples_per_bit;
            let sign = if bits[bit_index.min(bits.len() - 1)] == 1 { 1.0 } else { -1.0 };
            let baseband = sign * if frac - frac.floor() < 0.5 { level } else { -level };
            // The transmitted composite excerpt: the 19 kHz pilot (which the decoder needs to
            // derive the 57 kHz carrier phase) plus the baseband on the 57 kHz cosine.
            let pilot = 0.5 * (core::f64::consts::TAU * phase).sin();
            let carrier = (core::f64::consts::TAU * 3.0 * phase).cos();
            decoder.step(pilot + baseband * carrier, phase);
            samples += 1;
            let prev_phase = phase;
            phase += step_pilot;
            if phase >= 1.0 {
                phase -= 1.0;
            }
            // One bit index step per sixteen pilot wraps, tracked the way the decoder does.
            if phase < prev_phase {
                pilot_cycles = (pilot_cycles + 1) % 16;
                if pilot_cycles == 0 {
                    bit_index += 1;
                }
            }
        }
        assert_eq!(bit_index, bits.len() + 1, "every bit plus the deciding boundary must have run");
        assert!(decoder.state.locked, "the biphase chain must lock a clean signal");
        assert_eq!(decoder.state.pi, 0x1234);
    }

    #[test]
    fn biphase_survives_a_rotating_carrier() {
        // A real chain's recovered-pilot reference rotates slowly against the subcarrier (the
        // two clocks drift). The axis-picking decision the chain used to have inverted its bit
        // stream each time the rotation crossed a quadrant: groups stopped decoding for minutes.
        // The differentially-coherent decision must ride through every angle.
        let aux_fs = 144_000.0_f64;
        let mut decoder = RdsDecoder::new(aux_fs);
        let pilot_hz = 19_000.0_f64;
        let rotation_hz = 0.05;                    // a full turn every 20 s, all four quadrants
        let mut phase = 0.0_f64;
        let group = group_0a(0x1234, 0, *b"HA");
        let groups = core::array::from_fn::<_, 40, _>(|_| group.clone()).concat();
        let bits: Vec<u32> = groups
            .iter()
            .scan(0_u32, |carry, &b| {
                *carry ^= b;
                Some(*carry)
            })
            .collect();
        let level = 0.027;
        let step_pilot = pilot_hz / aux_fs;
        let samples_per_bit = aux_fs / (pilot_hz / 16.0);
        let mut bit_index = 0_usize;
        let mut pilot_cycles = 0_u32;
        let mut samples = 0_usize;
        let mut decoded = 0_u32;
        while bit_index < bits.len() + 1 && samples < (8.0 * aux_fs) as usize {
            let frac = samples as f64 / samples_per_bit;
            let sign = if bits[bit_index.min(bits.len() - 1)] == 1 { 1.0 } else { -1.0 };
            let baseband = sign * if frac - frac.floor() < 0.5 { level } else { -level };
            let theta = core::f64::consts::TAU * rotation_hz * samples as f64 / aux_fs;
            let pilot = 0.5 * (core::f64::consts::TAU * phase).sin();
            let carrier = (core::f64::consts::TAU * 3.0 * phase + theta).cos();
            let before = decoder.state.groups;
            decoder.step(pilot + baseband * carrier, phase);
            if decoder.state.groups > before {
                decoded += 1;
            }
            samples += 1;
            let prev_phase = phase;
            phase += step_pilot;
            if phase >= 1.0 {
                phase -= 1.0;
            }
            if phase < prev_phase {
                pilot_cycles = (pilot_cycles + 1) % 16;
                if pilot_cycles == 0 {
                    bit_index += 1;
                }
            }
        }
        assert!(
            decoded >= 30,
            "groups must keep decoding while the carrier rotates, got {decoded} of 40"
        );
        assert_eq!(decoder.state.pi, 0x1234);
    }

    #[test]
    fn syndrome_of_a_valid_block_equals_the_offset() {
        let mut decoder = RdsDecoder::new(144_000.0);
        let bits = block(0x1234, OFFSET_A);
        decoder.group[..BLOCK_BITS].copy_from_slice(&bits);
        assert_eq!(decoder.check_block(0), OFFSET_A, "a valid block's syndrome is its offset");
        let bits_b = block(0x0001, OFFSET_B);
        decoder.group[..BLOCK_BITS].copy_from_slice(&bits_b);
        assert_eq!(decoder.check_block(0), OFFSET_B);
    }

    #[test]
    fn reset_clears_the_decoded_state() {
        let mut decoder = RdsDecoder::new(144_000.0);
        for &bit in &group_0a(0x1234, 0, *b"HA") {
            decoder.push_bit(bit);
        }
        assert!(decoder.state.locked);
        decoder.reset();
        assert!(!decoder.state.locked);
        assert_eq!(decoder.state, RdsState::default());
    }
}
