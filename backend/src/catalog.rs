//! Service receiver metadata extends the native DSP kernel registry.
use serde::Serialize;
use serde_json::{json, Value};
use websa_dsp::plugin::{plugins, PluginKind};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeInfo {
    pub id: String, pub kind: String, pub label: String, pub category: String,
    pub description: String, pub aliases: Vec<String>, pub provider: String,
    pub implemented: bool, pub available: bool, pub unavailable_reason: Option<String>,
    pub audio_enhancement: bool,
    pub startup_timeout_ms: u32,
    pub default_bandwidth_hz: f64, pub min_bandwidth_hz: f64, pub max_bandwidth_hz: f64,
    pub bandwidth_presets_hz: Vec<f64>, pub bandwidth_editable: bool,
    pub input_rate_hz: Option<u32>, pub minimum_rate_hz: u32,
    pub audio_companion: Option<String>, pub sideband: String, pub outputs: Vec<String>, pub common: bool,
}
// Only demodulators with a supported raw FM-discriminator input are included. SCOPE,
// X10 and other incompatible/diagnostic upstream modules are not RF modes.
const MULTIMON: &[(&str, &str, &str)] = &[
    ("pocsag512", "POCSAG 512", "paging"), ("pocsag1200", "POCSAG 1200", "paging"),
    ("pocsag2400", "POCSAG 2400", "paging"), ("flex", "FLEX", "paging"),
    ("flex_next", "FLEX next", "paging"), ("eas", "EAS / SAME", "weather"),
    ("afsk1200", "AFSK 1200 / AX.25", "packet"), ("afsk2400", "AFSK 2400", "packet"),
    ("afsk2400_2", "AFSK 2400 alternate", "packet"), ("afsk2400_3", "AFSK 2400 HAPN", "packet"),
    ("hapn4800", "HAPN 4800", "packet"), ("fsk9600", "FSK 9600", "packet"),
    ("dtmf", "DTMF", "tones"), ("zvei1", "ZVEI 1", "selective"),
    ("zvei2", "ZVEI 2", "selective"), ("zvei3", "ZVEI 3", "selective"),
    ("dzvei", "DZVEI", "selective"), ("pzvei", "PZVEI", "selective"),
    ("eea", "EEA", "selective"), ("eia", "EIA", "selective"),
    ("ccir", "CCIR", "selective"), ("morse_cw", "Morse over FM", "tones"),
    ("ufsk1200", "UFSK 1200", "packet"), ("clipfsk", "CLIP FSK", "packet"),
];
pub fn is_external(mode: &str) -> bool {
    mode == "ism" || matches!(mode, "ais" | "aprs" | "ft4" | "wspr" | "meteor" | "adsb" | "rtty" | "rtty-reverse" | "gps")
        || MULTIMON.iter().any(|(id, _, _)| *id == mode)
}
/// Exact adapter rate and minimum channel IQ cadence, independent of dependency discovery.
pub fn external_rates(mode: &str) -> Option<(u32, u32)> {
    if MULTIMON.iter().any(|(id, _, _)| *id == mode) { return Some((22_050, 22_050)); }
    if matches!(mode, "rtty" | "rtty-reverse") { return Some((48_000, 48_000)); }
    if mode == "gps" { return Some((4_000_000, 2_600_000)); }
    match mode { "ism" | "ais" | "aprs" | "ft4" | "wspr" => Some((250_000, 250_000)),
        "meteor" => Some((288_000, 288_000)), "adsb" => Some((2_000_000, 2_500_000)), _ => None }
}
fn base(id: &str, label: &str, category: &str, bandwidth: f64) -> ModeInfo {
    ModeInfo { id: id.into(), kind: "digital".into(), label: label.into(), category: category.into(),
        description: label.into(), aliases: Vec::new(), provider: "native".into(),
        implemented: true, available: true, unavailable_reason: None, audio_enhancement: false,
        startup_timeout_ms: 3_000,
        default_bandwidth_hz: bandwidth, min_bandwidth_hz: bandwidth, max_bandwidth_hz: bandwidth,
        bandwidth_presets_hz: vec![bandwidth], bandwidth_editable: false,
        input_rate_hz: None, minimum_rate_hz: 0, audio_companion: None,
        sideband: "symmetric".into(), outputs: vec!["audio".into()], common: false }
}
pub fn mode(id: &str) -> Option<ModeInfo> {
    let native = plugins(PluginKind::Analog).iter().chain(plugins(PluginKind::Digital)).find(|p| p.id == id);
    if let Some(plugin) = native {
        let (label, category, bandwidth) = match id {
            "am" => ("AM", "analog", 6_000.0), "dsb" => ("DSB", "analog", 6_000.0),
            "usb" => ("USB", "analog", 2_400.0), "lsb" => ("LSB", "analog", 2_400.0),
            "cw" => ("CW / Morse", "amateur", 500.0), "nfm" => ("Narrow FM", "analog", 12_000.0),
            "wfm" => ("Broadcast FM / RDS", "broadcast", 180_000.0), "pm" => ("Phase modulation", "analog", 12_000.0),
            "ft8" => ("FT8", "amateur", 3_000.0), "sstv" => ("SSTV", "amateur", 3_000.0),
            "radiofax" => ("Radiofax / WEFAX", "weather", 3_000.0), "drm" => ("DRM", "broadcast", 12_000.0),
            "drmplus" => ("DRM+", "broadcast", 100_000.0), _ => return None,
        };
        let mut info = base(id, label, category, bandwidth);
        info.implemented = plugin.implemented; info.available = plugin.implemented;
        info.audio_enhancement = plugin.audio_enhancement;
        info.min_bandwidth_hz = if matches!(id, "usb" | "lsb") {101.0} else {50.0};
        info.kind = if plugin.kind == PluginKind::Analog { "analog" } else { "digital" }.into();
        info.common = matches!(id, "am" | "usb" | "lsb" | "cw" | "nfm" | "wfm" | "ft8" | "sstv");
        info.bandwidth_editable = plugin.kind == PluginKind::Analog;
        if info.bandwidth_editable {
            // Absolute integer-rate ceiling; STATUS supplies the tighter capture/PFB bound.
            info.max_bandwidth_hz = if matches!(id, "usb" | "lsb") {390_625.0} else {781_250.0};
            info.bandwidth_presets_hz = match id {
                "cw" => vec![100.0, 200.0, 500.0, 1000.0, 2000.0],
                "usb" | "lsb" => vec![1800.0, 2400.0, 2700.0, 3000.0, 6000.0],
                "wfm" => vec![100_000.0, 150_000.0, 180_000.0, 200_000.0, 250_000.0],
                "nfm" => vec![9000.0, 12_000.0, 12_500.0, 15_000.0, 25_000.0],
                "pm" => vec![6000.0, 9000.0, 12_000.0, 15_000.0, 25_000.0],
                _ => vec![3000.0, 4500.0, 6000.0, 9000.0, 12_000.0],
            };
        }
        info.input_rate_hz = match id { "ft8" | "radiofax" => Some(12_000), "sstv" => Some(8_000),
            "drm" => Some(48_000), "drmplus" => Some(96_000), _ => None };
        info.minimum_rate_hz = match id { "wfm" => 300_000, "ft8" => 6000, "sstv" => 8000, "radiofax" => 12000, _ => 0 };
        info.audio_companion = if matches!(id, "drm" | "drmplus") { None }
            else { Some(if matches!(id, "ft8" | "sstv" | "radiofax") {"usb"} else {id}.into()) };
        info.sideband = match id { "usb" | "ft8" | "sstv" | "radiofax" => "upper", "lsb" => "lower", _ => "symmetric" }.into();
        if matches!(id, "am" | "cw" | "ft8" | "wfm" | "drm" | "drmplus") { info.outputs.push("text".into()); }
        if matches!(id, "sstv" | "radiofax") { info.outputs.push("image".into()); }
        info.aliases = match id { "radiofax" => vec!["wefax".into(), "weather fax".into()],
            "nfm" => vec!["fm".into()], "wfm" => vec!["rds".into()], "cw" => vec!["morse".into()], _ => Vec::new() };
        return Some(info);
    }
    if let Some((_, label, category)) = MULTIMON.iter().find(|(mode, _, _)| *mode == id) {
        let mut info = base(id, label, category, 12_500.0);
        info.provider = "multimon".into(); info.input_rate_hz = Some(22_050); info.minimum_rate_hz = 22_050;
        info.startup_timeout_ms = 5_000;
        info.audio_companion = Some("nfm".into()); info.outputs = vec!["text".into()];
        info.description = format!("{label} from raw FM discriminator audio using multimon-ng");
        let dep = crate::external::multimon_dependency();
        info.unavailable_reason = dep.unavailable_reason().or_else(|| (!dep.modes.iter().any(|m| m == &id.to_ascii_uppercase()))
            .then(|| format!("installed multimon-ng does not advertise {}", id.to_ascii_uppercase())));
        info.available = info.unavailable_reason.is_none();
        return Some(info);
    }
    if id == "ism" {
        let mut info = base(id, "ISM sensors / rtl_433", "ism", 200_000.0);
        info.provider = "rtl433".into(); info.input_rate_hz = Some(250_000); info.minimum_rate_hz = 250_000;
        info.startup_timeout_ms = 5_000;
        info.outputs = vec!["table".into()]; info.aliases = vec!["433 MHz".into(), "868 MHz".into(), "sensors".into()];
        info.description = "Automatic ISM device protocol recognition; supported devices depend on installed rtl_433".into();
        info.unavailable_reason = crate::external::rtl433_dependency().unavailable_reason();
        info.available = info.unavailable_reason.is_none(); return Some(info);
    }
    if matches!(id, "rtty" | "rtty-reverse") {
        let reverse = id == "rtty-reverse";
        let mut info = base(id, if reverse { "RTTY (reverse)" } else { "RTTY" }, "amateur", 3_400.0);
        info.provider = "onair-rtty".into(); info.input_rate_hz = Some(48_000); info.minimum_rate_hz = 48_000;
        info.startup_timeout_ms = 5_000;
        info.audio_companion = Some("usb".into()); info.sideband = "upper".into();
        info.outputs = vec!["text".into(), "table".into()];
        info.description = format!("ITA2 Baudot via independent OnAir RTTY companion; raw USB, automatic tone-pair acquisition, {} mark tone. Default 45.45 baud / 170 Hz shift", if reverse { "upper" } else { "lower" });
        info.unavailable_reason = crate::external::rtty::unavailable_reason();
        info.available = info.unavailable_reason.is_none();
        return Some(info);
    }
    if id == "gps" {
        let mut info = base(id, "GPS L1 C/A", "satellite", 2_046_000.0);
        info.provider = "gnss-sdr".into();
        info.startup_timeout_ms = 25_000;
        info.input_rate_hz = Some(4_000_000); info.minimum_rate_hz = 2_600_000;
        info.outputs = vec!["table".into(), "status".into()];
        info.description = "GPS L1 C/A acquisition, tracking, navigation and PVT from SDR IQ using independent GNSS-SDR".into();
        info.aliases = vec!["GNSS".into(), "1575.42 MHz".into()];
        info.unavailable_reason = crate::external::gnss::unavailable_reason();
        info.available = info.unavailable_reason.is_none();
        return Some(info);
    }
    if matches!(id, "ais" | "aprs" | "ft4" | "wspr" | "meteor" | "adsb") {
        let cap = crate::external::onair::capabilities().into_iter().find(|c| c.mode == id)?;
        let (label, category) = match id { "ais" => ("AIS", "maritime"), "aprs" => ("APRS", "packet"),
            "ft4" => ("FT4", "amateur"), "wspr" => ("WSPR", "amateur"), "adsb" => ("ADS-B / Mode S", "aviation"),
            _ => ("Meteor LRPT", "satellite") };
        let mut info = base(id, label, category, cap.bandwidth_hz as f64);
        info.provider = "onair".into(); info.available = cap.available; info.unavailable_reason = cap.unavailable_reason;
        info.startup_timeout_ms = 15_000;
        info.input_rate_hz = Some(cap.input_rate_hz); info.minimum_rate_hz = cap.minimum_rate_hz;
        info.outputs = match id { "meteor" => vec!["image".into(), "table".into()],
            "adsb" => vec!["table".into()], _ => vec!["table".into(), "text".into()] };
        if matches!(id, "ft4" | "wspr") { info.sideband = "upper".into(); info.audio_companion = Some("usb".into()); }
        if id == "aprs" { info.audio_companion = Some("nfm".into()); }
        info.description = format!("{label} via isolated OnAir server; runtime Web capability required");
        if id == "adsb" {
            info.description = "1090 MHz ADS-B / Mode S aircraft identity, callsign, altitude and position via isolated OnAir; 2 MS/s decoder input, receiver rate at least 2.5 MS/s".into();
            info.aliases = vec!["ADS-B".into(), "Mode S".into(), "1090 MHz".into()];
        }
        return Some(info);
    }
    None
}
pub fn validate(mode_id: &str, bandwidth: f64) -> Result<(), String> {
    let info = mode(mode_id).ok_or_else(|| format!("unsupported receiver mode {mode_id}"))?;
    if !info.available { return Err(info.unavailable_reason.unwrap_or_else(|| format!("{mode_id} unavailable"))); }
    // Native geometry retains its established admission rules; external contracts are fixed.
    if is_external(mode_id) && (!bandwidth.is_finite() || bandwidth < info.min_bandwidth_hz || bandwidth > info.max_bandwidth_hz) {
        return Err(format!("{mode_id} requires filter bandwidth {} Hz", info.default_bandwidth_hz));
    }
    Ok(())
}
pub fn manifest() -> Vec<Value> {
    let mut result = Vec::new();
    for kind in [PluginKind::Analog, PluginKind::Digital] {
        for plugin in plugins(kind) { if let Some(info) = mode(plugin.id) { result.push(json!(info)); } }
    }
    for id in MULTIMON.iter().map(|(id, _, _)| *id).chain(["ism", "ais", "aprs", "ft4", "wspr", "meteor", "adsb", "rtty", "rtty-reverse", "gps"]) {
        if let Some(info) = mode(id) { result.push(json!(info)); }
    }
    for (kind, family) in [("audio", PluginKind::Audio), ("ddc", PluginKind::Ddc)] {
        result.extend(plugins(family).iter().map(|p| json!({"kind":kind,"id":p.id,
            "implemented":p.implemented,"audioEnhancement":p.audio_enhancement})));
    }
    result
}
