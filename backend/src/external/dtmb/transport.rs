use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Program {
    pub id: u16,
    pub name: String,
    pub provider: String,
    pub pmt_pid: u16,
    pub video: String,
    pub audio: String,
    pub scrambled: bool,
}

#[derive(Default)]
struct Section {
    bytes: Vec<u8>,
}
impl Section {
    fn append(&mut self, mut data: &[u8], out: &mut Vec<Vec<u8>>) {
        while !data.is_empty() {
            if self.bytes.is_empty() && data[0] == 0xff { break; }
            let needed = if self.bytes.len() < 3 { 3 } else {
                3 + (((self.bytes[1] & 15) as usize) << 8 | self.bytes[2] as usize)
            };
            if !(3..=4096).contains(&needed) { self.bytes.clear(); break; }
            let n = data.len().min(needed - self.bytes.len());
            self.bytes.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.bytes.len() == needed && needed > 3 {
                if needed >= 12 && crc(&self.bytes) == 0 && self.bytes[5] & 1 != 0 {
                    out.push(std::mem::take(&mut self.bytes));
                } else { self.bytes.clear(); }
            } else if n == 0 { self.bytes.clear(); break; }
        }
    }
    fn push(&mut self, data: &[u8], start: bool) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if start {
            let Some((&pointer, data)) = data.split_first() else { return out; };
            let pointer = pointer as usize;
            if pointer > data.len() { self.bytes.clear(); return out; }
            if !self.bytes.is_empty() { self.append(&data[..pointer], &mut out); }
            self.bytes.clear();
            self.append(&data[pointer..], &mut out);
        } else if !self.bytes.is_empty() { self.append(data, &mut out); }
        out
    }
}

pub(super) fn crc(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0xffff_ffff, |mut value, byte| {
        value ^= (*byte as u32) << 24;
        for _ in 0..8 { value = (value << 1) ^ if value & 0x8000_0000 != 0 { 0x04c1_1db7 } else { 0 }; }
        value
    })
}

fn text(data: &[u8]) -> String {
    let decoded = match data.first().copied() {
        Some(0x15) => String::from_utf8_lossy(&data[1..]).into_owned(),
        Some(0x13) => encoding_rs::GBK.decode(&data[1..]).0.into_owned(),
        Some(0x11) => String::from_utf16_lossy(&data[1..].chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect::<Vec<_>>()),
        Some(0x01..=0x0b) => encoding_rs::WINDOWS_1252.decode(&data[1..]).0.into_owned(),
        _ => std::str::from_utf8(data).map(str::to_owned).unwrap_or_else(|_| encoding_rs::GBK.decode(data).0.into_owned()),
    };
    decoded.chars().filter(|c| !c.is_control()).take(128).collect()
}
fn descriptors(mut data: &[u8], mut visit: impl FnMut(u8, &[u8])) {
    while data.len() >= 2 {
        let n = data[1] as usize;
        if n + 2 > data.len() { break; }
        visit(data[0], &data[2..2+n]);
        data = &data[2+n..];
    }
}
fn has_ca(data: &[u8]) -> bool {
    let mut ca = false;
    descriptors(data, |tag, _| ca |= tag == 9);
    ca
}

pub(super) struct Transport {
    pending: Vec<u8>,
    counters: [u8; 8192],
    sections: HashMap<u16, Section>,
    pat_key: Option<(u16, u8, u8)>,
    pat_parts: BTreeMap<u8, Vec<(u16, u16)>>,
    pub programs: BTreeMap<u16, Program>,
    pub packets: u64,
    pub errors: u64,
    pub continuity_errors: u64,
    pub sync_losses: u64,
}
impl Default for Transport {
    fn default() -> Self {
        Self { pending: Vec::new(), counters: [255; 8192], sections: HashMap::new(),
            pat_key: None, pat_parts: BTreeMap::new(), programs: BTreeMap::new(),
            packets: 0, errors: 0, continuity_errors: 0, sync_losses: 0 }
    }
}
impl Transport {
    pub fn push(&mut self, data: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(data);
        let mut offset = 0;
        let mut valid = Vec::with_capacity(self.pending.len());
        while self.pending.len() - offset >= 188 {
            if self.pending[offset] != 0x47 || (self.pending.len() - offset >= 376 && self.pending[offset+188] != 0x47) {
                offset += 1; self.sync_losses += 1; continue;
            }
            let packet: [u8; 188] = self.pending[offset..offset+188].try_into().unwrap();
            self.packet(&packet);
            valid.extend_from_slice(&packet);
            offset += 188;
        }
        self.pending.drain(..offset);
        valid
    }
    fn packet(&mut self, p: &[u8; 188]) {
        self.packets += 1;
        let pid = ((p[1] as u16 & 31) << 8) | p[2] as u16;
        if pid == 8191 { return; }
        let control = p[3] >> 4 & 3;
        if p[1] & 0x80 != 0 || control == 0 { self.errors += 1; self.sections.remove(&pid); return; }
        let mut offset = 4;
        if control & 2 != 0 {
            offset = 5 + p[4] as usize;
            if offset > 188 { self.errors += 1; self.sections.remove(&pid); return; }
            if p[4] > 0 && p[5] & 0x80 != 0 { self.counters[pid as usize] = 255; self.sections.remove(&pid); }
        }
        if control & 1 == 0 || offset >= 188 { return; }
        let previous = self.counters[pid as usize];
        let cc = p[3] & 15;
        if previous == cc { return; }
        if previous != 255 && cc != (previous + 1) & 15 {
            self.continuity_errors += 1;
            self.sections.remove(&pid);
        }
        self.counters[pid as usize] = cc;
        if pid != 0 && pid != 17 && !self.programs.values().any(|program| program.pmt_pid == pid) { return; }
        if p[3] & 0xc0 != 0 { return; }
        let sections = self.sections.entry(pid).or_default().push(&p[offset..], p[1] & 0x40 != 0);
        for section in sections {
            match (pid, section[0]) {
                (0, 0) => self.pat(&section),
                (17, 0x42) => self.sdt(&section),
                (_, 2) => self.pmt(pid, &section),
                _ => (),
            }
        }
    }
    fn pat(&mut self, s: &[u8]) {
        if s[6] > s[7] || s[7] > 31 { return; }
        let key = (u16::from_be_bytes([s[3], s[4]]), s[5] >> 1 & 31, s[7]);
        if self.pat_key != Some(key) { self.pat_key = Some(key); self.pat_parts.clear(); }
        let entries = s[8..s.len()-4].chunks_exact(4).filter_map(|p| {
            let id = u16::from_be_bytes([p[0], p[1]]);
            (id != 0).then_some((id, ((p[2] as u16 & 31) << 8) | p[3] as u16))
        }).collect();
        self.pat_parts.insert(s[6], entries);
        if self.pat_parts.len() != s[7] as usize + 1 { return; }
        let entries: BTreeMap<_, _> = self.pat_parts.values().flatten().copied().take(64).collect();
        self.programs.retain(|id, _| entries.contains_key(id));
        for (id, pid) in entries {
            let program = self.programs.entry(id).or_insert_with(|| Program { id, name: format!("Service {id}"), ..Program::default() });
            if program.pmt_pid != pid { program.video.clear(); program.audio.clear(); }
            program.pmt_pid = pid;
        }
        self.sections.retain(|pid, _| *pid == 0 || *pid == 17 || self.programs.values().any(|p| p.pmt_pid == *pid));
    }
    fn pmt(&mut self, pid: u16, s: &[u8]) {
        if s.len() < 16 { return; }
        let id = u16::from_be_bytes([s[3], s[4]]);
        let Some(program) = self.programs.get_mut(&id).filter(|p| p.pmt_pid == pid) else { return; };
        let end = s.len()-4;
        let start = 12 + (((s[10] & 15) as usize) << 8 | s[11] as usize);
        if start > end { return; }
        let mut video = String::new();
        let mut audio = String::new();
        let mut scrambled = has_ca(&s[12..start]);
        let mut pos = start;
        while pos + 5 <= end {
            let next = pos + 5 + (((s[pos+3] & 15) as usize) << 8 | s[pos+4] as usize);
            if next > end { return; }
            let descriptor = &s[pos+5..next];
            scrambled |= has_ca(descriptor);
            let (v, a) = match s[pos] {
                0x01 | 0x02 => ("MPEG-2", ""), 0x1b => ("H.264", ""), 0x24 => ("HEVC", ""),
                0x42 => ("AVS+", ""), 0xd2 => ("AVS2", ""), 0xd4 => ("AVS3", ""),
                0x03 | 0x04 => ("", "MPEG audio"), 0x0f | 0x11 => ("", "AAC"),
                0x81 => ("", "AC-3"), 0x87 => ("", "E-AC-3"), 0x80 => ("", "DRA"),
                _ => ("", ""),
            };
            if !v.is_empty() && video.is_empty() { video = v.into(); }
            if !a.is_empty() && audio.is_empty() { audio = a.into(); }
            if s[pos] == 6 { descriptors(descriptor, |tag, _| { if tag == 0x6a { audio = "AC-3".into(); } else if tag == 0x7a { audio = "E-AC-3".into(); } }); }
            pos = next;
        }
        program.video = video; program.audio = audio; program.scrambled = scrambled;
    }
    fn sdt(&mut self, s: &[u8]) {
        if s.len() < 15 || self.pat_key.is_none_or(|key| key.0 != u16::from_be_bytes([s[3], s[4]])) { return; }
        let end = s.len()-4;
        let mut pos = 11;
        while pos + 5 <= end {
            let next = pos + 5 + (((s[pos+3] & 15) as usize) << 8 | s[pos+4] as usize);
            if next > end { break; }
            let id = u16::from_be_bytes([s[pos], s[pos+1]]);
            if let Some(program) = self.programs.get_mut(&id) {
                program.scrambled |= s[pos+3] & 0x10 != 0;
                descriptors(&s[pos+5..next], |tag, data| {
                    if tag != 0x48 || data.len() < 3 { return; }
                    let n = data[1] as usize;
                    if 3+n > data.len() { return; }
                    let length = data[2+n] as usize;
                    if 3+n+length > data.len() { return; }
                    program.provider = text(&data[2..2+n]);
                    program.name = text(&data[3+n..3+n+length]);
                    if program.name.is_empty() { program.name = format!("Service {id}"); }
                });
            }
            pos = next;
        }
    }
}
