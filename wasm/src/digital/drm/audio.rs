//! MSC demultiplexing (ES 201 980 §6.2.3) and AAC audio super-frame parsing (§5.2.1),
//! ported from the previous receiver (MIT, project's own code) onto the drm SDC types.
//! This is the layer between a decoded MSC multiplex frame and the audio codec: it splits
//! the frame into logical streams and turns an audio stream's logical frames into AAC
//! access units ready for the decoder.

use crate::digital::drm::sdc::{MultiplexDescription, StreamDescription};

/// Bytes of the DRM text message carried at the end of an audio logical frame when the
/// SDC audio descriptor sets its text flag (ES 201 980 §5.2.1).
pub const TEXT_MESSAGE_BYTES: usize = 4;

/// The audio super frame part of a logical frame: the text message, when the stream
/// carries one, is the LAST four bytes and is NOT part of the super frame.
pub fn split_text_message(frame: &[u8], text_flag: bool) -> &[u8] {
    if !text_flag || frame.len() < TEXT_MESSAGE_BYTES {
        return frame;
    }
    &frame[..frame.len() - TEXT_MESSAGE_BYTES]
}

/// One stream's data of one multiplex frame: part A bytes then part B bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalFrame {
    pub stream_id: u8,
    pub data: Vec<u8>,
    pub part_a_len: usize,
}

/// Split a decoded multiplex frame (one bit per byte) into logical frames, one entry
/// per stream of the multiplex description. A stream that does not fit is `None`.
pub fn demultiplex(main: &[u8], mux: &MultiplexDescription) -> Vec<Option<LogicalFrame>> {
    let total_a: usize = mux.streams.iter().map(|s| 8 * s.len_a as usize).sum();
    let mut off_a = 0usize;
    let mut off_b = total_a;
    let mut out = Vec::with_capacity(mux.streams.len());
    for (i, s) in mux.streams.iter().enumerate() {
        let len_a = 8 * s.len_a as usize;
        let len_b = 8 * s.len_b as usize;
        let a = main.get(off_a..off_a + len_a);
        let b = main.get(off_b..off_b + len_b);
        match (a, b) {
            (Some(a), Some(b)) => {
                let mut data = pack(a);
                data.extend(pack(b));
                out.push(Some(LogicalFrame { stream_id: i as u8, data, part_a_len: s.len_a as usize }));
            }
            _ => out.push(None),
        }
        off_a += len_a;
        off_b += len_b;
    }
    out
}

/// Pack one-bit-per-byte bits into bytes, MSB first.
fn pack(bits: &[u8]) -> Vec<u8> {
    bits.chunks(8)
        .map(|c| c.iter().fold(0u8, |a, b| (a << 1) | (b & 1)))
        .collect()
}

/// One AAC access unit: the core data and the CRC byte the super frame carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub data: Vec<u8>,
    pub crc_byte: Option<u8>,
}

/// The framing of one AAC audio stream.
#[derive(Debug, Clone)]
pub struct AacSuperFrameFormat {
    pub num_frames: usize,
    num_borders: usize,
    higher_protected_bytes: usize,
}

impl AacSuperFrameFormat {
    /// AAC with `num_frames` (5 or 10) frames in a stream of the given lengths.
    pub fn aac(num_frames: usize, stream: &StreamDescription) -> Self {
        let num_borders = num_frames.saturating_sub(1);
        let header = header_bytes(num_borders);
        let hp = if stream.len_a > 0 && stream.len_b > 0 && num_frames > 0 {
            (stream.len_a as usize).saturating_sub(header + num_frames) / num_frames
        } else {
            0
        };
        Self { num_frames, num_borders, higher_protected_bytes: hp }
    }

    pub fn header_bytes(&self) -> usize {
        header_bytes(self.num_borders)
    }

    /// Bytes available for the frames in a super frame of `super_frame_len` bytes.
    pub fn payload_len(&self, super_frame_len: usize) -> Option<usize> {
        super_frame_len.checked_sub(self.header_bytes() + self.num_frames)
    }
}

fn header_bytes(num_borders: usize) -> usize {
    (12 * num_borders).div_ceil(8)
}

/// Read the `k`-th 12-bit value of a byte slice (MSB first).
fn read12(bytes: &[u8], k: usize) -> usize {
    let bit = 12 * k;
    let b = |i: usize| usize::from(bytes.get(i).copied().unwrap_or(0));
    let v = (b(bit / 8) << 16) | (b(bit / 8 + 1) << 8) | b(bit / 8 + 2);
    (v >> (12 - bit % 8)) & 0xFFF
}

/// Split an AAC audio super frame into its access units (without CRC checking — the
/// AAC frame CRC is not used by the reference receiver either).
pub fn parse_aac_super_frame(sf: &[u8], fmt: &AacSuperFrameFormat) -> Option<Vec<AudioFrame>> {
    let n = fmt.num_frames;
    if n == 0 {
        return None;
    }
    let payload = fmt.payload_len(sf.len())?;
    let implicit_last = fmt.num_borders < n;
    let mut lengths = Vec::with_capacity(n);
    let mut prev = 0usize;
    for k in 0..fmt.num_borders.min(n) {
        let mut border = read12(sf, k);
        if border < prev {
            border += 4096; // borders above 4095 are sent modulo 4096
        }
        if border < prev || (implicit_last && border >= payload) || (!implicit_last && border > payload) {
            return None;
        }
        lengths.push(border - prev);
        prev = border;
    }
    if implicit_last {
        lengths.push(payload - prev);
    }
    let hp = fmt.higher_protected_bytes;
    if lengths.iter().any(|&l| l < hp) {
        return None;
    }
    let mut pos = fmt.header_bytes();
    let mut frames = Vec::with_capacity(n);
    for _ in 0..n {
        let data = sf[pos..pos + hp].to_vec();
        pos += hp;
        let crc = sf.get(pos).copied();
        pos += 1;
        frames.push(AudioFrame { data, crc_byte: crc });
    }
    for (f, &len) in frames.iter_mut().zip(&lengths) {
        f.data.extend_from_slice(&sf[pos..pos + len - hp]);
        pos += len - hp;
    }
    Some(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The framing split matches the previous receiver's (which pins the full round-trip):
    /// verify the demux splits the known bit layout and the AAC super-frame format geometry.
    #[test]
    fn demux_and_super_frame_geometry() {
        let stream = StreamDescription { len_a: 150, len_b: 300 };
        let mux = MultiplexDescription {
            protection_a: 0,
            protection_b: 1,
            streams: vec![stream.clone()],
        };
        let bits: Vec<u8> = (0..8 * (150 + 300)).map(|i| (i % 2) as u8).collect();
        let logical = demultiplex(&bits, &mux);
        let lf = logical[0].as_ref().expect("the stream must fit");
        assert_eq!(lf.data.len(), 150 + 300, "part A then part B packed");
        assert_eq!(lf.part_a_len, 150);
        let fmt = AacSuperFrameFormat::aac(5, &stream);
        assert_eq!(fmt.header_bytes(), 6, "4 borders x 12 bits");
        // 5 AUs each carry one CRC byte; the payload is the super frame minus header and CRCs.
        assert_eq!(fmt.payload_len(6 + 5 + 5), Some(5));
        // Higher-protected bytes per AU = (len_a - header - num_frames) / num_frames.
        assert_eq!(fmt.payload_len(6 + 5 + 5 + 5 * 27), Some(5 + 5 * 27));
    }
}
