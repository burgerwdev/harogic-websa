//! WebSA IQ archive v2. Independent CF32 blocks, bounded index pages and a checked footer.
//! Not SigMF. v1 remains an experiment and is deliberately rejected by this reader.
use super::{invalid, json, parse, IqBlockMeta, MAX_BLOCK_BYTES};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Read, Seek, SeekFrom, Write};

pub const MAGIC: &[u8; 8] = b"WBIQ0002";
pub const INDEX_PAGE: usize = 128;
const MAX_META: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub service_id: String,
    pub segment_id: String,
    pub sequence: String,
    pub frequency_hz: f64,
    pub baseband: Value,
    pub clock: IqBlockMeta,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub offset: String,
    pub meta: Meta,
    pub codec: String,
    pub raw_bytes: u32,
    pub stored_bytes: u32,
    pub raw_crc32: u32,
}
#[derive(Serialize, Deserialize)]
struct Header {
    meta: Meta,
    codec: String,
    raw_crc32: u32,
}
#[derive(Serialize, Deserialize)]
struct Footer {
    blocks: String,
    samples: String,
    records_crc32: u32,
}

pub struct Writer<W> {
    output: W,
    page: Vec<Index>,
    blocks: u64,
    samples: u64,
    digest: crc32fast::Hasher,
}
impl<W: Write + Seek> Writer<W> {
    pub fn new(mut output: W) -> io::Result<Self> {
        output.write_all(MAGIC)?;
        Ok(Self {
            output,
            page: Vec::new(),
            blocks: 0,
            samples: 0,
            digest: crc32fast::Hasher::new(),
        })
    }
    fn record(&mut self, kind: &[u8; 4], meta: &[u8], payload: &[u8]) -> io::Result<u64> {
        if meta.len() > MAX_META || payload.len() > MAX_BLOCK_BYTES {
            return Err(invalid("archive record exceeds bounds"));
        }
        let offset = self.output.stream_position()?;
        let mut prefix = Vec::with_capacity(16);
        prefix.extend_from_slice(kind);
        prefix.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        prefix.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let mut hash = crc32fast::Hasher::new();
        hash.update(&prefix);
        hash.update(meta);
        hash.update(payload);
        prefix.extend_from_slice(&hash.finalize().to_le_bytes());
        self.output.write_all(&prefix)?;
        self.output.write_all(meta)?;
        self.output.write_all(payload)?;
        if kind != b"END2" {
            self.digest.update(&prefix[12..]);
        }
        Ok(offset)
    }
    pub fn event(&mut self, event: &Value) -> io::Result<()> {
        self.record(b"EVT2", &json(event)?, &[]).map(|_| ())
    }
    pub fn append(&mut self, meta: Meta, raw: &[u8]) -> io::Result<Index> {
        meta.clock.validate(raw.len())?;
        if meta.service_id.is_empty()
            || meta.segment_id.is_empty()
            || !meta.frequency_hz.is_finite()
            || meta.sequence.parse::<u32>().is_err()
        {
            return Err(invalid("invalid recording identity"));
        }
        let compressed = zstd::bulk::compress(raw, 1)?;
        let (codec, bytes) = if compressed.len() < raw.len() {
            ("zstd", compressed.as_slice())
        } else {
            ("raw", raw)
        };
        let raw_crc32 = crc32fast::hash(raw);
        let header = Header {
            meta: meta.clone(),
            codec: codec.into(),
            raw_crc32,
        };
        let offset = self.record(b"BLK2", &json(&header)?, bytes)?;
        let index = Index {
            offset: offset.to_string(),
            meta,
            codec: codec.into(),
            raw_bytes: raw.len() as u32,
            stored_bytes: bytes.len() as u32,
            raw_crc32,
        };
        self.blocks += 1;
        self.samples += u64::from(index.meta.clock.samples);
        self.page.push(index.clone());
        if self.page.len() == INDEX_PAGE {
            self.index_page()?;
        }
        Ok(index)
    }
    fn index_page(&mut self) -> io::Result<()> {
        if !self.page.is_empty() {
            self.record(b"IDX2", &json(&self.page)?, &[])?;
            self.page.clear();
        }
        Ok(())
    }
    pub fn position(&mut self) -> io::Result<u64> {
        self.output.stream_position()
    }
    pub fn output_mut(&mut self) -> &mut W {
        &mut self.output
    }
    pub fn finish(mut self) -> io::Result<W> {
        self.index_page()?;
        self.record(
            b"END2",
            &json(&Footer {
                blocks: self.blocks.to_string(),
                samples: self.samples.to_string(),
                records_crc32: self.digest.clone().finalize(),
            })?,
            &[],
        )?;
        self.output.flush()?;
        Ok(self.output)
    }
}

pub struct Scan {
    pub complete: bool,
    pub valid_bytes: u64,
    pub blocks: u64,
    pub samples: u64,
}
struct Record {
    kind: [u8; 4],
    meta: Vec<u8>,
    payload: Vec<u8>,
    crc: [u8; 4],
}
fn record<R: Read>(input: &mut R) -> io::Result<Record> {
    let mut prefix = [0; 16];
    input.read_exact(&mut prefix)?;
    let meta_len = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;
    let payload_len = u32::from_le_bytes(prefix[8..12].try_into().unwrap()) as usize;
    if meta_len > MAX_META || payload_len > MAX_BLOCK_BYTES {
        return Err(invalid("archive record exceeds bounds"));
    }
    let mut meta = vec![0; meta_len];
    let mut payload = vec![0; payload_len];
    input.read_exact(&mut meta)?;
    input.read_exact(&mut payload)?;
    let mut hash = crc32fast::Hasher::new();
    hash.update(&prefix[..12]);
    hash.update(&meta);
    hash.update(&payload);
    let crc = prefix[12..16].try_into().unwrap();
    if hash.finalize() != u32::from_le_bytes(crc) {
        return Err(invalid("archive record CRC32 mismatch"));
    }
    Ok(Record {
        kind: prefix[..4].try_into().unwrap(),
        meta,
        payload,
        crc,
    })
}
fn block(offset: u64, record: Record) -> io::Result<(Index, Vec<u8>)> {
    let header: Header = parse(&record.meta)?;
    let raw_len = header.meta.clock.samples as usize * 8;
    header.meta.clock.validate(raw_len)?;
    let raw = match header.codec.as_str() {
        "raw" => record.payload.clone(),
        "zstd" => zstd::bulk::decompress(&record.payload, raw_len)?,
        _ => return Err(invalid("unknown IQ block codec")),
    };
    if raw.len() != raw_len || crc32fast::hash(&raw) != header.raw_crc32 {
        return Err(invalid("IQ block raw CRC32/length mismatch"));
    }
    Ok((
        Index {
            offset: offset.to_string(),
            meta: header.meta,
            codec: header.codec,
            raw_bytes: raw_len as u32,
            stored_bytes: record.payload.len() as u32,
            raw_crc32: header.raw_crc32,
        },
        raw,
    ))
}
/// One decoded block plus one index page in memory, independent of recording length.
/// A truncated last record returns the checked prefix. Corruption is an error; callers retain
/// earlier visitor results and must mark the recording failed, never skip a corrupt block.
pub fn scan<R: Read + Seek>(
    input: &mut R,
    mut visit: impl FnMut(&Index, &[u8]) -> io::Result<()>,
) -> io::Result<Scan> {
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid(
            "unsupported IQ archive version (expected WBIQ0002)",
        ));
    }
    let mut result = Scan {
        complete: false,
        valid_bytes: 8,
        blocks: 0,
        samples: 0,
    };
    let mut page = Vec::new();
    let mut digest = crc32fast::Hasher::new();
    loop {
        let offset = input.stream_position()?;
        let rec = match record(input) {
            Ok(rec) => rec,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(result),
            Err(error) => return Err(error),
        };
        if &rec.kind != b"END2" {
            digest.update(&rec.crc);
        }
        match &rec.kind {
            b"BLK2" => {
                if page.len() == INDEX_PAGE {
                    return Err(invalid("missing bounded index page"));
                }
                let (index, raw) = block(offset, rec)?;
                visit(&index, &raw)?;
                result.blocks += 1;
                result.samples += u64::from(index.meta.clock.samples);
                page.push(index);
            }
            b"EVT2" => {
                let _: Value = parse(&rec.meta)?;
                if !rec.payload.is_empty() {
                    return Err(invalid("event payload"));
                }
            }
            b"IDX2" => {
                let persisted: Vec<Index> = parse(&rec.meta)?;
                if !rec.payload.is_empty() || persisted != page || page.is_empty() {
                    return Err(invalid("archive index mismatch"));
                }
                page.clear();
            }
            b"END2" => {
                let footer: Footer = parse(&rec.meta)?;
                if !page.is_empty()
                    || !rec.payload.is_empty()
                    || footer.blocks != result.blocks.to_string()
                    || footer.samples != result.samples.to_string()
                    || footer.records_crc32 != digest.finalize()
                {
                    return Err(invalid("archive footer mismatch"));
                }
                if input.read(&mut [0])? != 0 {
                    return Err(invalid("bytes after IQ archive footer"));
                }
                result.complete = true;
                result.valid_bytes = input.stream_position()?;
                return Ok(result);
            }
            _ => return Err(invalid("unknown IQ archive record")),
        }
        result.valid_bytes = input.stream_position()?;
    }
}
/// Indexed reads verify both the record and the raw CF32 checksum again.
pub fn read_at<R: Read + Seek>(input: &mut R, index: &Index) -> io::Result<Vec<u8>> {
    let offset = index
        .offset
        .parse::<u64>()
        .map_err(|_| invalid("invalid archive offset"))?;
    input.seek(SeekFrom::Start(offset))?;
    let rec = record(input)?;
    if &rec.kind != b"BLK2" {
        return Err(invalid("index is not a block"));
    }
    let (actual, raw) = block(offset, rec)?;
    if &actual != index {
        return Err(invalid("indexed IQ metadata mismatch"));
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn meta(samples: u32, sequence: u32) -> Meta {
        Meta {
            service_id: "boot-A".into(),
            segment_id: "7-9".into(),
            sequence: sequence.to_string(),
            frequency_hz: 100e6,
            baseband: serde_json::json!({"passband_hz":3000}),
            clock: IqBlockMeta {
                receiver_uid: "uid-A".into(),
                source_id: "primary".into(),
                track_id: "track-A".into(),
                epoch: "7".into(),
                channel_id: "9".into(),
                config_revision: "9007199254740999".into(),
                first_input_index: "18446744073709500000".into(),
                first_output_index: sequence.to_string(),
                fraction_num: "1".into(),
                fraction_den: "3".into(),
                samples,
                sample_format: "cf32_le".into(),
                amplitude_scale: 1.0,
                input_rate_hz: 62500000.0,
                actual_rate_hz: 62500000.0 / 10368.0,
                delay_seconds: Some(0.012345),
            },
        }
    }
    #[test]
    fn independent_checked_blocks_bounded_index_pages_exact_cf32_and_random_access() {
        let special: Vec<u8> = [
            0u32, 0x80000000, 0x7fc01234, 0x7f800000, 0xff800000, 1, 0x3f800001, 0xffc01234,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        let mut writer = Writer::new(Cursor::new(Vec::new())).unwrap();
        let mut expected = vec![special, vec![0; 8192]];
        expected.extend(
            include_bytes!("../../../tests/fixtures/ft8/real_slot_ja0txu.iq")
                .chunks(MAX_BLOCK_BYTES)
                .map(|b| b.to_vec()),
        );
        expected.extend((0..260).map(|_| vec![0; 8]));
        let mut indexes = Vec::new();
        for (seq, raw) in expected.iter().enumerate() {
            indexes.push(
                writer
                    .append(meta((raw.len() / 8) as u32, seq as u32), raw)
                    .unwrap(),
            );
        }
        assert!(indexes.iter().any(|i| i.codec == "raw"));
        assert!(indexes.iter().any(|i| i.codec == "zstd"));
        let data = writer.finish().unwrap().into_inner();
        let mut count = 0;
        let result = scan(&mut Cursor::new(&data), |index, raw| {
            assert_eq!(index, &indexes[count]);
            assert_eq!(raw, expected[count]);
            count += 1;
            Ok(())
        })
        .unwrap();
        assert!(result.complete);
        assert_eq!(count, expected.len());
        for (index, raw) in indexes.iter().zip(&expected).rev() {
            assert_eq!(read_at(&mut Cursor::new(&data), index).unwrap(), *raw);
        }
        // Tear the final block, an index page and the footer independently.
        let last = indexes.last().unwrap().offset.parse::<usize>().unwrap();
        for end in [last + 4, last + 20, data.len() - 1] {
            let mut recovered = 0;
            let result = scan(&mut Cursor::new(&data[..end]), |_, _| {
                recovered += 1;
                Ok(())
            })
            .unwrap();
            assert!(!result.complete);
            assert!(recovered >= expected.len() - 1);
        }
        let mut corrupt = data.clone();
        corrupt[last + 20] ^= 1;
        assert!(scan(&mut Cursor::new(corrupt), |_, _| Ok(()))
            .err()
            .unwrap()
            .to_string()
            .contains("CRC32"));
        let mut trailing = data;
        trailing.push(0);
        assert!(scan(&mut Cursor::new(trailing), |_, _| Ok(())).is_err());
        assert!(scan(&mut Cursor::new(b"WBIQ0001"), |_, _| Ok(())).is_err());
    }
    #[test]
    fn disk_full_is_an_error_and_cannot_report_completion() {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        assert_eq!(Writer::new(file).err().unwrap().raw_os_error(), Some(28));
    }
}
