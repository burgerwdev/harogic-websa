//! CRCs used by DRM (ES 201 980 annex D): CRC-8 (FAC, audio super frame headers) and
//! CRC-16/CCITT (SDC blocks, data packets). Register preset to all ones, result the
//! ones' complement, bits processed MSB first.

/// Incremental bit-level CRC.
#[derive(Debug, Clone)]
pub struct Crc {
    degree: u32,
    poly: u32,
    reg: u32,
}

impl Crc {
    pub fn crc8() -> Self {
        Self::with_poly(8, 0x1D)
    }

    pub fn crc16() -> Self {
        Self::with_poly(16, 0x1021)
    }

    /// `poly` includes the x⁰ term but not the xⁿ term.
    pub fn with_poly(degree: u32, poly: u32) -> Self {
        Self { degree, poly, reg: (1u32 << degree) - 1 }
    }

    pub fn add_bit(&mut self, bit: bool) {
        let top = (self.reg >> (self.degree - 1)) & 1 == 1;
        self.reg = (self.reg << 1) & ((1u32 << self.degree) - 1);
        if top ^ bit {
            self.reg ^= self.poly;
        }
    }

    pub fn add_byte(&mut self, byte: u8) {
        for i in (0..8).rev() {
            self.add_bit((byte >> i) & 1 == 1);
        }
    }

    pub fn add_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.add_byte(b);
        }
    }

    pub fn value(&self) -> u32 {
        !self.reg & ((1u32 << self.degree) - 1)
    }
}

/// CRC-16/CCITT over whole bytes.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut c = Crc::crc16();
    c.add_bytes(bytes);
    c.value() as u16
}

/// CRC-8 over whole bytes (the audio super frame header check).
pub fn crc8(bytes: &[u8]) -> u8 {
    let mut c = Crc::crc8();
    c.add_bytes(bytes);
    c.value() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_ccitt_check_value() {
        // CRC-16/GENIBUS of "123456789".
        assert_eq!(crc16(b"123456789"), 0xD64E);
    }
}
