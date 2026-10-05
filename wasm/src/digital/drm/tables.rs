//! Constant tables from ES 201 980 (transcribed from the standard; DecDRM's
//! `tables.rs` was used as a cross-check reference). Phases are "normalised to 1024":
//! a value `p` means an angle of `2π·p/1024`.

use crate::digital::drm::params::RobustnessMode;

// ---------------------------------------------------------------------------------
// Pilots and FAC positions (§8.4)
// ---------------------------------------------------------------------------------

/// Number of FAC cells per transmission frame (§8.4.5.1).
pub const NUM_FAC_CELLS: usize = 65;
/// Number of FAC cells per frame in robustness mode E (DRM+) (§8.5.2, table 66).
pub const NUM_FAC_CELLS_E: usize = 244;

pub const fn fac_cell_count(mode: RobustnessMode) -> usize {
    match mode {
        RobustnessMode::E => NUM_FAC_CELLS_E,
        _ => NUM_FAC_CELLS,
    }
}

/// FAC cell positions (symbol within frame, carrier index) (§8.4.5.1, tables 99–102;
/// mode E table 66).
pub const fn fac_positions(mode: RobustnessMode) -> &'static [(u8, i16)] {
    match mode {
        RobustnessMode::A => &FAC_A,
        RobustnessMode::B => &FAC_B,
        RobustnessMode::C => &FAC_C,
        RobustnessMode::D => &FAC_D,
        RobustnessMode::E => &FAC_E,
    }
}

/// Mode E's FAC cell positions (§8.5.2, table 66): the 244 4-QAM FAC cells span symbols
/// 5–26 of each 40-symbol frame, a four-symbol pattern (11/12/12/11 cells on carriers
/// stepped by 16) with the last symbol carrying only its first three cells.
const FAC_E: [(u8, i16); NUM_FAC_CELLS_E] = build_fac_e();

const fn build_fac_e() -> [(u8, i16); NUM_FAC_CELLS_E] {
    let mut out = [(0u8, 0i16); NUM_FAC_CELLS_E];
    let mut idx = 0usize;
    let mut s = 5u8;
    while s <= 26 {
        let (start, count) = match s % 4 {
            1 => (-78i16, 11usize),
            2 => (-90i16, 12usize),
            3 => (-86i16, 12usize),
            _ => (-82i16, 11usize),
        };
        let count = if s == 26 { 3 } else { count };
        let mut j = 0usize;
        while j < count {
            out[idx] = (s, start + j as i16 * 16);
            idx += 1;
            j += 1;
        }
        s += 1;
    }
    out
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
/// Mode E defines none (table 51).
pub const fn freq_pilots(mode: RobustnessMode) -> &'static [(i16, u16)] {
    match mode {
        RobustnessMode::A => &[(18, 205), (54, 836), (72, 215)],
        RobustnessMode::B => &[(16, 331), (48, 651), (64, 555)],
        RobustnessMode::C => &[(11, 214), (33, 392), (44, 242)],
        RobustnessMode::D => &[(7, 788), (21, 1014), (28, 332)],
        RobustnessMode::E => &[],
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
        RobustnessMode::E => &TIME_E,
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
/// Mode E time reference cells (§8.4.3, table 57): 21 cells in symbol 0 of each frame.
const TIME_E: [(i16, u16); 21] = [
    (-80, 219), (-79, 475), (-77, 987), (-53, 652), (-52, 652), (-51, 140),
    (-32, 819), (-31, 819), (12, 907), (13, 907), (14, 651), (21, 903),
    (22, 391), (23, 903), (40, 203), (41, 203), (42, 203), (67, 797),
    (68, 29), (79, 508), (80, 508),
];

/// Parameters of the gain reference (scattered) pilot grid (§8.4.4).
///
/// Modes A–D use the formula `(4·Z256 + p·W1024 + p²·(1+s)·Q1024) mod 1024`; mode E uses
/// `(p²·R1024 + p·Z1024 + Q1024[n,m]) mod 1024` with a per-cell `Q1024` matrix instead of a
/// scalar. The slices `w`/`q` are empty for mode E and `r`/`q_mat` are empty for modes A–D.
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
    pub r: &'static [i32],
    pub q: i32,
    pub q_mat: &'static [i32],
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
            r: &[],
            q: 36,
            q_mat: &[],
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
            r: &[],
            q: 12,
            q_mat: &[],
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
            r: &[],
            q: 12,
            q_mat: &[],
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
            r: &[],
            q: 14,
            q_mat: &[],
        },
        RobustnessMode::E => ScatteredPilotParams {
            freq_int: 4,
            time_int: 4,
            x: 4,
            y: 4,
            k0: 2,
            wz_cols: 10,
            w: &[],
            // Z1024 (§8.4.4.3.6), 4 rows × 10 columns.
            z: &[
                473, 394, 315, 236, 158, 79, 0, 0, 0, 0, //
                183, 914, 402, 37, 475, 841, 768, 768, 987, 183, //
                549, 622, 475, 110, 37, 622, 256, 768, 329, 549, //
                79, 158, 236, 315, 394, 473, 158, 315, 473, 630,
            ],
            // R1024 (§8.4.4.3.6), 4 rows × 10 columns.
            r: &[
                39, 118, 197, 276, 354, 433, 39, 118, 197, 276, //
                37, 183, 402, 37, 183, 402, 37, 183, 402, 37, //
                110, 329, 475, 110, 329, 475, 110, 329, 475, 110, //
                79, 158, 236, 315, 394, 473, 79, 158, 236, 315,
            ],
            q: 0,
            // Q1024 (§8.4.4.3.6), 4 rows × 10 columns (a matrix, not a scalar).
            q_mat: &[
                329, 489, 894, 419, 607, 519, 1020, 942, 817, 939, //
                824, 1023, 74, 319, 225, 207, 348, 422, 395, 92, //
                959, 379, 7, 738, 500, 920, 440, 727, 263, 733, //
                907, 946, 924, 91, 189, 133, 910, 804, 1022, 433,
            ],
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
    const E: [[i16; 4]; 6] = [
        [-106, -102, 102, 106],
        [0; 4],
        [0; 4],
        [0; 4],
        [0; 4],
        [0; 4],
    ];
    match mode {
        RobustnessMode::A => A[so],
        RobustnessMode::B => B[so],
        RobustnessMode::C => C[so],
        RobustnessMode::D => D[so],
        RobustnessMode::E => E[so],
    }
}

/// Average power of data cells, normal pilots and boosted pilots (§8.4.1).
pub const DATA_CELL_POWER: f64 = 1.0;
pub const PILOT_POWER: f64 = 2.0;
pub const BOOSTED_PILOT_POWER: f64 = 4.0;

// ---------------------------------------------------------------------------------
// AFS references (mode E only, §8.4.5)
// ---------------------------------------------------------------------------------

/// Number of AFS reference cells per AFS-bearing symbol (robustness mode E only).
pub const NUM_AFS_PILOTS: usize = 54;

/// Phase of AFS reference cells in symbol 4 of the first transmission frame (§8.4.5.1,
/// table 61). Entry `i` is carrier `-106 + 4·i`.
pub const AFS_PHASE_S4: [u16; NUM_AFS_PILOTS] = [
    134, 866, 588, 325, 77, 868, 649, 445, 256, 82, 946, 801, 671, 556, 455, 369, 298, 242,
    200, 173, 161, 164, 181, 213, 260, 322, 398, 489, 595, 716, 851, 1001, 142, 322, 516, 725,
    949, 164, 417, 685, 968, 242, 554, 881, 199, 556, 927, 289, 690, 82, 512, 957, 393, 868,
];

/// Phase of AFS reference cells in symbol 39 of the fourth transmission frame (§8.4.5.1,
/// table 61). Entry `i` is carrier `-106 + 4·i`.
pub const AFS_PHASE_S39: [u16; NUM_AFS_PILOTS] = [
    115, 135, 194, 293, 431, 608, 825, 57, 353, 688, 38, 452, 905, 373, 905, 452, 39, 689,
    354, 59, 827, 610, 433, 295, 197, 138, 118, 138, 197, 295, 433, 610, 827, 59, 354, 689,
    39, 452, 905, 373, 905, 452, 38, 688, 353, 57, 825, 608, 431, 293, 194, 135, 115, 134,
];

/// Carrier index of AFS reference cell `i` (`-106 + 4·i`).
pub const fn afs_carrier(i: usize) -> i16 {
    -106 + 4 * i as i16
}

/// Power of an AFS-only reference cell (§8.4.5.2): amplitude 1.0, not boosted. AFS cells
/// that coincide with a gain reference keep the gain reference's amplitude.
pub const AFS_PILOT_POWER: f64 = 1.0;

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
/// Generator tap masks of the 1/6 mother code (table 27): octal 133, 171, 145 repeated.
/// The first four also form the DRM30 1/4 mother code.
pub const GENERATORS: [u8; 6] = [0o155, 0o117, 0o123, 0o155, 0o117, 0o123];

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

pub const CODE_RATES: [CodeRate; 15] = [
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
    // Mode E MSC 4-QAM protection level 2 (table 29/27): 2/5.
    CodeRate { rx: 2, ry: 5, pattern: &[P0111, P0011] },
    // Mode E MSC 16-QAM protection level 0 (tables 27/31): 1/6.
    CodeRate { rx: 1, ry: 6, pattern: &[0b11_1111] },
];

/// Mode E MSC 4-QAM code-rate indices for protection levels 0..3.
pub const MSC4_E: [usize; 4] = [0, 2, 13, 4];
/// Mode E MSC 16-QAM code-rate combinations [R0, R1] and RYlcm (table 31).
pub const MSC16_E: [([usize; 2], usize); 4] = [
    ([14, 4], 6), ([0, 5], 28), ([2, 7], 3), ([4, 9], 4),
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
