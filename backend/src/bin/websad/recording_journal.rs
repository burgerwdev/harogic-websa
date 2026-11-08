//! Shared bounded CRC/Zstd journal codec. Consumers validate their own typed payloads.
use super::*;
use std::io::{Read, Seek};
const MAX_RECORD: usize = 3 * 1024 * 1024;

fn record(output: &mut impl Write, kind: &[u8; 4], raw: &[u8], stored: &[u8]) -> io::Result<u32> {
    let mut header = Vec::with_capacity(16);
    header.extend_from_slice(kind);
    header.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    header.extend_from_slice(&(stored.len() as u32).to_le_bytes());
    let mut crc = crc32fast::Hasher::new();
    crc.update(&header);
    crc.update(stored);
    let checksum = crc.finalize();
    header.extend_from_slice(&checksum.to_le_bytes());
    output.write_all(&header)?;
    output.write_all(stored)?;
    Ok(checksum)
}
pub(super) struct Writer {
    file: File,
    digest: crc32fast::Hasher,
    pub(super) records: u64,
    raw: u64,
}
impl Writer {
    pub(super) fn new(path: &FsPath, magic: &[u8; 8]) -> io::Result<Self> {
        let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
        file.write_all(magic)?;
        file.sync_all()?;
        Ok(Self {
            file,
            digest: crc32fast::Hasher::new(),
            records: 0,
            raw: 0,
        })
    }
    pub(super) fn append(&mut self, raw: &[u8]) -> io::Result<u64> {
        if raw.is_empty() || raw.len() > MAX_RECORD {
            return Err(io_error("journal record exceeds bound"));
        }
        let compressed = zstd::bulk::compress(raw, 1)?;
        let (kind, stored) = if compressed.len() < raw.len() {
            (b"EVZ1", compressed.as_slice())
        } else {
            (b"EVR1", raw)
        };
        let crc = record(&mut self.file, kind, raw, stored)?;
        self.digest.update(&crc.to_le_bytes());
        self.records += 1;
        self.raw += raw.len() as u64;
        self.file.stream_position()
    }
    pub(super) fn checkpoint(&mut self) -> io::Result<u64> {
        self.file.sync_data()?;
        self.file.stream_position()
    }
    pub(super) fn finish(&mut self) -> io::Result<u64> {
        let raw = serde_json::to_vec(
            &json!({"records":self.records.to_string(),"raw_bytes":self.raw.to_string(),"digest":self.digest.clone().finalize()}),
        )?;
        record(&mut self.file, b"END1", &raw, &raw)?;
        self.file.sync_all()?;
        self.file.stream_position()
    }
}
pub(super) struct Scan {
    pub(super) valid: u64,
    pub(super) complete: bool,
    pub(super) error: Option<String>,
}
/// One record stays resident. A checked prefix is returned even on corrupt or torn tails.
pub(super) fn scan(
    path: &FsPath,
    magic: &[u8; 8],
    limit: u64,
    mut visit: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<Scan> {
    let mut file = File::open(path)?;
    let mut actual_magic = [0; 8];
    file.read_exact(&mut actual_magic)?;
    if &actual_magic != magic || limit < 8 {
        return Err(io_error("invalid journal archive header"));
    }
    let mut result = Scan {
        valid: 8,
        complete: false,
        error: None,
    };
    let (mut records, mut bytes) = (0u64, 0u64);
    let mut digest = crc32fast::Hasher::new();
    while file.stream_position()? < limit {
        let checked = (|| -> io::Result<bool> {
            let mut header = [0; 16];
            file.read_exact(&mut header)?;
            let raw_len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            let stored_len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
            if raw_len == 0
                || raw_len > MAX_RECORD
                || stored_len == 0
                || stored_len > MAX_RECORD
                || file
                    .stream_position()?
                    .checked_add(stored_len as u64)
                    .is_none_or(|end| end > limit)
            {
                return Err(io_error("invalid or incomplete journal record length"));
            }
            let mut stored = vec![0; stored_len];
            file.read_exact(&mut stored)?;
            let mut crc = crc32fast::Hasher::new();
            crc.update(&header[..12]);
            crc.update(&stored);
            let checksum = u32::from_le_bytes(header[12..16].try_into().unwrap());
            if checksum != crc.finalize() {
                return Err(io_error("journal record CRC mismatch"));
            }
            let raw = match &header[..4] {
                b"EVZ1" => zstd::bulk::decompress(&stored, raw_len)?,
                b"EVR1" | b"END1" => stored,
                _ => return Err(io_error("unknown journal record kind")),
            };
            if raw.len() != raw_len {
                return Err(io_error("journal record decoded length mismatch"));
            }
            if &header[..4] == b"END1" {
                let row: Value = serde_json::from_slice(&raw)?;
                if number(&row["records"]) != records
                    || number(&row["raw_bytes"]) != bytes
                    || row["digest"].as_u64() != Some(u64::from(digest.clone().finalize()))
                    || file.stream_position()? != limit
                {
                    return Err(io_error("journal archive footer mismatch"));
                }
                return Ok(true);
            }
            visit(&raw)?;
            digest.update(&checksum.to_le_bytes());
            records += 1;
            bytes += raw.len() as u64;
            Ok(false)
        })();
        match checked {
            Ok(complete) => {
                result.valid = file.stream_position()?;
                result.complete = complete;
                if complete {
                    break;
                }
            }
            Err(error) => {
                result.error = Some(error.to_string());
                break;
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_and_compressed_records_roundtrip_with_checked_footer_and_io_errors() {
        let path = std::env::temp_dir().join(format!(
            "websa-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut seed = 43u64;
        let noise = (0..32768)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect::<Vec<_>>();
        let periodic = vec![0; 32768];
        let mut writer = Writer::new(&path, b"TEST0001").unwrap();
        writer.append(&noise).unwrap();
        writer.append(&periodic).unwrap();
        let checkpoint = writer.checkpoint().unwrap();
        let mut prefix = Vec::new();
        let checked = scan(&path, b"TEST0001", checkpoint, |raw| {
            prefix.push(raw.to_vec());
            Ok(())
        })
        .unwrap();
        assert!(!checked.complete && checked.error.is_none());
        assert_eq!(checked.valid, checkpoint);
        assert_eq!(prefix, vec![noise.clone(), periodic.clone()]);
        let length = writer.finish().unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[8..12], b"EVR1");
        let next = 24 + noise.len();
        assert_eq!(&bytes[next..next + 4], b"EVZ1");
        let mut rows = Vec::new();
        let result = scan(&path, b"TEST0001", length, |raw| {
            rows.push(raw.to_vec());
            Ok(())
        })
        .unwrap();
        assert!(result.complete && result.error.is_none());
        assert_eq!(rows, vec![noise, periodic]);
        if let Ok(mut full) = OpenOptions::new().write(true).open("/dev/full") {
            assert!(record(&mut full, b"EVR1", b"x", b"x").is_err());
        }
        fs::remove_file(path).unwrap();
    }
}
