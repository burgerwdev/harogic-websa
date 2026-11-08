//! WBIQ0001 experiment; available only with the `iq-experiments` feature.
use super::{invalid, json, parse, IqBlockMeta, MAX_BLOCK_BYTES};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Arc;

const MAGIC: &[u8; 8] = b"WBIQ0001";
const MAX_META: usize = 65_536;

/// Immutable encoded samples retained by the archive experiment.
#[derive(Clone)]
pub struct SharedIqBlock {
    pub meta: IqBlockMeta,
    pub cf32_le: Arc<[u8]>,
}
impl SharedIqBlock {
    pub fn new(meta: IqBlockMeta, bytes: Arc<[u8]>) -> io::Result<Self> {
        meta.validate(bytes.len())?;
        Ok(Self {
            meta,
            cf32_le: bytes,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BlockIndex {
    pub offset: u64,
    pub meta: IqBlockMeta,
    pub codec: String,
    pub raw_bytes: u32,
    pub stored_bytes: u32,
}
#[derive(Serialize, Deserialize)]
struct Header {
    meta: IqBlockMeta,
    codec: String,
    checksum: String,
}
fn checksum(bytes: &[u8]) -> String {
    // Original v1 checksum, retained for experiment compatibility. Production v2 uses CRC32.
    format!(
        "{:016x}",
        bytes
            .iter()
            .fold(0xcbf29ce484222325u64, |v, b| (v ^ *b as u64)
                .wrapping_mul(0x100000001b3))
    )
}
pub struct ArchiveWriter<W> {
    output: W,
    index: Vec<BlockIndex>,
    level: i32,
}
impl<W: Write + Seek> ArchiveWriter<W> {
    pub fn new(mut output: W, level: i32) -> io::Result<Self> {
        if !(1..=3).contains(&level) {
            return Err(invalid("experiment supports zstd levels 1..3"));
        }
        output.write_all(MAGIC)?;
        Ok(Self {
            output,
            index: Vec::new(),
            level,
        })
    }
    pub fn append(&mut self, block: &SharedIqBlock) -> io::Result<BlockIndex> {
        block.meta.validate(block.cf32_le.len())?;
        let compressed = zstd::bulk::compress(&block.cf32_le, self.level)?;
        let (codec, payload) = if compressed.len() < block.cf32_le.len() {
            ("zstd", compressed.as_slice())
        } else {
            ("raw", block.cf32_le.as_ref())
        };
        let header = Header {
            meta: block.meta.clone(),
            codec: codec.into(),
            checksum: checksum(&block.cf32_le),
        };
        let offset = self.record(b"BLK1", &json(&header)?, payload)?;
        let item = BlockIndex {
            offset,
            meta: block.meta.clone(),
            codec: codec.into(),
            raw_bytes: block.cf32_le.len() as u32,
            stored_bytes: payload.len() as u32,
        };
        self.index.push(item.clone());
        Ok(item)
    }
    fn record(&mut self, kind: &[u8; 4], meta: &[u8], payload: &[u8]) -> io::Result<u64> {
        if meta.len() > MAX_META || payload.len() > MAX_BLOCK_BYTES {
            return Err(invalid("record exceeds bound"));
        }
        let offset = self.output.stream_position()?;
        self.output.write_all(kind)?;
        self.output.write_all(&(meta.len() as u32).to_le_bytes())?;
        self.output
            .write_all(&(payload.len() as u32).to_le_bytes())?;
        self.output.write_all(meta)?;
        self.output.write_all(payload)?;
        Ok(offset)
    }
    /// Consumes the writer: no append after stop. Flushes the index before reporting completion.
    pub fn finish(mut self) -> io::Result<W> {
        self.record(b"IDX1", &json(&self.index)?, &[])?;
        self.output.flush()?;
        Ok(self.output)
    }
}

pub struct ArchiveScan {
    pub blocks: Vec<SharedIqBlock>,
    pub index: Vec<BlockIndex>,
    pub complete: bool,
}
fn decode(header: Header, payload: &[u8]) -> io::Result<SharedIqBlock> {
    let raw_len = header.meta.samples as usize * 8;
    header.meta.validate(raw_len)?;
    let bytes = match header.codec.as_str() {
        "raw" => payload.to_vec(),
        "zstd" => zstd::bulk::decompress(payload, raw_len)?,
        _ => return Err(invalid("unknown block codec")),
    };
    if bytes.len() != raw_len || checksum(&bytes) != header.checksum {
        return Err(invalid("block size/checksum mismatch"));
    }
    SharedIqBlock::new(header.meta, bytes.into())
}
/// Seek using an index produced by a validated scan; validate the target record again.
pub fn read_block_at<R: Read + Seek>(
    input: &mut R,
    index: &BlockIndex,
) -> io::Result<SharedIqBlock> {
    input.seek(SeekFrom::Start(index.offset))?;
    let mut prefix = [0; 12];
    input.read_exact(&mut prefix)?;
    let meta_len = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;
    let stored = u32::from_le_bytes(prefix[8..12].try_into().unwrap()) as usize;
    if &prefix[..4] != b"BLK1"
        || meta_len > MAX_META
        || stored > MAX_BLOCK_BYTES
        || stored != index.stored_bytes as usize
    {
        return Err(invalid("invalid indexed block"));
    }
    let mut meta = vec![0; meta_len];
    let mut payload = vec![0; stored];
    input.read_exact(&mut meta)?;
    input.read_exact(&mut payload)?;
    let header: Header = parse(&meta)?;
    if header.meta != index.meta || header.codec != index.codec {
        return Err(invalid("index points at different metadata"));
    }
    let block = decode(header, &payload)?;
    if block.cf32_le.len() != index.raw_bytes as usize {
        return Err(invalid("index raw size mismatch"));
    }
    Ok(block)
}
/// Scan complete blocks; partial final record is recoverable, corruption is an explicit error.
/// Retains decoded blocks for this bounded experiment; service v2 uses streaming reads.
pub fn scan<R: Read + Seek>(mut input: R) -> io::Result<ArchiveScan> {
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid("unknown archive version"));
    }
    let mut result = ArchiveScan {
        blocks: Vec::new(),
        index: Vec::new(),
        complete: false,
    };
    loop {
        let offset = input.stream_position()?;
        let mut prefix = [0; 12];
        if let Err(e) = input.read_exact(&mut prefix) {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                return Ok(result);
            }
            return Err(e);
        }
        let meta_len = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;
        let stored = u32::from_le_bytes(prefix[8..12].try_into().unwrap()) as usize;
        if meta_len > MAX_META || stored > MAX_BLOCK_BYTES {
            return Err(invalid("record exceeds bound"));
        }
        let mut meta = vec![0; meta_len];
        let mut payload = vec![0; stored];
        if let Err(e) = input
            .read_exact(&mut meta)
            .and_then(|_| input.read_exact(&mut payload))
        {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                return Ok(result);
            }
            return Err(e);
        }
        match &prefix[..4] {
            b"BLK1" => {
                let header: Header = parse(&meta)?;
                let codec = header.codec.clone();
                let block = decode(header, &payload)?;
                result.index.push(BlockIndex {
                    offset,
                    meta: block.meta.clone(),
                    codec,
                    raw_bytes: block.cf32_le.len() as u32,
                    stored_bytes: stored as u32,
                });
                result.blocks.push(block);
            }
            b"IDX1" => {
                let persisted: Vec<BlockIndex> = parse(&meta)?;
                if stored != 0 || persisted != result.index {
                    return Err(invalid("index mismatch"));
                }
                let mut trailing = [0];
                if input.read(&mut trailing)? != 0 {
                    return Err(invalid("bytes after final index"));
                }
                result.complete = true;
                return Ok(result);
            }
            _ => return Err(invalid("unknown record type")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn meta(samples: u32) -> IqBlockMeta {
        IqBlockMeta {
            receiver_uid: "r-stable".into(),
            source_id: "fixture".into(),
            track_id: "track".into(),
            epoch: "1".into(),
            channel_id: "2".into(),
            config_revision: "3".into(),
            first_input_index: "9007199254740993".into(),
            first_output_index: "0".into(),
            fraction_num: "1".into(),
            fraction_den: "3".into(),
            samples,
            input_rate_hz: 62_500_000.0,
            actual_rate_hz: 48_840.130763116205,
            sample_format: "cf32_le".into(),
            amplitude_scale: 1.0,
            delay_seconds: None,
        }
    }
    #[test]
    fn byte_exact_special_float_bits_real_capture_mixed_codecs_and_stop() {
        let special: Vec<u8> = [
            0u32, 0x80000000, 0x7fc01234, 0x7f800000, 0xff800000, 1, 0x3f800001, 0xffc01234,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        let real = include_bytes!("../../../tests/fixtures/ft8/real_slot_ja0txu.iq");
        let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), 1).unwrap();
        let mut expected = vec![special, vec![0; 8192]];
        expected.extend(real.chunks(MAX_BLOCK_BYTES).map(|b| b.to_vec()));
        let mut indices = Vec::new();
        for bytes in &expected {
            indices.push(
                writer
                    .append(
                        &SharedIqBlock::new(meta((bytes.len() / 8) as u32), bytes.clone().into())
                            .unwrap(),
                    )
                    .unwrap(),
            );
        }
        assert!(indices.iter().any(|i| i.codec == "raw"));
        assert!(indices.iter().any(|i| i.codec == "zstd"));
        let file = writer.finish().unwrap().into_inner();
        let result = scan(Cursor::new(&file)).unwrap();
        assert!(result.complete);
        assert_eq!(result.index, indices);
        for index in indices.iter().rev() {
            let block = read_block_at(&mut Cursor::new(&file), index).unwrap();
            assert_eq!(block.meta, index.meta);
        }
        for (block, original) in result.blocks.iter().zip(expected) {
            assert_eq!(block.cf32_le.as_ref(), original);
            assert_eq!(block.meta, meta((original.len() / 8) as u32));
        }
        // Partial last block recovers every earlier block, never fabricates a complete recording.
        let last = result.index.last().unwrap().offset as usize;
        let recovered = scan(Cursor::new(&file[..last + 20])).unwrap();
        assert!(!recovered.complete);
        assert_eq!(recovered.blocks.len(), result.blocks.len() - 1);
        let metadata_bytes =
            u32::from_le_bytes(file[last + 4..last + 8].try_into().unwrap()) as usize;
        let footer = last + 12 + metadata_bytes + indices.last().unwrap().stored_bytes as usize;
        let no_index = scan(Cursor::new(&file[..footer])).unwrap();
        assert!(!no_index.complete);
        assert_eq!(no_index.blocks.len(), indices.len());
        let payload_offset = indices[0].offset as usize
            + 12
            + u32::from_le_bytes(file[12..16].try_into().unwrap()) as usize;
        let mut corrupt_payload = file.clone();
        corrupt_payload[payload_offset] ^= 1;
        assert!(scan(Cursor::new(corrupt_payload))
            .err()
            .unwrap()
            .to_string()
            .contains("checksum"));
        let mut corrupt = file;
        corrupt[indices[0].offset as usize + 12] ^= 0xff;
        assert!(scan(Cursor::new(corrupt)).is_err());
    }
    #[test]
    fn malformed_clocks_and_unbounded_blocks_rejected() {
        let mut m = meta(1);
        m.fraction_den = "0".into();
        assert!(SharedIqBlock::new(m, vec![0; 8].into()).is_err());
        assert!(SharedIqBlock::new(meta(1), vec![0; 9].into()).is_err());
        let mut bytes = MAGIC.to_vec();
        bytes.extend(b"BLK1");
        bytes.extend(u32::MAX.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        assert!(scan(Cursor::new(bytes)).is_err());
    }
}
