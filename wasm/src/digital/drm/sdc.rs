//! Service Description Channel content (ES 201 980 §6.4): the SDC block layout
//! (AFS index + data field + CRC-16) and the data entities this receiver reads —
//! the multiplex description, station label and audio information.

use crate::digital::drm::fec::crc::Crc;

/// A decoded SDC block.
#[derive(Debug, Clone)]
pub struct SdcBlock {
    pub afs_index: u8,
    pub data: Vec<u8>,
    pub crc_ok: bool,
}

/// Parse a decoded SDC block (one bit per byte): AFS index (4 bits), data field
/// (`(len − 20)/8` bytes), CRC-16.
pub fn parse_sdc_block(bits: &[u8]) -> Option<SdcBlock> {
    if bits.len() < 20 {
        return None;
    }
    let data_bytes = (bits.len() - 20) / 8;
    let mut r = BitReader::new(bits);
    let afs_index = r.read(4) as u8;
    let data: Vec<u8> = (0..data_bytes).map(|_| r.read_byte()).collect();
    let rx_crc = r.read(16) as u16;
    let mut crc = Crc::crc16();
    crc.add_byte(afs_index);
    crc.add_bytes(&data);
    let crc_ok = crc.value() as u16 == rx_crc;
    Some(SdcBlock { afs_index, data, crc_ok })
}

/// A reader over one-bit-per-byte bits (MSB first), with byte access.
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

    fn read_byte(&mut self) -> u8 {
        self.read(8) as u8
    }

    fn read_bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.read_byte()).collect()
    }
}

// ---------------------------------------------------------------------------------
// Data entities
// ---------------------------------------------------------------------------------

/// One stream's lengths in the multiplex description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamDescription {
    pub len_a: u16,
    pub len_b: u16,
}

/// Multiplex description data entity — type 0 (§6.4.3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiplexDescription {
    pub protection_a: u8,
    pub protection_b: u8,
    pub streams: Vec<StreamDescription>,
}

/// Station label data entity — type 1 (§6.4.3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub short_id: u8,
    pub bytes: Vec<u8>,
}

impl Label {
    /// Label text (lossy UTF-8, without an optional text-control byte or padding).
    pub fn text(&self) -> String {
        let start = usize::from(matches!(self.bytes.first(), Some(&b) if (1..=0x0F).contains(&b)));
        let s = String::from_utf8_lossy(&self.bytes[start..]);
        s.trim_end_matches(['\0', ' ']).to_string()
    }
}

/// Audio information data entity — type 9 (§6.4.3.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInfo {
    pub short_id: u8,
    pub stream_id: u8,
    /// 0 AAC, 1 reserved (Opus), 2 reserved (DAC), 3 xHE-AAC.
    pub coding: u8,
    pub sbr: bool,
    /// 0 mono, 1 parametric stereo, 2 stereo.
    pub mode: u8,
    pub sample_rate: u8,
    pub text: bool,
    pub enhancement: bool,
    pub coder_field: u8,
}

impl AudioInfo {
    /// The type-9 bytes FDK-AAC's `aacDecoder_ConfigRaw` expects for `TT_DRM` (Dream's
    /// `CAudioParam::getType9Bytes()`): coding/sbr/mode/rate in the first byte, text /
    /// enhancement / coder field in the second.
    pub fn to_type9_bytes(&self) -> Vec<u8> {
        let b0 = (self.coding << 6) | (u8::from(self.sbr) << 5) | (self.mode << 3) | (self.sample_rate & 7);
        let b1 = (u8::from(self.text) << 7) | (u8::from(self.enhancement) << 6) | (self.coder_field << 1);
        vec![b0, b1]
    }
}

/// Language/country data entity — type 12 (§6.4.3.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageCountry {
    pub short_id: u8,
    pub language: [u8; 3],
    pub country: [u8; 2],
}

/// A data entity, decoded for the types this receiver reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entity {
    Multiplex(MultiplexDescription),
    Label(Label),
    Audio(AudioInfo),
    LanguageCountry(LanguageCountry),
    /// Any other (or invalid) entity, skipped by its length.
    Skipped,
}

/// Parse the SDC data field (between AFS index and CRC) into entities, stopping at
/// the zero padding.
pub fn parse_entities(data: &[u8]) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 2 <= data.len() {
        let length = usize::from(data[p] >> 1);
        let version = data[p] & 1 == 1;
        let entity_type = data[p + 1] >> 4;
        let first_nibble = data[p + 1] & 0x0F;
        if length == 0 && !version && entity_type == 0 {
            break; // padding
        }
        let end = p + 2 + length;
        if end > data.len() {
            break;
        }
        let bytes = &data[p + 2..end];
        let body = body_bits(first_nibble, bytes);
        let mut r = BitReader::new(&body);
        let entity = match entity_type {
            0 => parse_multiplex(&mut r, length).map(Entity::Multiplex).unwrap_or(Entity::Skipped),
            1 => parse_label(&mut r, length).map(Entity::Label).unwrap_or(Entity::Skipped),
            9 => parse_audio(&mut r, length).map(Entity::Audio).unwrap_or(Entity::Skipped),
            12 => parse_language(&mut r).map(Entity::LanguageCountry).unwrap_or(Entity::Skipped),
            _ => Entity::Skipped,
        };
        out.push(entity);
        p = end;
    }
    out
}

/// Body bits (one per byte): the 4-bit first nibble then the body bytes, MSB first.
fn body_bits(first_nibble: u8, bytes: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + 8 * bytes.len());
    for i in (0..4).rev() {
        v.push((first_nibble >> i) & 1);
    }
    for &b in bytes {
        for i in (0..8).rev() {
            v.push((b >> i) & 1);
        }
    }
    v
}

fn parse_multiplex(r: &mut BitReader, len: usize) -> Option<MultiplexDescription> {
    if len == 0 || len % 3 != 0 || len / 3 > 4 {
        return None;
    }
    let protection_a = r.read(2) as u8;
    let protection_b = r.read(2) as u8;
    let streams = (0..len / 3)
        .map(|_| StreamDescription { len_a: r.read(12) as u16, len_b: r.read(12) as u16 })
        .collect();
    Some(MultiplexDescription { protection_a, protection_b, streams })
}

fn parse_label(r: &mut BitReader, len: usize) -> Option<Label> {
    if len == 0 || len > 64 {
        return None;
    }
    let short_id = r.read(2) as u8;
    if r.read(2) != 0 {
        return None;
    }
    let bytes = r.read_bytes(len);
    Some(Label { short_id, bytes })
}

fn parse_audio(r: &mut BitReader, _len: usize) -> Option<AudioInfo> {
    let audio = AudioInfo {
        short_id: r.read(2) as u8,
        stream_id: r.read(2) as u8,
        coding: r.read(2) as u8,
        sbr: r.read(1) == 1,
        mode: r.read(2) as u8,
        sample_rate: r.read(3) as u8,
        text: r.read(1) == 1,
        enhancement: r.read(1) == 1,
        coder_field: r.read(5) as u8,
    };
    // The remaining fields (rfa bit and codec config) are not needed.
    Some(audio)
}

fn parse_language(r: &mut BitReader) -> Option<LanguageCountry> {
    let short_id = r.read(2) as u8;
    if r.read(2) != 0 {
        return None;
    }
    let language = [r.read_byte(), r.read_byte(), r.read_byte()];
    let country = [r.read_byte(), r.read_byte()];
    Some(LanguageCountry { short_id, language, country })
}
