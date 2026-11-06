//! MIT process adapter; GNSS-SDR remains a separately installed GPL executable.
use super::{executable, process::{self, Process}, Dependency, ExternalDecodeEvent};
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

fn dependency() -> &'static Dependency {
    static DEP: LazyLock<Dependency> = LazyLock::new(|| {
        let result = executable("WEBSA_GNSS_BRIDGE_PATH", "websa-gnss").and_then(|path| {
            let banner = process::probe_with_timeout(&path, "--version", 0, Duration::from_secs(7))?;
            if !banner.contains("websa-gnss protocol=1 iq=cf32le rate=4000000") {
                return Err("GNSS bridge protocol mismatch; run tools/setup-gnss-decoder.sh".into());
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
pub const NOMINAL_CARRIER_HZ: u64 = 1_575_420_000;

/// Validate a receive-only translated bench frequency without changing the GPS signal model.
pub fn validate_center_hz(center_hz: u64, bench: Option<&str>) -> Result<bool, String> {
    let bench_hz = bench.map(|text| {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("WEBSA_GPS_BENCH_HZ must be an integer from 400000000 through 450000000".to_owned());
        }
        let hz = text.parse::<u64>().map_err(|_| "WEBSA_GPS_BENCH_HZ integer is out of range".to_owned())?;
        if !(400_000_000..=450_000_000).contains(&hz) {
            return Err("WEBSA_GPS_BENCH_HZ must be from 400000000 through 450000000 Hz".to_owned());
        }
        Ok(hz)
    }).transpose()?;
    if center_hz == NOMINAL_CARRIER_HZ { return Ok(false); }
    if bench_hz == Some(center_hz) { return Ok(true); }
    Err("GPS requires 1575420000 Hz or the exact receive bench frequency explicitly configured by WEBSA_GPS_BENCH_HZ".into())
}

fn bench_setting() -> Result<Option<String>, String> {
    match std::env::var("WEBSA_GPS_BENCH_HZ") {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("invalid WEBSA_GPS_BENCH_HZ: {error}")),
    }
}

pub fn unavailable_reason() -> Option<String> {
    dependency().unavailable_reason().or_else(|| bench_setting()
        .and_then(|setting| validate_center_hz(NOMINAL_CARRIER_HZ, setting.as_deref())).err())
}

pub struct GnssBridge { process: Process, pending: Vec<ExternalDecodeEvent> }
impl GnssBridge {
    pub fn buffer_status(&self)->serde_json::Value {self.process.buffer_status()}
    pub fn spawn(rate: u32, center_hz: u64) -> Result<Self, String> {
        if rate != 4_000_000 { return Err("GPS requires exactly 4000000 complex samples/s".into()); }
        validate_center_hz(center_hz, bench_setting()?.as_deref())?;
        if let Some(error) = unavailable_reason() { return Err(error); }
        let path = dependency().path.as_ref().ok_or("GNSS bridge unavailable")?;
        let mut process = Process::spawn(path, &["--center-hz".into(), center_hz.to_string()])?;
        let mut pending: Vec<ExternalDecodeEvent> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let events = Self::events(process.push(Vec::new())?)?;
            let ready = events.iter().any(|event| event.kind == "status"
                && event.data.get("state").and_then(Value::as_str) == Some("ready"));
            for event in events {
                // Readiness may take seconds; retain only each current snapshot.
                let table = event.data.get("table").and_then(Value::as_str);
                if event.kind == "table" && matches!(table, Some("satellites" | "pvt")) {
                    if let Some(previous) = pending.iter_mut().find(|previous|
                        previous.kind == "table" && previous.data.get("table").and_then(Value::as_str) == table) {
                        *previous = event;
                        continue;
                    }
                }
                pending.push(event);
            }
            if ready { return Ok(Self { process, pending }); }
            if pending.len() > 64 { return Err("GNSS startup emitted excessive records".into()); }
            if Instant::now() >= deadline { return Err("GNSS-SDR did not open its IQ FIFO within 20 seconds".into()); }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn events(lines: Vec<String>) -> Result<Vec<ExternalDecodeEvent>, String> {
        lines.into_iter().map(|line| {
            let mut data: Value = serde_json::from_str(&line).map_err(|e| format!("GNSS malformed JSON: {e}"))?;
            let kind = data.get("kind").and_then(Value::as_str).ok_or("GNSS result lacks kind")?.to_owned();
            if !matches!(kind.as_str(), "table" | "status") { return Err("GNSS unsupported result kind".into()); }
            data.as_object_mut().ok_or("GNSS result must be an object")?.remove("kind");
            Ok(ExternalDecodeEvent { provider: "gnss-sdr".into(), mode: "gps".into(), kind, data })
        }).collect()
    }
    pub fn push_iq(&mut self, iq: &[f32]) -> Result<Vec<ExternalDecodeEvent>, String> {
        let mut bytes = self.process.buffer();
        bytes.reserve(iq.len() * 4);
        for sample in iq { bytes.extend_from_slice(&sample.to_le_bytes()); }
        let mut events = std::mem::take(&mut self.pending);
        events.extend(Self::events(self.process.push(bytes)?)?);
        Ok(events)
    }
    pub fn retire(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        let mut events = std::mem::take(&mut self.pending);
        events.extend(Self::events(self.process.retire()?)?);
        events.push(ExternalDecodeEvent { provider: "gnss-sdr".into(), mode: "gps".into(), kind: "status".into(),
            data: json!({"state":"stopped","reason":"reconfigured","fix":{"valid":false},"upstreamCompletionKnown":false}) });
        Ok(events)
    }
    pub fn finish(&mut self) -> Result<Vec<ExternalDecodeEvent>, String> {
        let mut events = std::mem::take(&mut self.pending);
        events.extend(Self::events(self.process.finish()?)?);
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translated_receive_requires_exact_explicit_bench_frequency() {
        assert_eq!(validate_center_hz(NOMINAL_CARRIER_HZ, None), Ok(false));
        assert!(validate_center_hz(435_000_000, None).is_err());
        assert_eq!(validate_center_hz(435_000_000, Some("435000000")), Ok(true));
        assert_eq!(validate_center_hz(NOMINAL_CARRIER_HZ, Some("435000000")), Ok(false));
        assert!(validate_center_hz(434_000_000, Some("435000000")).is_err());
        for boundary in [400_000_000, 450_000_000] {
            assert_eq!(validate_center_hz(boundary, Some(&boundary.to_string())), Ok(true));
        }
    }

    #[test]
    fn malformed_bench_configuration_is_never_silently_ignored() {
        for value in ["", " 435000000", "+435000000", "435000000.0", "399999999", "450000001", "1575420000", "18446744073709551616"] {
            assert!(validate_center_hz(NOMINAL_CARRIER_HZ, Some(value)).is_err(), "{value}");
        }
    }
}
