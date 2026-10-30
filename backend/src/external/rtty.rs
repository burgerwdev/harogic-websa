//! MIT host bridge to the separately distributed GPL OnAir RTTY companion.
//! Input is protocol IQ; listener gain, squelch, AGC and NR never enter this path.
use super::{executable, process::{self, Process}, Dependency, ExternalDecodeEvent};
use serde_json::{json, Value};
use std::sync::LazyLock;
use websa_dsp::analog::{self, AnalogDemod};
use websa_dsp::plugin::{AnalogDemodulator, DemodConfig};

fn dependency() -> &'static Dependency {
    static DEP: LazyLock<Dependency> = LazyLock::new(|| {
        let result = executable("WEBSA_RTTY_PATH", "websa-rtty").and_then(|path| {
            let banner = process::probe(&path, "--version", 0)?;
            if !banner.contains("websa-rtty protocol=1 audio=f32le rate=8000") {
                return Err("RTTY companion protocol mismatch; reinstall websa-rtty".into());
            }
            Ok(path)
        });
        match result {
            Ok(path) => Dependency { path: Some(path), error: None, modes: Vec::new() },
            Err(error) => Dependency { path: None, error: Some(error), modes: Vec::new() },
        }
    });
    &DEP
}
fn setting(name: &str, default: &str, choices: &[&str]) -> Result<String, String> {
    let value = std::env::var(name).unwrap_or_else(|_| default.into());
    if choices.contains(&value.as_str()) { Ok(value) }
    else { Err(format!("{name} must be one of {}", choices.join(", "))) }
}
fn settings() -> Result<Vec<String>, String> {
    let baud = setting("WEBSA_RTTY_BAUD", "45.45", &["45.45", "50", "75", "100"])?;
    let shift = setting("WEBSA_RTTY_SHIFT", "170", &["170", "200", "425", "450", "850"])?;
    let usos = setting("WEBSA_RTTY_USOS", "0", &["0", "1"])?;
    let low = std::env::var("WEBSA_RTTY_LOW_TONE").unwrap_or_else(|_| "0".into());
    let frequency: f64 = low.parse().map_err(|_| "WEBSA_RTTY_LOW_TONE must be numeric")?;
    let shift_hz: f64 = shift.parse().map_err(|_| "invalid RTTY shift")?;
    if !frequency.is_finite() || (frequency != 0.0 && !(300.0..=3300.0-shift_hz).contains(&frequency)) {
        return Err("WEBSA_RTTY_LOW_TONE must be 0 (auto) or a tone pair within 300–3300 Hz".into());
    }
    Ok(vec!["--baud".into(), baud, "--shift".into(), shift,
        "--low-tone".into(), low, "--usos".into(), usos])
}
pub fn unavailable_reason() -> Option<String> {
    dependency().unavailable_reason().or_else(|| settings().err())
}

pub struct RttyBridge {
    process: Process,
    demod: AnalogDemod,
    audio: Vec<f32>,
    mode: String,
}
impl RttyBridge {
    pub fn spawn(mode: &str, rate: u32) -> Result<Self, String> {
        if rate != 48_000 { return Err("RTTY requires exactly 48000 complex samples/s".into()); }
        let reverse = match mode { "rtty" => false, "rtty-reverse" => true, _ => return Err("unknown RTTY mode".into()) };
        if let Some(error) = unavailable_reason() { return Err(error); }
        let path = dependency().path.as_ref().ok_or("RTTY companion unavailable")?;
        let mut args = settings()?;
        args.extend(["--reverse".into(), if reverse { "1" } else { "0" }.into()]);
        let mut demod = AnalogDemod::new_prechannelized(
            analog::spec_for("usb").ok_or("USB demodulator unavailable")?,
            DemodConfig::new(rate as f64, 3400.0), 8000.0);
        demod.set_agc_enabled(false);
        demod.set_deemph_us(0.0);
        Ok(Self { process: Process::spawn(path, &args)?, demod, audio: Vec::new(), mode: mode.into() })
    }
    pub fn push_iq(&mut self, iq: &[f32]) -> Result<Vec<ExternalDecodeEvent>, String> {
        self.demod.process_into(iq, &mut self.audio);
        let mut bytes = self.process.buffer();
        bytes.reserve(self.audio.len() * 4);
        for sample in &self.audio { bytes.extend_from_slice(&sample.to_le_bytes()); }
        let lines = self.process.push(bytes)?;
        self.events(lines)
    }
    fn events(&self, lines: Vec<String>) -> Result<Vec<ExternalDecodeEvent>, String> {
        lines.into_iter().filter(|line| !line.trim().is_empty()).map(|line| {
            let mut data: Value = serde_json::from_str(&line).map_err(|e| format!("RTTY malformed JSON: {e}"))?;
            let kind = data.get("kind").and_then(Value::as_str).ok_or("RTTY result lacks kind")?.to_owned();
            if !matches!(kind.as_str(), "text" | "table") || !data.is_object() {
                return Err("RTTY companion returned unsupported record".into());
            }
            data.as_object_mut().unwrap().remove("kind");
            Ok(ExternalDecodeEvent { provider: "onair-rtty".into(), mode: self.mode.clone(), kind, data })
        }).collect()
    }
    pub fn retire(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        let lines = self.process.retire()?;
        let mut events = self.events(lines)?;
        events.push(ExternalDecodeEvent { provider: "onair-rtty".into(), mode: self.mode.clone(), kind: "status".into(),
            data: json!({"state":"stopped","reason":"reconfigured","upstreamCompletionKnown":false}) });
        Ok(events)
    }
    pub fn finish(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        let lines = self.process.finish()?;
        self.events(lines)
    }
}
