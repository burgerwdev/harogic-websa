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

// ---------------------------------------------------------------------------------
// xHE-AAC (MPEG-D USAC) audio super frames
// ---------------------------------------------------------------------------------

/// Largest xHE-AAC audio frame: the 6144-bit bit reservoir per channel for stereo
/// (ES 201 980 §5.3.1.3), plus the trailing 16-bit frame CRC.
pub const XHE_AAC_MAX_FRAME_BYTES: usize = 2 * 6144 / 8 + 2;

/// Header of an xHE-AAC audio super frame (§5.3.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XheHeader {
    /// Audio frames whose border starts in this super frame.
    pub frame_border_count: u8,
    /// Bit-reservoir level (4 bits).
    pub bit_reservoir_level: u8,
    /// Whether the one-byte header CRC passed.
    pub header_crc_ok: bool,
}

/// One xHE-AAC audio frame: the USAC access unit followed by its two-byte CRC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XheFrame {
    pub data: Vec<u8>,
    pub crc_ok: bool,
}

impl XheFrame {
    /// The USAC access unit (the frame data without the trailing CRC-16).
    pub fn access_unit(&self) -> &[u8] {
        &self.data[..self.data.len().saturating_sub(2)]
    }
}

/// Check the audio frame CRC of an xHE-AAC frame (§5.3.1.2).
pub fn xhe_frame_crc_ok(frame: &[u8]) -> bool {
    frame.len() >= 2 && {
        let (au, crc) = frame.split_at(frame.len() - 2);
        crate::digital::drm::fec::crc::crc16(au) == u16::from_be_bytes([crc[0], crc[1]])
    }
}

/// Stateful xHE-AAC audio super frame parser (ES 201 980 §5.3.1). Unlike AAC, xHE-AAC has no
/// fixed frame count: a super frame's directory lists the borders of the frames that START in
/// it, and a frame may span several super frames, so the tail of a frame is carried over.
///
/// This is a clean-room port of the same state machine Dream's `XHEAACSuperFrame` and
/// DecDRM's `XheAacDeframer` implement, with two deliberate differences: the partial frame
/// running when reception starts is dropped (not passed on incomplete), and any error resets
/// the carried-over bytes.
#[derive(Debug, Clone, Default)]
pub struct XheAacDeframer {
    /// Payload bytes since the last frame border (the start of the frame in progress).
    pending: Vec<u8>,
    /// Whether `pending` starts at a frame border.
    synced: bool,
}

impl XheAacDeframer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop the carried-over bytes (after loss of reception or a reconfiguration).
    pub fn reset(&mut self) {
        self.pending.clear();
        self.synced = false;
    }

    /// Parse one audio super frame (the logical frame without the text message bytes) and
    /// return the frames it completed. `None` means the super frame was malformed; the state
    /// is reset so the next one starts clean.
    pub fn push(&mut self, sf: &[u8]) -> Option<(XheHeader, Vec<XheFrame>)> {
        match self.push_inner(sf) {
            Some(r) => Some(r),
            None => {
                self.reset();
                None
            }
        }
    }

    fn push_inner(&mut self, sf: &[u8]) -> Option<(XheHeader, Vec<XheFrame>)> {
        if sf.len() < 2 {
            return None;
        }
        let header_crc_ok = crate::digital::drm::fec::crc::crc8(&sf[..1]) == sf[1];
        // Without a good header CRC the count is repeated in the last directory byte.
        let count = if header_crc_ok { sf[0] >> 4 } else { sf[sf.len() - 1] & 0x0F };
        let header = XheHeader {
            frame_border_count: count,
            bit_reservoir_level: sf[0] & 0x0F,
            header_crc_ok,
        };
        let n = count as usize;
        // The directory is `n` two-byte entries at the end, after the two header bytes.
        let dir_start = sf.len().checked_sub(2 * n).filter(|&d| d >= 2)?;
        // Entry j describes border n − 1 − j; its low nibble repeats the frame count.
        let mut index = vec![0usize; n];
        for j in 0..n {
            let e = u16::from_be_bytes([sf[dir_start + 2 * j], sf[dir_start + 2 * j + 1]]);
            if e & 0x0F != u16::from(count) {
                return None;
            }
            index[n - 1 - j] = usize::from(e >> 4);
        }
        let payload = &sf[2..dir_start];
        let start = self.pending.len();
        let mut borders = Vec::with_capacity(n);
        for (i, &x) in index.iter().enumerate() {
            let pos = match x {
                // The special values 0xFFE/0xFFF are borders relative to the received payload
                // start (1 = first payload byte, 2 = second).
                0xFFE | 0xFFF if i == 0 => {
                    let back = if x == 0xFFE { 2 } else { 1 };
                    match start.checked_sub(back) {
                        Some(p) => p,
                        None if !self.synced => continue,
                        None => return None,
                    }
                }
                _ if x < payload.len() => start + x,
                _ => return None,
            };
            if borders.last().is_some_and(|&b| pos <= b) {
                return None;
            }
            borders.push(pos);
        }
        self.pending.extend_from_slice(payload);
        let mut frames = Vec::with_capacity(borders.len());
        let mut prev = 0;
        for &b in &borders {
            if self.synced {
                let data = self.pending[prev..b].to_vec();
                let crc_ok = xhe_frame_crc_ok(&data);
                frames.push(XheFrame { data, crc_ok });
            }
            // The first border ends the partial frame running before the download started.
            self.synced = true;
            prev = b;
        }
        self.pending.drain(..prev);
        if !self.synced {
            // Only the last two bytes can matter (a delayed 0xFFE/0xFFF border).
            let keep = self.pending.len().saturating_sub(2);
            self.pending.drain(..keep);
        } else if self.pending.len() > 4 * XHE_AAC_MAX_FRAME_BYTES {
            return None;
        }
        Some((header, frames))
    }
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

    /// The xHE-AAC deframer's framing: a frame's end is signalled by the NEXT super frame's
    /// first border, so the first super frame yields no complete frame and the second releases
    /// the first one. The header CRC and the frame CRC-16 must both pass.
    #[test]
    fn xhe_super_frames_deframe() {
        use crate::digital::drm::fec::crc::{crc16, crc8};
        let au = vec![0xA1u8; 30];
        let mut frame = au.clone();
        frame.extend_from_slice(&crc16(&au).to_be_bytes());
        let build = |payload: &[u8], border: usize, count: u8| {
            let mut sf = vec![count << 4, 0];
            sf[1] = crc8(&sf[..1]);
            sf.extend_from_slice(payload);
            let entry = ((border as u16) << 4) | u16::from(count);
            sf.extend_from_slice(&entry.to_be_bytes());
            sf
        };
        let mut def = XheAacDeframer::new();
        let (h1, f1) = def.push(&build(&frame, 0, 1)).expect("header parses");
        assert_eq!(h1.frame_border_count, 1);
        assert!(h1.header_crc_ok);
        assert!(f1.is_empty(), "the first border carries no complete frame yet");
        let (_h2, f2) = def.push(&build(&frame, 0, 1)).expect("header parses");
        assert_eq!(f2.len(), 1);
        assert_eq!(f2[0].data, frame);
        assert!(f2[0].crc_ok, "the frame CRC-16 must pass");
        assert_eq!(f2[0].access_unit(), &au, "the USAC access unit drops the CRC-16");
    }
}
