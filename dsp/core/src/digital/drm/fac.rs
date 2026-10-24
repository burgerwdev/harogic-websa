//! Fast Access Channel content (ES 201 980 §6.3): the channel parameters of the
//! transmission plus one service's parameters per frame.

use crate::digital::drm::fec::crc::Crc;
use crate::digital::drm::params::{RobustnessMode, SpectrumOccupancy};

/// Interleaver depth of the MSC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interleaving {
    Long,
    Short,
}

/// MSC constellation (FAC "MSC mode" field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MscMode {
    Qam64Sm,
    Qam64HmMix,
    Qam64HmSym,
    Qam16Sm,
    Qam4,
}

impl MscMode {
    pub fn from_bits(v: u32) -> Self {
        match v & 3 {
            0 => Self::Qam64Sm,
            1 => Self::Qam64HmMix,
            2 => Self::Qam64HmSym,
            _ => Self::Qam16Sm,
        }
    }
}

/// SDC constellation (FAC "SDC mode" field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdcMode {
    Qam16,
    Qam4,
}

/// Channel parameters (first 20 bits of the FAC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelParams {
    pub enhancement: bool,
    /// Frame index within the super frame (0..=2 in A-D, 0..=3 in E).
    pub frame_index: u8,
    pub afs_valid: bool,
    pub occupancy: SpectrumOccupancy,
    pub interleaving: Interleaving,
    pub msc_mode: MscMode,
    pub sdc_mode: SdcMode,
    /// Mode E: 4-QAM SDC rate 1/2 (0) or 1/4 (1); DRM30 uses 0 for 16-QAM, 1 for 4-QAM.
    pub sdc_protection: u8,
    pub num_audio: u8,
    pub num_data: u8,
    pub reconfiguration_index: u8,
    pub toggle: bool,
}

/// Service parameters of the service signalled in this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceParams {
    pub service_id: u32,
    pub short_id: u8,
    pub audio_ca: bool,
    pub language: u8,
    pub is_data: bool,
    pub descriptor: u8,
    pub data_ca: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fac {
    pub channel: ChannelParams,
    pub service: ServiceParams,
    /// The second 44-bit service descriptor in a mode E FAC block.
    pub second_service: Option<ServiceParams>,
}

/// Number-of-services code (§6.3.3, table 60): (audio, data) → 4-bit code.
const NUM_SERVICES: [[i32; 5]; 5] = [
    [-1, 1, 2, 3, 15],
    [4, 5, 6, 7, -1],
    [8, 9, 10, -1, -1],
    [12, 13, -1, -1, -1],
    [0, -1, -1, -1, -1],
];

fn decode_num_services(code: u32) -> Option<(u8, u8)> {
    for (a, row) in NUM_SERVICES.iter().enumerate() {
        for (d, &v) in row.iter().enumerate() {
            if v as i32 == code as i32 {
                return Some((a as u8, d as u8));
            }
        }
    }
    None
}

/// A reader over one-bit-per-byte decoded bits (MSB first).
struct BitReader<'a> {
    bits: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(bits: &'a [u8]) -> Self {
        Self { bits, pos: 0 }
    }

    fn read(&mut self, n: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.bits[self.pos] & 1);
            self.pos += 1;
        }
        v
    }
}

impl Fac {
    /// DRM30 FAC (72 bits). Kept for readers that already know their mode is A-D.
    pub fn parse(bits: &[u8]) -> Option<Self> {
        Self::parse_for(RobustnessMode::B, bits)
    }

    /// Parse mode-aware FAC content. E has 116 bits: two 44-bit service descriptors,
    /// a four-zero-bit CRC extension (not transmitted), then eight CRC bits.
    pub fn parse_for(mode: RobustnessMode, bits: &[u8]) -> Option<Self> {
        let drm_plus = mode == RobustnessMode::E;
        let data_bits = if drm_plus { 108 } else { 64 };
        if bits.len() < data_bits + 8 {
            return None;
        }
        let mut crc = Crc::crc8();
        for &b in &bits[..data_bits] {
            crc.add_bit(b & 1 == 1);
        }
        if drm_plus {
            for _ in 0..4 { crc.add_bit(false); }
        }
        let mut crc_reader = BitReader::new(&bits[data_bits..data_bits + 8]);
        if crc.value() != crc_reader.read(8) {
            return None;
        }

        let mut r = BitReader::new(bits);
        let enhancement = r.read(1) == 1;
        let identity = r.read(2) as u8;
        let rm = r.read(1) == 1;
        if rm != drm_plus { return None; }
        let occupancy = SpectrumOccupancy::new(r.read(3) as u8)?;
        if drm_plus && occupancy != SpectrumOccupancy::SO_0 { return None; }
        let interleaving = if r.read(1) == 0 { Interleaving::Long } else { Interleaving::Short };
        if drm_plus && interleaving != Interleaving::Long { return None; }
        let msc_bits = r.read(2);
        let msc_mode = if drm_plus {
            match msc_bits {
                0 => MscMode::Qam16Sm,
                3 => MscMode::Qam4,
                _ => return None,
            }
        } else {
            MscMode::from_bits(msc_bits)
        };
        let sdc_flag = r.read(1) as u8;
        let sdc_mode = if drm_plus || sdc_flag == 1 { SdcMode::Qam4 } else { SdcMode::Qam16 };
        let (num_audio, num_data) = decode_num_services(r.read(4))?;
        let reconfiguration_index = r.read(3) as u8;
        let toggle = r.read(1) == 1;
        let _rfu = r.read(1);
        let frame_index = if drm_plus {
            match (identity, toggle) {
                (0 | 3, false) => 0,
                (1, true) => 1,
                (1, false) => 2,
                (2, true) => 3,
                _ => return None,
            }
        } else if identity == 3 { 0 } else { identity };
        let read_service = |r: &mut BitReader<'_>| -> ServiceParams {
            let service_id = r.read(24);
            let short_id = r.read(2) as u8;
            let audio_ca = r.read(1) == 1;
            let language = r.read(4) as u8;
            let is_data = r.read(1) == 1;
            let descriptor = r.read(5) as u8;
            let data_ca = r.read(1) == 1;
            let _rfa = r.read(6);
            ServiceParams { service_id, short_id, audio_ca, language, is_data, descriptor, data_ca }
        };
        let service = read_service(&mut r);
        let second_service = drm_plus.then(|| read_service(&mut r));

        Some(Fac {
            channel: ChannelParams {
                enhancement,
                frame_index,
                afs_valid: identity == 0,
                occupancy,
                interleaving,
                msc_mode,
                sdc_mode,
                sdc_protection: sdc_flag,
                num_audio,
                num_data,
                reconfiguration_index,
                toggle,
            },
            service,
            second_service,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn push(bits: &mut Vec<u8>, value: u32, width: usize) {
        for i in (0..width).rev() {
            bits.push(((value >> i) & 1) as u8);
        }
    }

    pub(crate) fn mode_e_fac(identity: u32, toggle: u32, msc: u32, sdc: u32) -> Vec<u8> {
        let mut bits = Vec::new();
        push(&mut bits, 0, 1); // base layer
        push(&mut bits, identity, 2);
        push(&mut bits, 1, 1); // RM E
        push(&mut bits, 0, 3); // SO 0: 100 kHz
        push(&mut bits, 0, 1); // 600 ms interleaver
        push(&mut bits, msc, 2);
        push(&mut bits, sdc, 1);
        push(&mut bits, 4, 4); // one audio service
        push(&mut bits, 0, 3); // reconfiguration
        push(&mut bits, toggle, 1);
        push(&mut bits, 0, 1); // reserved
        for short_id in 0..2 {
            push(&mut bits, 0x123456, 24);
            push(&mut bits, short_id, 2);
            push(&mut bits, 0, 1 + 4 + 1 + 5 + 1 + 6);
        }
        assert_eq!(bits.len(), 108);
        let mut crc = Crc::crc8();
        for &b in &bits { crc.add_bit(b == 1); }
        for _ in 0..4 { crc.add_bit(false); }
        push(&mut bits, crc.value(), 8);
        assert_eq!(bits.len(), 116);
        bits
    }

    #[test]
    fn mode_e_fac_has_four_distinct_frame_indices_and_two_services() {
        for (identity, toggle, index) in [(0, 0, 0), (1, 1, 1), (1, 0, 2), (2, 1, 3)] {
            let bits = mode_e_fac(identity, toggle, 3, 1);
            let fac = Fac::parse_for(RobustnessMode::E, &bits).expect("mode E FAC CRC");
            assert_eq!(fac.channel.frame_index, index);
            assert_eq!(fac.channel.msc_mode, MscMode::Qam4);
            assert_eq!(fac.channel.sdc_mode, SdcMode::Qam4);
            assert_eq!(fac.channel.sdc_protection, 1);
            assert_eq!(fac.service.short_id, 0);
            assert_eq!(fac.second_service.unwrap().short_id, 1);
            assert!(Fac::parse(&bits).is_none(), "DRM30 must not accept a mode E FAC");
        }
        let mut bad = mode_e_fac(0, 0, 0, 0);
        assert_eq!(Fac::parse_for(RobustnessMode::E, &bad).unwrap().channel.msc_mode, MscMode::Qam16Sm);
        bad[70] ^= 1;
        assert!(Fac::parse_for(RobustnessMode::E, &bad).is_none());
    }
}
