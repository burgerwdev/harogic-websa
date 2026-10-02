//! DRM30 transmission parameters (ES 201 980 §8.1): robustness modes, spectrum
//! occupancy and the OFDM numerology derived from them.
//!
//! Independent reimplementation (no code copied) of the standard tables; DecDRM's
//! `params.rs` was used as a reference for cross-checking. The working sample rate is
//! 48 kHz, at which every mode has an integer number of samples per symbol.

/// Working sample rate of the DRM chain, Hz.
pub const SAMPLE_RATE: u32 = 48_000;
/// Transmission frames per transmission super frame (§8.1).
pub const FRAMES_PER_SUPERFRAME: usize = 3;
/// Duration of one transmission frame in samples (400 ms).
pub const SAMPLES_PER_FRAME: usize = 19_200;

/// DRM30 robustness mode (§8.1, table 82).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RobustnessMode {
    A,
    B,
    C,
    D,
}

impl RobustnessMode {
    pub const ALL: [RobustnessMode; 4] = [Self::A, Self::B, Self::C, Self::D];

    pub const fn index(self) -> usize {
        self as usize
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

    /// Length of the useful symbol part Tu in samples (= FFT size).
    pub const fn fft_size(self) -> usize {
        match self {
            Self::A => 1152,
            Self::B => 1024,
            Self::C => 704,
            Self::D => 448,
        }
    }

    /// Guard interval length Tg in samples.
    pub const fn guard_len(self) -> usize {
        match self {
            Self::A => 128,
            Self::B => 256,
            Self::C => 256,
            Self::D => 352,
        }
    }

    /// Total symbol length Ts = Tu + Tg in samples.
    pub const fn symbol_len(self) -> usize {
        self.fft_size() + self.guard_len()
    }

    /// Number of OFDM symbols per transmission frame.
    pub const fn symbols_per_frame(self) -> usize {
        match self {
            Self::A | Self::B => 15,
            Self::C => 20,
            Self::D => 24,
        }
    }

    /// Number of OFDM symbols per transmission super frame.
    pub const fn symbols_per_superframe(self) -> usize {
        self.symbols_per_frame() * FRAMES_PER_SUPERFRAME
    }

    /// Number of symbols at the start of each super frame carrying the SDC.
    pub const fn sdc_symbols(self) -> usize {
        match self {
            Self::A | Self::B => 2,
            Self::C | Self::D => 3,
        }
    }

    /// Carrier spacing 1/Tu in Hz.
    pub fn carrier_spacing(self) -> f64 {
        f64::from(SAMPLE_RATE) / self.fft_size() as f64
    }
}

/// Spectrum occupancy 0..=5 (§8.1, table 83): nominal bandwidths 4.5/5/9/10/18/20 kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpectrumOccupancy(u8);

impl SpectrumOccupancy {
    pub const SO_0: Self = Self(0);
    pub const SO_1: Self = Self(1);
    pub const SO_2: Self = Self(2);
    pub const SO_3: Self = Self(3);
    pub const SO_4: Self = Self(4);
    pub const SO_5: Self = Self(5);
    pub const ALL: [SpectrumOccupancy; 6] =
        [Self::SO_0, Self::SO_1, Self::SO_2, Self::SO_3, Self::SO_4, Self::SO_5];

    pub const fn new(value: u8) -> Option<Self> {
        if value <= 5 { Some(Self(value)) } else { None }
    }

    pub const fn value(self) -> u8 {
        self.0
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub const fn bandwidth_khz(self) -> f64 {
        match self.0 {
            0 => 4.5,
            1 => 5.0,
            2 => 9.0,
            3 => 10.0,
            4 => 18.0,
            _ => 20.0,
        }
    }
}

/// Lowest and highest carrier index (Kmin, Kmax) for a mode/occupancy pair (§8.1,
/// table 84). `None` for combinations the standard does not define (modes C and D
/// only exist with occupancies 3 and 5).
pub const fn carrier_range(mode: RobustnessMode, so: SpectrumOccupancy) -> Option<(i32, i32)> {
    const KMIN: [[i32; 4]; 6] = [
        [2, 1, 0, 0],
        [2, 1, 0, 0],
        [-102, -91, 0, 0],
        [-114, -103, -69, -44],
        [-98, -87, 0, 0],
        [-110, -99, -67, -43],
    ];
    const KMAX: [[i32; 4]; 6] = [
        [102, 91, 0, 0],
        [114, 103, 0, 0],
        [102, 91, 0, 0],
        [114, 103, 69, 44],
        [314, 279, 0, 0],
        [350, 311, 213, 135],
    ];
    let kmin = KMIN[so.index()][mode.index()];
    let kmax = KMAX[so.index()][mode.index()];
    if kmin == 0 && kmax == 0 { None } else { Some((kmin, kmax)) }
}

/// A valid (mode, occupancy) combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelLayout {
    pub mode: RobustnessMode,
    pub occupancy: SpectrumOccupancy,
}

impl ChannelLayout {
    pub fn new(mode: RobustnessMode, occupancy: SpectrumOccupancy) -> Option<Self> {
        carrier_range(mode, occupancy).map(|_| Self { mode, occupancy })
    }

    pub fn carrier_range(self) -> (i32, i32) {
        carrier_range(self.mode, self.occupancy).expect("validated in ChannelLayout::new")
    }

    pub fn num_carriers(self) -> usize {
        let (kmin, kmax) = self.carrier_range();
        (kmax - kmin + 1) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_fills_exactly_400_ms() {
        for m in RobustnessMode::ALL {
            assert_eq!(m.symbol_len() * m.symbols_per_frame(), SAMPLES_PER_FRAME, "mode {m:?}");
        }
    }

    #[test]
    fn guard_lengths_match_spec() {
        let g: Vec<_> = RobustnessMode::ALL.iter().map(|m| m.guard_len()).collect();
        assert_eq!(g, [128, 256, 256, 352]);
    }

    #[test]
    fn mode_b_so3_is_the_10khz_broadcast_layout() {
        let map = ChannelLayout::new(RobustnessMode::B, SpectrumOccupancy::SO_3).unwrap();
        assert_eq!(map.carrier_range(), (-103, 103));
        assert_eq!(map.num_carriers(), 207);
        assert_eq!(RobustnessMode::B.fft_size(), 1024);
        assert_eq!(RobustnessMode::B.symbol_len(), 1280);
        assert_eq!(RobustnessMode::B.symbols_per_frame(), 15);
    }
}
