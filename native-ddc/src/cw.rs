//! Mono CW decoding with spectral tone gating, AFC and transmission boundaries.
//!
//! Feed unenhanced demodulator audio. Recreate on tune, mode, IF, parameter,
//! disable or disconnect changes. Reset preserves the operator's pitch and
//! clears decoder, AFC, gate and pending-line history.
use std::ffi::{c_char, c_void};
use std::ptr::NonNull;

const PROBES: usize = 28;
const MIN_SHARE: f64 = 0.45;
const TOLERANCE_HZ: f32 = 900.0;

extern "C" {
    fn native_ggmorse_new(rate: i32, pitch: f32, tolerance: f32) -> *mut c_void;
    fn native_ggmorse_free(handle: *mut c_void);
    fn native_ggmorse_push(handle: *mut c_void, pcm: *const i16, count: usize) -> i32;
    fn native_ggmorse_take(handle: *mut c_void, text: *mut c_char, capacity: usize) -> usize;
    fn native_ggmorse_pitch(handle: *mut c_void) -> f32;
    fn native_ggmorse_wpm(handle: *mut c_void) -> f32;
}

#[derive(Debug, Default)]
pub struct CwChunk {
    pub text: String,
    pub ends_line: bool,
    pub share: f32,
    pub rms: f32,
    pub keyed: bool,
    /// AFC input-filter centre; the listener's pitch is never changed by AFC.
    pub center_hz: f32,
    pub pitch_hz: f32,
    pub wpm: f32,
    pub error: Option<String>,
}

pub struct CwDecoder {
    handle: NonNull<c_void>,
    rate: u32,
    pitch: f32,
    nco_re: [f64; PROBES],
    nco_im: [f64; PROBES],
    nco_dr: [f64; PROBES],
    nco_di: [f64; PROBES],
    band_i: [f64; PROBES],
    band_q: [f64; PROBES],
    band_env: [f64; PROBES],
    band_alpha: f64,
    env_alpha: f64,
    level: f64,
    box_sum: f64,
    box_n: usize,
    bp_alpha: f64,
    tone_decim: usize,
    bp_re: f64,
    bp_im: f64,
    bp_dr: f64,
    bp_di: f64,
    bp_i: f64,
    bp_q: f64,
    center: f64,
    share_peak: f64,
    tone_hold: usize,
    since_confident: f64,
    line_pending: bool,
    pcm: Vec<i16>,
}

// The C++ decoder has no thread affinity or shared mutable globals. A stream
// has exactly one owner and all mutation requires &mut self.
unsafe impl Send for CwDecoder {}

impl CwDecoder {
    pub fn new(rate: u32, pitch: f32) -> Result<Self, String> {
        if !(8000..=192000).contains(&rate) || !pitch.is_finite() || !(100.0..=1600.0).contains(&pitch) {
            return Err("CW requires 8–192 kHz mono PCM and a 100–1600 Hz pitch".into());
        }
        let handle = NonNull::new(unsafe { native_ggmorse_new(rate as i32, pitch, TOLERANCE_HZ) })
            .ok_or_else(|| "ggmorse: decoder allocation failed".to_string())?;
        let tau = std::f64::consts::TAU;
        let tone_decim = (rate as usize / 8000).max(1);
        let gate_rate = rate as f64 / tone_decim as f64;
        let mut decoder = Self {
            handle, rate, pitch,
            nco_re: [1.0; PROBES], nco_im: [0.0; PROBES],
            nco_dr: [0.0; PROBES], nco_di: [0.0; PROBES],
            band_i: [0.0; PROBES], band_q: [0.0; PROBES], band_env: [0.0; PROBES],
            band_alpha: 1.0 - (-tau * 50.0 / gate_rate).exp(),
            env_alpha: 1.0 - (-tau * 3.0 / gate_rate).exp(),
            level: 0.0, box_sum: 0.0, box_n: 0, tone_decim,
            bp_alpha: 1.0 - (-tau * 200.0 / rate as f64).exp(),
            bp_re: 1.0, bp_im: 0.0, bp_dr: 1.0, bp_di: 0.0,
            bp_i: 0.0, bp_q: 0.0, center: pitch as f64,
            share_peak: 0.0, tone_hold: 0, since_confident: 0.0, line_pending: false,
            pcm: Vec::with_capacity((rate / 10) as usize),
        };
        for k in 0..PROBES {
            let phase = tau * (250 + 50 * k) as f64 / gate_rate;
            decoder.nco_dr[k] = phase.cos();
            decoder.nco_di[k] = phase.sin();
        }
        decoder.recenter(pitch as f64);
        Ok(decoder)
    }

    pub fn reset(&mut self) -> Result<(), String> {
        *self = Self::new(self.rate, self.pitch)?;
        Ok(())
    }

    fn recenter(&mut self, hz: f64) {
        let low = if self.pitch > TOLERANCE_HZ { self.pitch - TOLERANCE_HZ } else { 100.0 };
        self.center = hz.clamp(low as f64, (self.pitch + TOLERANCE_HZ) as f64);
        let phase = std::f64::consts::TAU * self.center / self.rate as f64;
        self.bp_dr = phase.cos();
        self.bp_di = phase.sin();
    }

    fn band_pass(&mut self, x: f64) -> f64 {
        self.bp_i += self.bp_alpha * (2.0 * x * self.bp_re - self.bp_i);
        self.bp_q += self.bp_alpha * (2.0 * x * self.bp_im - self.bp_q);
        let re = self.bp_re;
        let im = self.bp_im;
        self.bp_re = re * self.bp_dr - im * self.bp_di;
        self.bp_im = re * self.bp_di + im * self.bp_dr;
        self.bp_i * self.bp_re + self.bp_q * self.bp_im
    }

    fn tone(&mut self, input: &[f32]) -> (bool, f64, f64) {
        let mut peak = 0.0_f64;
        for &sample in input {
            self.box_sum += sample as f64;
            self.box_n += 1;
            if self.box_n < self.tone_decim { continue; }
            let x = self.box_sum / self.tone_decim as f64;
            self.box_sum = 0.0;
            self.box_n = 0;
            self.level += self.env_alpha * (x * x - self.level);
            let rms = self.level.sqrt();
            for k in 0..PROBES {
                self.band_i[k] += self.band_alpha * (x * self.nco_re[k] - self.band_i[k]);
                self.band_q[k] += self.band_alpha * (x * self.nco_im[k] - self.band_q[k]);
                let re = self.nco_re[k];
                let im = self.nco_im[k];
                self.nco_re[k] = re * self.nco_dr[k] - im * self.nco_di[k];
                self.nco_im[k] = re * self.nco_di[k] + im * self.nco_dr[k];
                self.band_env[k] += self.env_alpha * (self.band_i[k].hypot(self.band_q[k]) - self.band_env[k]);
                if rms > 0.0 { peak = peak.max(self.band_env[k] / rms); }
            }
        }
        let rms = self.level.sqrt();
        if rms <= 0.0 { return (false, 0.0, rms); }
        let mut best = 0.0;
        let mut best_k = 0;
        for k in 0..PROBES {
            let share = self.band_env[k] / rms;
            if share > best { best = share; best_k = k; }
        }
        let keyed = best >= MIN_SHARE;
        if keyed { self.recenter((250 + 50 * best_k) as f64); }
        (keyed, best.max(peak), rms)
    }

    /// Arbitrary block lengths are accepted; the bridge receives at most 100 ms
    /// per call. Status and line timing use actual input samples, never wall time.
    pub fn push(&mut self, input: &[f32]) -> CwChunk {
        let mut error = None;
        let block = (self.rate / 10) as usize;
        for samples in input.chunks(block) {
            self.pcm.clear();
            for &x in samples {
                let v = self.band_pass(x as f64);
                let quantized = if v >= 1.0 { 32767 } else if v <= -1.0 { -32768 }
                    else { (v * 32767.0 + 0.5).floor() as i16 };
                self.pcm.push(quantized);
            }
            if unsafe { native_ggmorse_push(self.handle.as_ptr(), self.pcm.as_ptr(), self.pcm.len()) } != 0 {
                error = Some("ggmorse: native decode failed".into());
                break;
            }
        }
        let (keyed, share, rms) = self.tone(input);
        let seconds = input.len() as f64 / self.rate as f64;
        self.share_peak = share.max(self.share_peak * (-seconds / 2.0).exp());
        if keyed { self.tone_hold = (self.rate as f64 * 0.8).round() as usize; }
        else { self.tone_hold = self.tone_hold.saturating_sub(input.len()); }
        let mut text_bytes = [0_u8; 4096];
        let count = unsafe { native_ggmorse_take(self.handle.as_ptr(), text_bytes.as_mut_ptr().cast(), text_bytes.len()) };
        let text = if self.tone_hold > 0 && error.is_none() {
            String::from_utf8_lossy(&text_bytes[..count]).chars().filter(|c| *c > '\u{1f}').collect()
        } else { String::new() };
        if !text.is_empty() { self.line_pending = true; }
        if !text.is_empty() && self.share_peak >= MIN_SHARE { self.since_confident = 0.0; }
        else { self.since_confident += seconds; }
        let ends_line = self.line_pending && self.since_confident >= 4.0;
        if ends_line { self.line_pending = false; }
        CwChunk {
            text, ends_line, keyed, share: self.share_peak as f32, rms: rms as f32,
            center_hz: self.center as f32,
            pitch_hz: unsafe { native_ggmorse_pitch(self.handle.as_ptr()) },
            wpm: unsafe { native_ggmorse_wpm(self.handle.as_ptr()) }, error,
        }
    }
}

impl Drop for CwDecoder {
    fn drop(&mut self) { unsafe { native_ggmorse_free(self.handle.as_ptr()) }; }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const BLOCK: usize = 4800;
    const PHRASE: &str = "CQ DE W1ABC";

    fn code(ch: char) -> &'static str {
        match ch {
            'C' => "-.-.", 'Q' => "--.-", 'D' => "-..", 'E' => ".",
            'W' => ".--", '1' => ".----", 'A' => ".-", 'B' => "-...",
            _ => unreachable!(),
        }
    }

    fn phrase_stream(tone_hz: f64) -> Vec<f32> {
        let dot = (1.2 / 20.0 * RATE as f64).round() as usize;
        let mut pcm = vec![0.0; RATE as usize / 5];
        let mut append = |key: bool, dots: usize| {
            for _ in 0..dots * dot {
                let phase = std::f64::consts::TAU * tone_hz * pcm.len() as f64 / RATE as f64;
                pcm.push(if key { (0.4 * phase.sin()) as f32 } else { 0.0 });
            }
        };
        for _ in 0..2 {
            for (wi, word) in PHRASE.split_whitespace().enumerate() {
                if wi > 0 { append(false, 7); }
                for (ci, ch) in word.chars().enumerate() {
                    if ci > 0 { append(false, 3); }
                    for (si, symbol) in code(ch).chars().enumerate() {
                        if si > 0 { append(false, 1); }
                        append(true, if symbol == '.' { 1 } else { 3 });
                    }
                }
            }
            append(false, 10);
            append(false, 42);
        }
        pcm.resize(pcm.len() + 6 * RATE as usize, 0.0);
        pcm
    }

    #[test]
    fn copies_call_and_acquires_off_pitch_tone() {
        let pcm = phrase_stream(800.0);
        for pitch in [800.0, 650.0] {
            let mut decoder = CwDecoder::new(RATE, pitch).unwrap();
            let mut text = String::new();
            let mut acquired = false;
            let mut speeds = Vec::new();
            for block in pcm.chunks(BLOCK) {
                let chunk = decoder.push(block);
                assert!(chunk.error.is_none(), "{:?}", chunk.error);
                text.push_str(&chunk.text);
                if chunk.keyed && chunk.rms > 0.05 {
                    assert!(chunk.share >= 0.45);
                    acquired |= (chunk.center_hz - 800.0).abs() <= 25.0;
                }
                if chunk.wpm > 4.0 { speeds.push(chunk.wpm); }
            }
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(normalized.contains(PHRASE), "pitch {pitch}: {text:?}");
            assert!(acquired, "AFC failed at operator pitch {pitch}");
            speeds.sort_by(f32::total_cmp);
            let speed = speeds[speeds.len() / 2];
            assert!((17.0..23.0).contains(&speed), "reported {speed} WPM");
        }
    }

    #[test]
    fn rejects_quiet_and_band_limited_noise() {
        let mut decoder = CwDecoder::new(RATE, 800.0).unwrap();
        let quiet = [0.0; BLOCK];
        for _ in 0..60 {
            let chunk = decoder.push(&quiet);
            assert_eq!(chunk.text, "");
            assert!(!chunk.keyed);
            assert!(!chunk.ends_line);
        }
        let mut seed = 0x4767_6d6f_u32;
        let mut ring = [0.0_f32; 60];
        let mut sum = 0.0;
        let mut index = 0;
        for _ in 0..60 {
            let mut noise = [0.0; BLOCK];
            for sample in &mut noise {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let raw = (seed as f64 / u32::MAX as f64 - 0.5) as f32;
                sum += raw - ring[index];
                ring[index] = raw;
                index = (index + 1) % ring.len();
                *sample = sum / ring.len() as f32 * 3.0;
            }
            let chunk = decoder.push(&noise);
            assert_eq!(chunk.text, "", "noise produced text");
            assert!(!chunk.ends_line);
        }
    }

    #[test]
    fn closes_after_trusted_character_gap_and_reset_discards_pending_line() {
        let pcm = phrase_stream(800.0);
        let mut decoder = CwDecoder::new(RATE, 800.0).unwrap();
        let mut last_trusted = None;
        let mut closed = false;
        for (at, block) in pcm.chunks(BLOCK).enumerate() {
            let chunk = decoder.push(block);
            if !chunk.text.is_empty() && chunk.share >= 0.45 { last_trusted = Some(at); }
            if chunk.ends_line {
                let gap = at - last_trusted.expect("line closed without trusted text");
                assert!((40..=41).contains(&gap), "line gap was {} ms", gap * 100);
                closed = true;
            }
        }
        assert!(closed, "transmission never closed");

        decoder.reset().unwrap();
        let mut pending_text = false;
        for block in pcm.chunks(BLOCK) {
            let chunk = decoder.push(block);
            if !chunk.text.is_empty() {
                pending_text = true;
                break;
            }
        }
        assert!(pending_text, "fixture never opened a line");
        decoder.reset().unwrap();
        let quiet = [0.0; BLOCK];
        for _ in 0..60 {
            let chunk = decoder.push(&quiet);
            assert_eq!(chunk.text, "");
            assert!(!chunk.ends_line, "previous stream's pending line escaped reset");
            assert!(!chunk.keyed);
            assert_eq!(chunk.center_hz, 800.0);
        }
    }
}
