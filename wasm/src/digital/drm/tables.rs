//! Constant tables from ES 201 980 (transcribed from the standard; DecDRM's
//! `tables.rs` was used as a cross-check reference). Phases are "normalised to 1024":
//! a value `p` means an angle of `2π·p/1024`.

use crate::digital::drm::params::RobustnessMode;

// ---------------------------------------------------------------------------------
// Pilots and FAC positions (§8.4)
// ---------------------------------------------------------------------------------

/// Number of FAC cells per transmission frame (§8.4.5.1).
pub const NUM_FAC_CELLS: usize = 65;

/// FAC cell positions (symbol within frame, carrier index) (§8.4.5.1, tables 99–102).
pub const fn fac_positions(mode: RobustnessMode) -> &'static [(u8, i16); NUM_FAC_CELLS] {
    match mode {
        RobustnessMode::A => &FAC_A,
        RobustnessMode::B => &FAC_B,
        RobustnessMode::C => &FAC_C,
        RobustnessMode::D => &FAC_D,
    }
}

const FAC_A: [(u8, i16); NUM_FAC_CELLS] = [
    (2, 26), (2, 46), (2, 66), (2, 86),
    (3, 10), (3, 30), (3, 50), (3, 70), (3, 90),
    (4, 14), (4, 22), (4, 34), (4, 62), (4, 74), (4, 94),
    (5, 26), (5, 38), (5, 58), (5, 66), (5, 78),
    (6, 22), (6, 30), (6, 42), (6, 62), (6, 70), (6, 82),
    (7, 26), (7, 34), (7, 46), (7, 66), (7, 74), (7, 86),
    (8, 10), (8, 30), (8, 38), (8, 50), (8, 58), (8, 70), (8, 78), (8, 90),
    (9, 14), (9, 22), (9, 34), (9, 42), (9, 62), (9, 74), (9, 82), (9, 94),
    (10, 26), (10, 38), (10, 46), (10, 66), (10, 86),
    (11, 10), (11, 30), (11, 50), (11, 70), (11, 90),
    (12, 14), (12, 34), (12, 74), (12, 94),
    (13, 38), (13, 58), (13, 78),
];

const FAC_B: [(u8, i16); NUM_FAC_CELLS] = [
    (2, 13), (2, 25), (2, 43), (2, 55), (2, 67),
    (3, 15), (3, 27), (3, 45), (3, 57), (3, 69),
    (4, 17), (4, 29), (4, 47), (4, 59), (4, 71),
    (5, 19), (5, 31), (5, 49), (5, 61), (5, 73),
    (6, 9), (6, 21), (6, 33), (6, 51), (6, 63), (6, 75),
    (7, 11), (7, 23), (7, 35), (7, 53), (7, 65), (7, 77),
    (8, 13), (8, 25), (8, 37), (8, 55), (8, 67), (8, 79),
    (9, 15), (9, 27), (9, 39), (9, 57), (9, 69), (9, 81),
    (10, 17), (10, 29), (10, 41), (10, 59), (10, 71), (10, 83),
    (11, 19), (11, 31), (11, 43), (11, 61), (11, 73),
    (12, 21), (12, 33), (12, 45), (12, 63), (12, 75),
    (13, 23), (13, 35), (13, 47), (13, 65), (13, 77),
];

const FAC_C: [(u8, i16); NUM_FAC_CELLS] = [
    (3, 9), (3, 21), (3, 45), (3, 57),
    (4, 23), (4, 35), (4, 47),
    (5, 13), (5, 25), (5, 37), (5, 49),
    (6, 15), (6, 27), (6, 39), (6, 51),
    (7, 5), (7, 17), (7, 29), (7, 41), (7, 53),
    (8, 7), (8, 19), (8, 31), (8, 43), (8, 55),
    (9, 9), (9, 21), (9, 45), (9, 57),
    (10, 23), (10, 35), (10, 47),
    (11, 13), (11, 25), (11, 37), (11, 49),
    (12, 15), (12, 27), (12, 39), (12, 51),
    (13, 5), (13, 17), (13, 29), (13, 41), (13, 53),
    (14, 7), (14, 19), (14, 31), (14, 43), (14, 55),
    (15, 9), (15, 21), (15, 45), (15, 57),
    (16, 23), (16, 35), (16, 47),
    (17, 13), (17, 25), (17, 37), (17, 49),
    (18, 15), (18, 27), (18, 39), (18, 51),
];

const FAC_D: [(u8, i16); NUM_FAC_CELLS] = [
    (3, 9), (3, 18), (3, 27),
    (4, 10), (4, 19),
    (5, 11), (5, 20), (5, 29),
    (6, 12), (6, 30),
    (7, 13), (7, 22), (7, 31),
    (8, 5), (8, 14), (8, 23), (8, 32),
    (9, 6), (9, 15), (9, 24), (9, 33),
    (10, 16), (10, 25), (10, 34),
    (11, 8), (11, 17), (11, 26), (11, 35),
    (12, 9), (12, 18), (12, 27), (12, 36),
    (13, 10), (13, 19), (13, 37),
    (14, 11), (14, 20), (14, 29),
    (15, 12), (15, 30),
    (16, 13), (16, 22), (16, 31),
    (17, 5), (17, 14), (17, 23), (17, 32),
    (18, 6), (18, 15), (18, 24), (18, 33),
    (19, 16), (19, 25), (19, 34),
    (20, 8), (20, 17), (20, 26), (20, 35),
    (21, 9), (21, 18), (21, 27), (21, 36),
    (22, 10), (22, 19), (22, 37),
];

/// Frequency reference pilots: (carrier, phase₁₀₂₄) (§8.4.2, table 86), every symbol.
pub const fn freq_pilots(mode: RobustnessMode) -> &'static [(i16, u16); 3] {
    match mode {
        RobustnessMode::A => &[(18, 205), (54, 836), (72, 215)],
        RobustnessMode::B => &[(16, 331), (48, 651), (64, 555)],
        RobustnessMode::C => &[(11, 214), (33, 392), (44, 242)],
        RobustnessMode::D => &[(7, 788), (21, 1014), (28, 332)],
    }
}

/// Time reference pilots in symbol 0 of each frame: (carrier, phase₁₀₂₄) (§8.4.3,
/// table 87).
pub const fn time_pilots(mode: RobustnessMode) -> &'static [(i16, u16)] {
    match mode {
        RobustnessMode::A => &TIME_A,
        RobustnessMode::B => &TIME_B,
        RobustnessMode::C => &TIME_C,
        RobustnessMode::D => &TIME_D,
    }
}

const TIME_A: [(i16, u16); 21] = [
    (17, 973), (18, 205), (19, 717), (21, 264), (28, 357), (29, 357), (32, 952),
    (33, 440), (39, 856), (40, 88), (41, 88), (53, 68), (54, 836), (55, 836),
    (56, 836), (60, 1008), (61, 1008), (63, 752), (71, 215), (72, 215), (73, 727),
];
const TIME_B: [(i16, u16); 19] = [
    (14, 304), (16, 331), (18, 108), (20, 620), (24, 192), (26, 704), (32, 44),
    (36, 432), (42, 588), (44, 844), (48, 651), (49, 651), (50, 651), (54, 460),
    (56, 460), (62, 944), (64, 555), (66, 940), (68, 428),
];
const TIME_C: [(i16, u16); 19] = [
    (8, 722), (10, 466), (11, 214), (12, 214), (14, 479), (16, 516), (18, 260),
    (22, 577), (24, 662), (28, 3), (30, 771), (32, 392), (33, 392), (36, 37),
    (38, 37), (42, 474), (44, 242), (45, 242), (46, 754),
];
const TIME_D: [(i16, u16); 21] = [
    (5, 636), (6, 124), (7, 788), (8, 788), (9, 200), (11, 688), (12, 152),
    (14, 920), (15, 920), (17, 644), (18, 388), (20, 652), (21, 1014), (23, 176),
    (24, 176), (26, 752), (27, 496), (28, 332), (29, 432), (30, 964), (32, 452),
];

/// Parameters of the gain reference (scattered) pilot grid (§8.4.4).
#[derive(Debug, Clone, Copy)]
pub struct ScatteredPilotParams {
    pub freq_int: usize,
    pub time_int: usize,
    pub x: i32,
    pub y: i32,
    pub k0: i32,
    pub wz_cols: usize,
    pub w: &'static [i32],
    pub z: &'static [i32],
    pub q: i32,
}

pub const fn scattered_pilots(mode: RobustnessMode) -> ScatteredPilotParams {
    match mode {
        RobustnessMode::A => ScatteredPilotParams {
            freq_int: 4,
            time_int: 5,
            x: 4,
            y: 5,
            k0: 2,
            wz_cols: 3,
            w: &[228, 341, 455, 455, 569, 683, 683, 796, 910, 910, 0, 114, 114, 228, 341],
            z: &[0, 81, 248, 18, 106, 106, 122, 116, 31, 129, 129, 39, 33, 32, 111],
            q: 36,
        },
        RobustnessMode::B => ScatteredPilotParams {
            freq_int: 2,
            time_int: 3,
            x: 2,
            y: 3,
            k0: 1,
            wz_cols: 5,
            w: &[512, 0, 512, 0, 512, 0, 512, 0, 512, 0, 512, 0, 512, 0, 512],
            z: &[0, 57, 164, 64, 12, 168, 255, 161, 106, 118, 25, 232, 132, 233, 38],
            q: 12,
        },
        RobustnessMode::C => ScatteredPilotParams {
            freq_int: 2,
            time_int: 2,
            x: 2,
            y: 2,
            k0: 1,
            wz_cols: 10,
            w: &[
                465, 372, 279, 186, 93, 0, 931, 838, 745, 652, //
                931, 838, 745, 652, 559, 465, 372, 279, 186, 93,
            ],
            z: &[
                0, 76, 29, 76, 9, 190, 161, 248, 33, 108, //
                179, 178, 83, 253, 127, 105, 101, 198, 250, 145,
            ],
            q: 12,
        },
        RobustnessMode::D => ScatteredPilotParams {
            freq_int: 1,
            time_int: 3,
            x: 1,
            y: 3,
            k0: 1,
            wz_cols: 8,
            w: &[
                366, 439, 512, 585, 658, 731, 805, 878, //
                731, 805, 878, 951, 0, 73, 146, 219, //
                73, 146, 219, 293, 366, 439, 512, 585,
            ],
            z: &[
                0, 240, 17, 60, 220, 38, 151, 101, //
                110, 7, 78, 82, 175, 150, 106, 25, //
                165, 7, 252, 124, 253, 177, 197, 142,
            ],
            q: 14,
        },
    }
}

/// Carrier indices of the boosted (power 4) gain references at the band edges, indexed
/// by spectrum occupancy (§8.4.4.3.2, table 92). Zero rows are undefined combinations.
pub const fn boosted_pilots(mode: RobustnessMode, so: usize) -> [i16; 4] {
    const A: [[i16; 4]; 6] = [
        [2, 6, 98, 102],
        [2, 6, 110, 114],
        [-102, -98, 98, 102],
        [-114, -110, 110, 114],
        [-98, -94, 310, 314],
        [-110, -106, 346, 350],
    ];
    const B: [[i16; 4]; 6] = [
        [1, 3, 89, 91],
        [1, 3, 101, 103],
        [-91, -89, 89, 91],
        [-103, -101, 101, 103],
        [-87, -85, 277, 279],
        [-99, -97, 309, 311],
    ];
    const C: [[i16; 4]; 6] = [
        [0; 4],
        [0; 4],
        [0; 4],
        [-69, -67, 67, 69],
        [0; 4],
        [-67, -65, 211, 213],
    ];
    const D: [[i16; 4]; 6] = [
        [0; 4],
        [0; 4],
        [0; 4],
        [-44, -43, 43, 44],
        [0; 4],
        [-43, -42, 134, 135],
    ];
    match mode {
        RobustnessMode::A => A[so],
        RobustnessMode::B => B[so],
        RobustnessMode::C => C[so],
        RobustnessMode::D => D[so],
    }
}

/// Average power of data cells, normal pilots and boosted pilots (§8.4.1).
pub const DATA_CELL_POWER: f64 = 1.0;
pub const PILOT_POWER: f64 = 2.0;
pub const BOOSTED_PILOT_POWER: f64 = 4.0;

// ---------------------------------------------------------------------------------
// QAM mapping (§7.4)
// ---------------------------------------------------------------------------------

/// Normalised PAM amplitudes per axis (index bits are the level bits, level 0 most
/// significant).
pub const QAM4: [f64; 2] = [std::f64::consts::FRAC_1_SQRT_2, -std::f64::consts::FRAC_1_SQRT_2];

pub const QAM16: [f64; 4] = [0.948_683_298_0, -0.316_227_766_0, 0.316_227_766_0, -0.948_683_298_0];

pub const QAM64_SM: [f64; 8] = [
    1.080_123_449_7,
    -0.154_303_349_9,
    0.462_910_049_8,
    -0.771_516_749_8,
    0.771_516_749_8,
    -0.462_910_049_8,
    0.154_303_349_9,
    -1.080_123_449_7,
];

pub const QAM64_HMSYM: [f64; 8] = [
    1.080_123_449_7,
    0.462_910_049_8,
    0.771_516_749_8,
    0.154_303_349_9,
    -0.154_303_349_9,
    -0.771_516_749_8,
    -0.462_910_049_8,
    -1.080_123_449_7,
];

/// HMmix uses different tables for the in-phase and quadrature axes.
pub const QAM64_HMMIX_RE: [f64; 8] = [
    1.080_123_449_7,
    0.462_910_049_8,
    0.771_516_749_8,
    0.154_303_349_9,
    -0.154_303_349_9,
    -0.771_516_749_8,
    -0.462_910_049_8,
    -1.080_123_449_7,
];
pub const QAM64_HMMIX_IM: [f64; 8] = [
    1.080_123_449_7,
    -0.154_303_349_9,
    0.462_910_049_8,
    -0.771_516_749_8,
    0.771_516_749_8,
    -0.462_910_049_8,
    0.154_303_349_9,
    -1.080_123_449_7,
];

// ---------------------------------------------------------------------------------
// Channel coding (§7.3) — used by the FAC/SDC/MSC decoders (task-3).
// ---------------------------------------------------------------------------------

/// Constraint length of the DRM convolutional mother code (rate 1/4).
pub const CONSTRAINT_LENGTH: usize = 7;
/// Number of trellis states.
pub const NUM_STATES: usize = 1 << (CONSTRAINT_LENGTH - 1);
/// Generator polynomials as tap masks (bit `i` holds the input delayed by `i`, bit 0
/// the current input), bit-reversed forms of octal 133, 171, 145, 133.
pub const GENERATORS: [u8; 4] = [0o155, 0o117, 0o123, 0o155];

/// Puncture mask: bit `i` set ⇒ mother-code output `b_i` is transmitted.
pub type PunctureMask = u8;

const P1111: PunctureMask = 0b1111;
const P0111: PunctureMask = 0b0111;
const P0011: PunctureMask = 0b0011;
const P0001: PunctureMask = 0b0001;
const P0101: PunctureMask = 0b0101;

/// A code rate: input bits per period `rx`, output bits per period `ry`, and one
/// puncture mask per input bit of the period.
#[derive(Debug, Clone, Copy)]
pub struct CodeRate {
    pub rx: usize,
    pub ry: usize,
    pub pattern: &'static [PunctureMask],
}

impl CodeRate {
    pub fn rate(&self) -> f64 {
        self.rx as f64 / self.ry as f64
    }
}

pub const CODE_RATES: [CodeRate; 13] = [
    CodeRate { rx: 1, ry: 4, pattern: &[P1111] },
    CodeRate { rx: 3, ry: 10, pattern: &[P1111, P0111, P0111] },
    CodeRate { rx: 1, ry: 3, pattern: &[P0111] },
    CodeRate { rx: 4, ry: 11, pattern: &[P0111, P0111, P0111, P0011] },
    CodeRate { rx: 1, ry: 2, pattern: &[P0011] },
    CodeRate { rx: 4, ry: 7, pattern: &[P0011, P0101, P0011, P0001] },
    CodeRate { rx: 3, ry: 5, pattern: &[P0011, P0001, P0011] },
    CodeRate { rx: 2, ry: 3, pattern: &[P0011, P0001] },
    CodeRate { rx: 8, ry: 11, pattern: &[P0011, P0001, P0001, P0011, P0001, P0001, P0011, P0001] },
    CodeRate { rx: 3, ry: 4, pattern: &[P0011, P0001, P0001] },
    CodeRate { rx: 4, ry: 5, pattern: &[P0011, P0001, P0001, P0001] },
    CodeRate { rx: 7, ry: 8, pattern: &[P0011, P0001, P0001, P0001, P0001, P0001, P0001] },
    CodeRate { rx: 8, ry: 9, pattern: &[P0011, P0001, P0001, P0001, P0001, P0001, P0001, P0001] },
];

/// FAC 4-QAM code-rate index (rate 0.6).
pub const FAC_RATE: usize = 6;
/// SDC 4-QAM code-rate index (rate 0.5).
pub const SDC4_RATE: usize = 4;
/// SDC 16-QAM code-rate indices [R0, R1] (overall rate 0.5).
pub const SDC16_RATES: [usize; 2] = [2, 7];

/// Bit-interleaver constants t₀ for the two interleaver types (§7.3.3).
pub const BIT_INTERLEAVER_T0: [usize; 2] = [13, 21];

/// MSC 16-QAM SM code-rate indices per protection level: [R0, R1] and RYlcm (§7.5.1).
pub const MSC16_SM: [([usize; 2], usize); 2] = [([2, 7], 3), ([4, 9], 4)];

/// MSC 64-QAM SM code-rate indices per protection level: [R0, R1, R2] and RYlcm.
pub const MSC64_SM: [([usize; 3], usize); 4] =
    [([0, 4, 9], 4), ([2, 7, 10], 15), ([4, 9, 11], 8), ([7, 10, 12], 45)];

/// MSC 64-QAM HMsym code-rate indices: [R0 (VSPP), R1, R2] and RYlcm.
pub const MSC64_HMSYM: [([usize; 3], usize); 4] =
    [([4, 1, 6], 10), ([5, 3, 8], 11), ([6, 5, 11], 56), ([7, 7, 12], 9)];

/// MSC 64-QAM HMmix code-rate indices: [R0Re, R0Im, R1Re, R1Im, R2Re, R2Im] and RYlcm.
pub const MSC64_HMMIX: [([usize; 6], usize); 4] = [
    ([4, 0, 1, 4, 6, 9], 20),
    ([5, 2, 3, 7, 8, 10], 165),
    ([6, 4, 5, 9, 11, 11], 56),
    ([7, 7, 7, 10, 12, 12], 45),
];

/// Puncturing patterns for the six tail bits, selected by
/// `r_p = (2·N₂ − 12) − RY·⌊(2·N₂ − 12)/RY⌋` (§7.3.1, table 68).
pub const TAIL_PATTERNS: [[PunctureMask; 6]; 12] = [
    [P0011, P0011, P0011, P0011, P0011, P0011],
    [P0111, P0011, P0011, P0011, P0011, P0011],
    [P0111, P0011, P0011, P0111, P0011, P0011],
    [P0111, P0111, P0011, P0111, P0011, P0011],
    [P0111, P0111, P0011, P0111, P0111, P0011],
    [P0111, P0111, P0111, P0111, P0111, P0011],
    [P0111, P0111, P0111, P0111, P0111, P0111],
    [P1111, P0111, P0111, P0111, P0111, P0111],
    [P1111, P0111, P0111, P1111, P0111, P0111],
    [P1111, P1111, P0111, P1111, P0111, P0111],
    [P1111, P1111, P0111, P1111, P0111, P1111],
    [P1111, P1111, P1111, P1111, P0111, P1111],
];
