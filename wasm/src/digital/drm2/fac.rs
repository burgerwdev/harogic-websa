//! Fast Access Channel content (ES 201 980 §6.3): the channel parameters of the
//! transmission plus one service's parameters per frame.

use crate::digital::drm2::fec::crc::Crc;
use crate::digital::drm2::params::SpectrumOccupancy;

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
    /// Frame index within the super frame (0..=2).
    pub frame_index: u8,
    pub afs_valid: bool,
    pub occupancy: SpectrumOccupancy,
    pub interleaving: Interleaving,
    pub msc_mode: MscMode,
    pub sdc_mode: SdcMode,
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
    /// Parse a decoded FAC block (72 bits, one bit per byte). `None` on CRC failure or
    /// an invalid field.
    pub fn parse(bits: &[u8]) -> Option<Self> {
        if bits.len() < 72 {
            return None;
        }
        let mut crc = Crc::crc8();
        for &b in &bits[..64] {
            crc.add_bit(b & 1 == 1);
        }
        let rx_crc = {
            let mut t = BitReader::new(&bits[64..72]);
            t.read(8)
        };
        if crc.value() != rx_crc {
            return None;
        }

        let mut r = BitReader::new(bits);
        let enhancement = r.read(1) == 1;
        let identity = r.read(2) as u8;
        let occupancy = SpectrumOccupancy::new(r.read(4) as u8)?;
        let interleaving = if r.read(1) == 0 { Interleaving::Long } else { Interleaving::Short };
        let msc_mode = MscMode::from_bits(r.read(2));
        let sdc_mode = if r.read(1) == 0 { SdcMode::Qam16 } else { SdcMode::Qam4 };
        let (num_audio, num_data) = decode_num_services(r.read(4))?;
        let reconfiguration_index = r.read(3) as u8;
        let toggle = r.read(1) == 1;
        let _rfu = r.read(1);
        let service_id = r.read(24);
        let short_id = r.read(2) as u8;
        let audio_ca = r.read(1) == 1;
        let language = r.read(4) as u8;
        let is_data = r.read(1) == 1;
        let descriptor = r.read(5) as u8;
        let data_ca = r.read(1) == 1;

        Some(Fac {
            channel: ChannelParams {
                enhancement,
                frame_index: if identity == 3 { 0 } else { identity },
                afs_valid: identity == 0,
                occupancy,
                interleaving,
                msc_mode,
                sdc_mode,
                num_audio,
                num_data,
                reconfiguration_index,
                toggle,
            },
            service: ServiceParams { service_id, short_id, audio_ca, language, is_data, descriptor, data_ca },
        })
    }
}
