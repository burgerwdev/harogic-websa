//! DRM robustness modes and their OFDM geometry.
//!
//! Every constant here is cross-checked against Dream's own table,
//! `src/tables/TableDRMGlobal.h` (the `RMA_`/`RMB_`/`RMC_`/`RMD_` FFT size, symbols per frame
//! and guard ratio), which in turn follows ETSI ES 201 980. The invariant that ties them
//! together: one frame lasts 400 ms in every mode, i.e. `symbol_len * symbols_per_frame`
//! equals `0.4 s * SAMPLE_RATE` — the receiver's per-mode timing derives from that, so the
//! tests below pin it.
//!
//! Mode E (DRM+, the VHF variant) is deliberately absent: Dream's receiver chain defines
//! `NUM_ROBUSTNESS_MODES 4` (A to D) and contains no mode E OFDM geometry, so mode E cannot
//! be ported from Dream — it needs the DRM+ specification or another reference and is its
//! own task. Adding it here without that reference would be a guess, not a port.

/// The DRM core sample rate. The receiver works at this rate; the channelizer's rate is
/// converted to it before the chain sees it.
pub const SAMPLE_RATE: u32 = 48_000;

/// DRM frames per transmission super frame (ES 201 980 §5.1.1).
pub const NUM_FRAMES_IN_SUPERFRAME: usize = 3;

/// Duration of one DRM frame, samples at [`SAMPLE_RATE`].
pub const SAMPLES_PER_FRAME: usize = (SAMPLE_RATE as usize) * 4 / 10;

/// One DRM robustness mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobustnessMode {
    A,
    B,
    C,
    D,
}

impl RobustnessMode {
    /// Every mode the ported chain supports, in FAC coding order.
    pub const ALL: [Self; 4] = [Self::A, Self::B, Self::C, Self::D];

    /// Index used by the FAC's robustness-mode field (2 bits: 00 = A ... 11 = D).
    pub const fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
            Self::C => 2,
            Self::D => 3,
        }
    }

    pub const fn from_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(Self::A),
            1 => Some(Self::B),
            2 => Some(Self::C),
            3 => Some(Self::D),
            _ => None,
        }
    }

    /// FFT (useful symbol) length, samples: Dream `RMA_FFT_SIZE_N` … `RMD_FFT_SIZE_N`.
    pub const fn fft_size(self) -> usize {
        match self {
            Self::A => 1152,
            Self::B => 1024,
            Self::C => 704,
            Self::D => 448,
        }
    }

    /// OFDM symbols per frame: Dream `RMA_NUM_SYM_PER_FRAME` … `RMD_NUM_SYM_PER_FRAME`.
    pub const fn symbols_per_frame(self) -> usize {
        match self {
            Self::A | Self::B => 15,
            Self::C => 20,
            Self::D => 24,
        }
    }

    /// Guard-interval ratio `Tg/Tu` as `(numerator, denominator)`: Dream `RMA_ENUM_TG_TU` /
    /// `RMA_DENOM_TG_TU` and the same for B to D.
    pub const fn guard_ratio(self) -> (usize, usize) {
        match self {
            Self::A => (1, 9),
            Self::B => (1, 4),
            Self::C => (4, 11),
            Self::D => (11, 14),
        }
    }

    /// Guard-interval length, samples (integer in every mode: 128, 256, 256, 352).
    pub const fn guard_len(self) -> usize {
        self.fft_size() * self.guard_ratio().0 / self.guard_ratio().1
    }

    /// One OFDM symbol including its guard interval, samples.
    pub const fn symbol_len(self) -> usize {
        self.fft_size() + self.guard_len()
    }

    /// Symbols per transmission super frame (three frames).
    pub const fn symbols_per_superframe(self) -> usize {
        self.symbols_per_frame() * NUM_FRAMES_IN_SUPERFRAME
    }

    /// Samples per frame (400 ms in every mode).
    pub const fn samples_per_frame(self) -> usize {
        self.symbol_len() * self.symbols_per_frame()
    }

    /// Number of symbols at the start of a super frame that carry the SDC.
    pub const fn sdc_symbols(self) -> usize {
        match self {
            Self::A | Self::B => 2,
            Self::C | Self::D => 3,
        }
    }

    /// Carrier spacing in Hz (`SAMPLE_RATE / fft_size`).
    pub fn carrier_spacing_hz(self) -> f64 {
        f64::from(SAMPLE_RATE) / self.fft_size() as f64
    }

    /// The DRM radio-frequency bandwidth the mode needs, Hz (10 kHz for A/B, 20 kHz for C/D
    /// at their full spectrum occupancy; the actual occupancy is signalled in the FAC).
    pub const fn nominal_bandwidth_hz(self) -> u32 {
        match self {
            Self::A | Self::B => 10_000,
            Self::C | Self::D => 20_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact figures Dream's `src/tables/TableDRMGlobal.h` defines, so a typo in the
    /// tables above cannot pass unnoticed. Format: (mode, fft, symbols/frame, guard num, den).
    const DREAM_TABLE: [(RobustnessMode, usize, usize, usize, usize); 4] = [
        (RobustnessMode::A, 1152, 15, 1, 9),
        (RobustnessMode::B, 1024, 15, 1, 4),
        (RobustnessMode::C, 704, 20, 4, 11),
        (RobustnessMode::D, 448, 24, 11, 14),
    ];

    #[test]
    fn geometry_matches_dreams_table() {
        for (mode, fft, syms, gn, gd) in DREAM_TABLE {
            assert_eq!(mode.fft_size(), fft, "mode {mode:?} FFT size");
            assert_eq!(mode.symbols_per_frame(), syms, "mode {mode:?} symbols per frame");
            assert_eq!(mode.guard_ratio(), (gn, gd), "mode {mode:?} guard ratio");
        }
    }

    #[test]
    fn guard_lengths_are_whole_samples_and_match_the_spec() {
        assert_eq!(RobustnessMode::A.guard_len(), 128);
        assert_eq!(RobustnessMode::B.guard_len(), 256);
        assert_eq!(RobustnessMode::C.guard_len(), 256);
        assert_eq!(RobustnessMode::D.guard_len(), 352);
        for mode in RobustnessMode::ALL {
            let (n, d) = mode.guard_ratio();
            assert_eq!(mode.guard_len() * d, mode.fft_size() * n, "mode {mode:?} exact guard");
        }
    }

    #[test]
    fn every_mode_has_the_same_400_ms_frame() {
        for mode in RobustnessMode::ALL {
            assert_eq!(mode.samples_per_frame(), SAMPLES_PER_FRAME, "mode {mode:?}");
            assert_eq!(mode.symbol_len() * mode.symbols_per_frame(), 19_200);
        }
    }

    #[test]
    fn mode_coding_round_trips() {
        for mode in RobustnessMode::ALL {
            assert_eq!(RobustnessMode::from_index(mode.index()), Some(mode));
        }
        assert_eq!(RobustnessMode::from_index(4), None);
    }

    #[test]
    fn carrier_spacing_follows_the_fft_size() {
        assert!((RobustnessMode::A.carrier_spacing_hz() - 41.666_666_666_666_664).abs() < 1e-9);
        assert!((RobustnessMode::B.carrier_spacing_hz() - 46.875).abs() < 1e-9);
        assert!((RobustnessMode::C.carrier_spacing_hz() - 68.181_818_181_818_18).abs() < 1e-9);
        assert!((RobustnessMode::D.carrier_spacing_hz() - 107.142_857_142_857_14).abs() < 1e-9);
    }
}
