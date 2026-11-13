//! Independent decoder processes consume protocol IQ, never listener PCM.
pub mod onair;
pub mod rtty;
pub mod gnss;
pub mod dtmb;
mod process;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;
use std::sync::LazyLock;
use process::Process;
use websa_dsp::audio::DcBlock;

#[derive(Debug, Clone)]
pub struct ExternalDecodeEvent {
    pub provider: String,
    pub mode: String,
    pub kind: String,
    pub data: Value,
}
impl ExternalDecodeEvent {
    pub fn into_value(self) -> Value {
        json!({"type":"decoder-result","provider":self.provider,"mode":self.mode,
            "kind":self.kind,"data":self.data})
    }
}

pub(crate) fn executable(variable: &str, fallback: &str) -> Result<PathBuf, String> {
    fn valid(path: &Path) -> bool {
        path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    if let Some(path) = std::env::var_os(variable) {
        let path = PathBuf::from(path);
        return if valid(&path) { Ok(path) } else { Err(format!("Decoder executable unavailable; install {fallback} and configure {variable}")) };
    }
    // Match the setup scripts' user-local prefix, including services launched from a
    // fresh terminal where the generated decoder environment has not been sourced.
    let prefix = std::env::var_os("WEBSA_DECODERS_PREFIX").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| {
        std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .map(|data| data.join("websa/decoders"))
    });
    std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join(fallback))
        .chain(prefix.map(|dir| dir.join("bin").join(fallback)))
        .find(|p| valid(p))
        .ok_or_else(|| format!("Install {fallback} with the decoder setup script, configure {variable}, or add it to PATH"))
}

pub(crate) struct Dependency {
    pub path: Option<PathBuf>,
    pub error: Option<String>,
    pub modes: Vec<String>,
}
impl Dependency {
    pub(crate) fn unavailable_reason(&self) -> Option<String> {
        self.error.clone().or_else(|| self.path.as_ref().and_then(|path| {
            (!path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
                .then(|| "Decoder executable is no longer available; check installation and restart the service".into())
        }))
    }
}
fn discover(variable: &str, name: &str, argument: &str) -> Dependency {
    let result = executable(variable, name).and_then(|path| {
        let text = process::probe(&path, argument, if name == "multimon-ng" {2} else {0})?;
        if !text.contains(name) || (name == "multimon-ng" && !text.lines().any(|line|
            line.strip_prefix("Available demodulators:").is_some_and(|modes| !modes.trim().is_empty()))) {
            return Err(format!("{name} capability probe failed; check installation and {variable}"));
        }
        Ok((path, text))
    });
    match result {
        Ok((path, text)) => Dependency { path: Some(path), error: None,
            modes: text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .filter(|s| !s.is_empty()).map(str::to_owned).collect() },
        Err(error) => Dependency { path: None, error: Some(error), modes: Vec::new() },
    }
}
pub(crate) fn multimon_dependency() -> &'static Dependency {
    static VALUE: LazyLock<Dependency> = LazyLock::new(|| discover("WEBSA_MULTIMON_PATH", "multimon-ng", "-h"));
    &VALUE
}
pub(crate) fn rtl433_dependency() -> &'static Dependency {
    static VALUE: LazyLock<Dependency> = LazyLock::new(|| discover("WEBSA_RTL433_PATH", "rtl_433", "-V"));
    &VALUE
}

pub enum ExternalDecoder {
    Multimon { process: Process, mode: String, previous: [f32; 2], has_previous: bool, dc_block: Option<DcBlock> },
    Rtl433 { process: Process },
    OnAir(onair::OnAirBridge),
    Rtty(rtty::RttyBridge),
    Gnss(gnss::GnssBridge),
    Dtmb(dtmb::DtmbBridge),
}
impl ExternalDecoder {
    pub fn buffer_status(&self)->Value {
        match self {
            Self::Multimon {process,..}|Self::Rtl433 {process}=>process.buffer_status(),
            Self::Rtty(bridge)=>bridge.buffer_status(),Self::Gnss(bridge)=>bridge.buffer_status(),
            Self::OnAir(bridge)=>bridge.buffer_status(),
            Self::Dtmb(bridge)=>bridge.buffer_status(),
        }
    }
    pub fn new(mode: &str, rate: u32, center_hz: u64) -> Result<Self, String> {
        let descriptor = crate::catalog::mode(mode).ok_or_else(|| format!("unknown mode {mode}"))?;
        if !descriptor.available { return Err(descriptor.unavailable_reason.unwrap_or_else(|| "decoder unavailable".into())); }
        match descriptor.provider.as_str() {
            "multimon" => {
                if rate != 22_050 { return Err("multimon requires exactly 22050 Hz discriminator input".into()); }
                let path = multimon_dependency().path.as_ref().ok_or("multimon dependency unavailable")?;
                let args = vec!["-q".into(), "-c".into(), "-t".into(), "raw".into(), "-a".into(), mode.to_ascii_uppercase(), "-".into()];
                // Carrier detuning becomes DC after FM discrimination. DTMF's tone-to-total
                // energy check includes that DC, so reject it before PCM quantization.
                // At 22050 Hz a 50 Hz pole settles a DC step to 1% in ~15 ms and
                // attenuates the lowest DTMF tone (697 Hz) by less than 0.1 dB.
                // Keep pager/FSK inputs raw: their near-DC content can carry data.
                let dc_block = (mode == "dtmf").then(|| DcBlock::new(
                    (-std::f64::consts::TAU * 50.0 / rate as f64).exp()));
                Ok(Self::Multimon { process: Process::spawn(path, &args)?, mode: mode.into(), previous: [0.0; 2], has_previous: false, dc_block })
            }
            "rtl433" => {
                if rate != 250_000 { return Err("rtl_433 requires exactly 250000 complex samples/s".into()); }
                let path = rtl433_dependency().path.as_ref().ok_or("rtl_433 dependency unavailable")?;
                // rtl_433 changes its default rate while parsing frequencies above 800 MHz.
                // Apply the exact input rate last, after all frequency-derived defaults.
                let args = rtl433_args(rate, center_hz);
                Ok(Self::Rtl433 { process: Process::spawn(path, &args)? })
            }
            "onair" => Ok(Self::OnAir(onair::OnAirBridge::spawn(mode, rate, center_hz)?)),
            "onair-rtty" => Ok(Self::Rtty(rtty::RttyBridge::spawn(mode, rate)?)),
            "gnss-sdr" => Ok(Self::Gnss(gnss::GnssBridge::spawn(rate, center_hz)?)),
            "dtmb" => Ok(Self::Dtmb(dtmb::DtmbBridge::spawn(rate)?)),
            _ => Err(format!("{mode} is not an external decoder")),
        }
    }
    pub fn push_iq(&mut self, iq: &[f32]) -> Result<Vec<ExternalDecodeEvent>, String> {
        match self {
            Self::Multimon { process, mode, previous, has_previous, dc_block } => {
                let mut bytes = process.buffer();
                bytes.reserve(iq.len());
                for pair in iq.chunks_exact(2) {
                    // Phase discriminator: no AGC, de-emphasis, squelch, NR or listener gain.
                    let sample = if *has_previous {
                        let re = pair[0] * previous[0] + pair[1] * previous[1];
                        let im = pair[1] * previous[0] - pair[0] * previous[1];
                        let phase = im.atan2(re) / std::f32::consts::PI;
                        let audio = match dc_block {
                            Some(filter) => filter.process_sample(phase),
                            None => phase,
                        };
                        (audio * 32767.0).round() as i16
                    } else { 0 };
                    *previous = [pair[0], pair[1]];
                    *has_previous = true;
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
                let lines = process.push(bytes)?;
                decode_lines("multimon", mode, lines)
            }
            Self::Rtl433 { process } => {
                let mut bytes = process.buffer();
                bytes.reserve(iq.len() * 4);
                for sample in iq { bytes.extend_from_slice(&sample.to_le_bytes()); }
                decode_lines("rtl433", "ism", process.push(bytes)?)
            }
            Self::OnAir(bridge) => bridge.push_iq(iq),
            Self::Rtty(bridge) => bridge.push_iq(iq),
            Self::Gnss(bridge) => bridge.push_iq(iq),
            Self::Dtmb(bridge) => bridge.push_iq(iq),
        }
    }
    /// Live replacement drains admitted input and available results without an EOF wait.
    pub fn retire(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        let (provider, mode, mut events) = match self {
            Self::OnAir(bridge) => return bridge.retire(),
            Self::Rtty(bridge) => return bridge.retire(),
            Self::Gnss(bridge) => return bridge.retire(),
            Self::Dtmb(bridge) => return bridge.retire(),
            Self::Multimon { process, mode, .. } => ("multimon", mode.as_str(), decode_lines("multimon", mode, process.retire()?)?),
            Self::Rtl433 { process } => ("rtl433", "ism", decode_lines("rtl433", "ism", process.retire()?)?),
        };
        events.push(ExternalDecodeEvent { provider:provider.into(), mode:mode.into(), kind:"status".into(),
            data:json!({"type":"status","state":"stopped","reason":"reconfigured","upstreamCompletionKnown":false,
                "message":"Live decoder reconfigured after writing admitted input and draining available results; pending upstream decoding is not confirmed complete"}) });
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        match self {
            Self::Multimon { process, mode, .. } => decode_lines("multimon", mode, process.finish()?),
            Self::Rtl433 { process } => decode_lines("rtl433", "ism", process.finish()?),
            Self::OnAir(bridge) => bridge.finish(),
            Self::Rtty(bridge) => bridge.finish(),
            Self::Gnss(bridge) => bridge.finish(),
            Self::Dtmb(bridge) => bridge.finish(),
        }
    }
}
fn rtl433_args(rate: u32, center_hz: u64) -> Vec<String> {
    vec!["-c".into(), "0".into(), "-r".into(), "cf32:-".into(),
        "-f".into(), center_hz.to_string(), "-s".into(), rate.to_string(), "-F".into(), "json".into()]
}
fn decode_lines(provider: &str, mode: &str, lines: Vec<String>) -> Result<Vec<ExternalDecodeEvent>, String> {
    lines.into_iter().filter(|s| !s.trim().is_empty()).map(|line| {
        let (kind, data) = if provider == "rtl433" {
            let data: Value = serde_json::from_str(&line).map_err(|e| format!("rtl_433 malformed JSON: {e}"))?;
            if !data.is_object() { return Err("rtl_433 result must be a JSON object".into()); }
            ("table", data)
        } else { ("text", json!({"text":line})) };
        Ok(ExternalDecodeEvent { provider: provider.into(), mode: mode.into(), kind: kind.into(), data })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requires the real consumer; run explicitly when multimon-ng is installed.
    /// The carrier offsets dominate the two tones' combined discriminator energy.
    #[test]
    #[ignore = "requires multimon-ng; exercises live decoding before EOF"]
    fn dtmf_decodes_detuned_fm_before_eof() {
        const RATE: usize = 22_050;
        for carrier_hz in [-1500.0, 0.0, 1500.0] {
            let mut decoder = ExternalDecoder::new("dtmf", RATE as u32, 145_500_000)
                .expect("install multimon-ng or set WEBSA_MULTIMON_PATH to run this regression");
            let mut iq = Vec::new();
            let mut phase = 0.0_f64;
            let mut append = |duration: f64, tones: Option<(f64, f64)>| {
                for n in 0..(duration * RATE as f64).round() as usize {
                    let t = n as f64 / RATE as f64;
                    let deviation = tones.map_or(0.0, |(low, high)| {
                        500.0 * ((std::f64::consts::TAU * low * t).sin()
                            + (std::f64::consts::TAU * high * t).sin())
                    });
                    phase = (phase + std::f64::consts::TAU * (carrier_hz + deviation)
                        / RATE as f64) % std::f64::consts::TAU;
                    iq.extend_from_slice(&[phase.cos() as f32, phase.sin() as f32]);
                }
            };
            append(0.3, None);
            for tones in [(770.0, 1336.0), (697.0, 1336.0), (941.0, 1477.0)] {
                append(0.2, Some(tones));
                append(0.2, None);
            }
            append(0.5, None);
            let mut events = Vec::new();
            // Non-tone-aligned chunks exercise discriminator/filter state at boundaries.
            for chunk in iq.chunks(4096 * 2) {
                events.extend(decoder.push_iq(chunk).unwrap());
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                events.extend(decoder.push_iq(&[]).unwrap());
                let digits: Vec<_> = events.iter().filter_map(|event| {
                    event.data["text"].as_str()?.strip_prefix("DTMF: ").map(str::trim)
                }).collect();
                if digits.len() >= 3 || std::time::Instant::now() >= deadline {
                    assert_eq!(digits, ["5", "2", "#"], "carrier {carrier_hz} Hz, before EOF");
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            decoder.finish().unwrap();
        }
    }
    #[test]
    fn rtl433_preserves_fields_and_rejects_malformed_records() {
        let events = decode_lines("rtl433", "ism", vec![r#"{"model":"sensor","id":7,"temperature_C":12.5}"#.into()]).unwrap();
        assert_eq!(events[0].data["temperature_C"], 12.5);
        assert!(decode_lines("rtl433", "ism", vec!["[]".into()]).is_err());
        assert!(decode_lines("rtl433", "ism", vec!["truncated{".into()]).is_err());
    }
}
