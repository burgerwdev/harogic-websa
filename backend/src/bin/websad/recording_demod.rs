//! Bounded offline analogue demodulation of checked IQ. No receiver slot, live UID
//! or acquisition state is allocated. A completed render is an immutable snapshot.
use super::recording_read::{visit_range, Selection};
use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Write},
    path::Path as FsPath,
};
use websa_dsp::{
    analog::{spec_for, AnalogDemod},
    ddc::Nco,
    plugin::{AnalogDemodulator, DemodConfig},
};

const MAX_SAMPLES: u64 = 1_048_576;
const MAX_SECONDS: f64 = 60.0;
const RATE: u32 = 48_000;
const MAX_PCM: usize = RATE as usize * 60 * 4 + 8;
const MAX_META: usize = 4 * 1024 * 1024;
pub(super) const MODES: [&str; 4] = ["am", "usb", "lsb", "nfm"];

fn invalid(message: impl ToString) -> io::Error {
    io::Error::other(message.to_string())
}
fn cancelled(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "file demodulation cancelled",
        ))
    } else {
        Ok(())
    }
}
struct Request {
    selection: Selection,
    mode: String,
    bandwidth: f64,
    offset: f64,
}
impl Request {
    fn parse(params: &HashMap<String, String>) -> io::Result<Self> {
        if params.keys().any(|key| {
            !matches!(
                key.as_str(),
                "segment" | "start" | "count" | "mode" | "if_bw" | "offset_hz"
            )
        }) {
            return Err(invalid("unknown file demodulation parameter"));
        }
        let selection = Selection::parse_limit(params, MAX_SAMPLES)?;
        let mode = params
            .get("mode")
            .filter(|s| MODES.contains(&s.as_str()))
            .ok_or_else(|| invalid("mode must be am, usb, lsb or nfm"))?
            .clone();
        let bandwidth: f64 = params
            .get("if_bw")
            .ok_or_else(|| invalid("if_bw is required"))?
            .parse()
            .map_err(invalid)?;
        let minimum = if matches!(mode.as_str(), "usb" | "lsb") {
            101.0
        } else {
            50.0
        };
        if !bandwidth.is_finite() || !(minimum..=20_000.0).contains(&bandwidth) {
            return Err(invalid(format!("if_bw must be {minimum}..20000 Hz")));
        }
        let offset: f64 = params
            .get("offset_hz")
            .map_or(Ok(0.0), |s| s.parse())
            .map_err(invalid)?;
        if !offset.is_finite() {
            return Err(invalid("offset_hz must be finite"));
        }
        Ok(Self {
            selection,
            mode,
            bandwidth,
            offset,
        })
    }
}
struct Render {
    wav: Vec<u8>,
    source: Vec<u8>,
    manifest: Vec<u8>,
}
impl Render {
    fn members(&self) -> [(&str, &[u8]); 3] {
        [
            ("render.json", &self.manifest),
            ("source.sigmf-meta", &self.source),
            ("audio.wav", &self.wav),
        ]
    }
    fn length(&self) -> u64 {
        1024 + self
            .members()
            .iter()
            .map(|(_, bytes)| 512 + (bytes.len() as u64).div_ceil(512) * 512)
            .sum::<u64>()
    }
    fn emit(&self, output: &mut dyn Write, cancel: &AtomicBool) -> io::Result<()> {
        for (name, bytes) in self.members() {
            cancelled(cancel)?;
            super::recording_export::header(output, name, bytes.len() as u64)?;
            output.write_all(bytes)?;
            output.write_all(&[0; 512][..(512 - bytes.len() % 512) % 512])?;
        }
        output.write_all(&[0; 1024])
    }
}
fn render(
    path: &FsPath,
    task: &Value,
    request: &Request,
    cancel: &AtomicBool,
) -> io::Result<Render> {
    if task["tracks"][0]["kind"] != "iq" {
        return Err(invalid("file demodulation requires an IQ task"));
    }
    let segment = task["tracks"][0]["segments"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|s| s["segment_id"] == request.selection.segment)
        })
        .ok_or_else(|| invalid("unknown recording segment"))?;
    let rate = segment["actual_rate_hz"]
        .as_f64()
        .ok_or_else(|| invalid("missing IQ rate"))?;
    let passband = segment["baseband"]["passband_hz"]
        .as_f64()
        .ok_or_else(|| invalid("missing recorded passband"))?;
    let spec = spec_for(&request.mode).ok_or_else(|| invalid("analogue kernel unavailable"))?;
    let (low, high) = spec.band_hz(request.bandwidth, 700.0);
    let center = segment["frequency_hz"]
        .as_f64()
        .ok_or_else(|| invalid("missing recorded frequency"))?;
    let tuned = center + request.offset;
    let selected_band = [request.offset + low, request.offset + high];
    let retained_edge = (passband / 2.0).min(rate / 2.0);
    // The original IQ is a symmetric selected baseband. Reject requests for spectrum
    // it never retained and avoid the kernel's legacy bandwidth clamp.
    if !rate.is_finite()
        || !(1_000.0..=192_000.0).contains(&rate)
        || !passband.is_finite()
        || passband <= 0.0
        || !center.is_finite()
        || !tuned.is_finite()
        || tuned < 0.0
        || !request.offset.is_finite()
        || request.selection.count as f64 / rate > MAX_SECONDS
        || request.bandwidth > rate * 0.45
        || selected_band[0] < -retained_edge
        || selected_band[1] > retained_edge
    {
        return Err(invalid("range must be <=60 seconds; shifted filter must fit recorded passband and IQ Nyquist; bandwidth <=0.45 * IQ rate (1000..192000 S/s)"));
    }
    cancelled(cancel)?;
    let mut demod = AnalogDemod::new(spec, DemodConfig::new(rate, request.bandwidth), RATE as f64);
    demod.set_agc_enabled(false);
    let mut mixer = Nco::new();
    mixer.set_frequency(request.offset, rate);
    let mut mixed = Vec::new();
    let mut pcm = Vec::new();
    let mut annotations = Vec::new();
    let mut metadata_bytes = 0;
    let mut source_hash = Sha256::new();
    let mut iq = Vec::new();
    let mut audio = Vec::new();
    let mut source = visit_range(path, task, &request.selection, cancel, |raw, annotation| {
        cancelled(cancel)?;
        metadata_bytes += serde_json::to_vec(annotation)?.len();
        if annotations.len() >= 4096 || metadata_bytes > MAX_META {
            return Err(invalid(
                "file demodulation provenance exceeds 4096 blocks / 4 MiB",
            ));
        }
        annotations.push(annotation.clone());
        source_hash.update(raw);
        // Fixed short turns bound native scratch memory and cancellation latency.
        // State carries across original block/volume boundaries without inserting samples.
        for block in raw.chunks(8192 * 8) {
            cancelled(cancel)?;
            iq.clear();
            iq.extend(
                block
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap())),
            );
            if iq.iter().any(|s| !s.is_finite()) {
                return Err(invalid("file IQ must be finite"));
            }
            if request.offset != 0.0 {
                mixed.clear();
                mixed.extend(iq.iter().map(|&s| f64::from(s)));
                mixer.mix_exact(&mut mixed);
                for (sample, &value) in iq.iter_mut().zip(&mixed) {
                    *sample = value as f32;
                }
                if iq.iter().any(|s| !s.is_finite()) {
                    return Err(invalid("mixed file IQ must be finite"));
                }
            }
            demod.process_into(&iq, &mut audio);
            if pcm.len() + audio.len() * 4 > MAX_PCM || audio.iter().any(|s| !s.is_finite()) {
                return Err(invalid(
                    "file demodulation PCM exceeded finite / 60 second bounds",
                ));
            }
            pcm.extend(audio.iter().flat_map(|sample| sample.to_le_bytes()));
        }
        Ok(())
    })?;
    if pcm.is_empty() {
        return Err(invalid("selected range is too short to produce audio"));
    }
    source["annotations"] = json!(annotations);
    let source = serde_json::to_vec(&source)?;
    if source.len() > MAX_META {
        return Err(invalid("file demodulation provenance exceeds 4 MiB"));
    }
    // IEEE Float32 WAV with fact; no integer quantization or silent tail padding.
    let frames = pcm.len() / 4;
    let mut wav = Vec::with_capacity(56 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(48 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&3u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&32u16.to_le_bytes());
    wav.extend_from_slice(b"fact");
    wav.extend_from_slice(&4u32.to_le_bytes());
    wav.extend_from_slice(&(frames as u32).to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    let manifest = serde_json::to_vec(&json!({"format":"websa-iq-demod-v1", "task":task,
        "selection":{"segment":request.selection.segment,"start":request.selection.start.to_string(),"count":request.selection.count.to_string(),"iq_rate_hz":rate},
        "processing":{"kernel":"websa_dsp::analog::AnalogDemod","mode":request.mode,"if_bw_hz":request.bandwidth,
            "tuning":{"offset_hz":request.offset,"recorded_center_hz":center,"frequency_hz":tuned,
                "selected_band_hz":selected_band,"mixer":"websa_dsp::ddc::Nco",
                "operation":"IQ * exp(-j * 2*pi * offset_hz * n / actual_iq_rate_hz)",
                "phase_origin":"zero at selection start; continuous across blocks and volumes; zero offset bypasses mixing"},
            "band_hz":[low,high],"agc":false,"am_sync":false,"gain":1,"nr":false,"squelch":false,
            "deemphasis_us":demod.deemph_us(),"resampling":"native stateful linear interpolation after IF and audio FIR"},
        "audio":{"file":"audio.wav","format":"f32_le","rate_hz":RATE,"channels":1,"samples":frames.to_string(),
            "pcm_sha256":format!("{:x}",Sha256::digest(&pcm))},
        "source":{"file":"source.sigmf-meta","cf32_sha256":format!("{:x}",source_hash.finalize()),
            "metadata_sha256":format!("{:x}",Sha256::digest(&source)),"original_unchanged":true},
        "time":{"origin":"selected range sample zero","pcm_seconds":"audio sample index / 48000",
            "source_mapping":"original per-block clocks and decimal trim offsets in source.sigmf-meta; no f64 conversion of original u64 positions",
            "utc_reliable":false,"delay_seconds":null,
            "boundary":"fresh causal filters at selection start; startup transient retained; no pre-roll, tail flush, gap filling or cross-segment stitching; end limited to samples actually emitted"},
        "local_controls":"live monitor controls excluded; offline player volume affects playback only"}))?;
    if manifest.len() > 2 * 1024 * 1024 {
        return Err(invalid("file render manifest exceeds 2 MiB"));
    }
    Ok(Render {
        wav,
        source,
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use websa_channelizer::iq_archive::{v2, IqBlockMeta};

    fn fixture(
        mode: &str,
        rate: f64,
        samples: usize,
    ) -> (std::path::PathBuf, Value, Request, Vec<u8>) {
        tuned_fixture(mode, rate, samples, 0.0, 8192)
    }
    fn tuned_fixture(
        mode: &str,
        rate: f64,
        samples: usize,
        offset: f64,
        block_samples: usize,
    ) -> (std::path::PathBuf, Value, Request, Vec<u8>) {
        let path = std::env::temp_dir().join(format!(
            "websa-demod-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let mut raw = Vec::new();
        for n in 0..samples {
            let phase = std::f64::consts::TAU * 731.0 * n as f64 / rate;
            let (i, q) = match mode {
                "am" => (0.5 + 0.2 * phase.cos(), 0.0),
                "nonfinite" => (f64::NAN, 0.0),
                "nfm" => {
                    let p = 500.0 / 731.0 * phase.sin();
                    (0.5 * p.cos(), 0.5 * p.sin())
                }
                _ => (
                    0.2 * phase.cos(),
                    0.2 * phase.sin() * if mode == "lsb" { -1.0 } else { 1.0 },
                ),
            };
            let carrier = std::f64::consts::TAU * offset * n as f64 / rate;
            let (s, c) = carrier.sin_cos();
            raw.extend_from_slice(&((i * c - q * s) as f32).to_le_bytes());
            raw.extend_from_slice(&((i * s + q * c) as f32).to_le_bytes());
        }
        let mut writer =
            v2::Writer::new(std::fs::File::create(path.join("iq-000000.wbiq")).unwrap()).unwrap();
        let mut index = std::fs::File::create(path.join("iq-000000.jsonl")).unwrap();
        let origin = u64::MAX - 10_000_000;
        for (n, bytes) in raw.chunks(block_samples * 8).enumerate() {
            let at = n * block_samples;
            let meta = v2::Meta {
                service_id: "old-boot".into(),
                segment_id: "old-stream".into(),
                sequence: n.to_string(),
                frequency_hz: 100e6,
                baseband: json!({"passband_hz":12000}),
                clock: IqBlockMeta {
                    receiver_uid: "retired-uid".into(),
                    source_id: "file".into(),
                    track_id: "iq".into(),
                    epoch: "7".into(),
                    channel_id: "9".into(),
                    config_revision: "9007199254740993".into(),
                    first_input_index: (origin + at as u64 * 3).to_string(),
                    first_output_index: (origin + at as u64).to_string(),
                    fraction_num: "1".into(),
                    fraction_den: "3".into(),
                    samples: (bytes.len() / 8) as u32,
                    sample_format: "cf32_le".into(),
                    amplitude_scale: 1.0,
                    input_rate_hz: rate * 3.0,
                    actual_rate_hz: rate,
                    delay_seconds: Some(0.001),
                },
            };
            let row = writer.append(meta, bytes).unwrap();
            serde_json::to_writer(&mut index, &row).unwrap();
            index.write_all(b"\n").unwrap();
        }
        writer.finish().unwrap();
        let task = json!({"task_id":"task","service_id":"old-boot","receiver_uid":"retired-uid","state":"failed","fault":{"code":"SERVICE_INTERRUPTED"},"tracks":[{"kind":"iq","track_id":"iq","volumes":[{"file":"iq-000000.wbiq"}],"segments":[{"segment_id":"old-stream","samples":samples.to_string(),"first_output_index":origin.to_string(),"stream":{"epoch":"7","channel_id":"9","source_id":"file"},"frequency_hz":100e6,"baseband":{"passband_hz":12000},"input_rate_hz":rate*3.0,"actual_rate_hz":rate}]}]});
        let request = Request {
            selection: Selection {
                segment: "old-stream".into(),
                start: 137,
                count: samples as u64 - 137,
            },
            mode: mode.into(),
            bandwidth: 2400.0,
            offset,
        };
        (path, task, request, raw)
    }
    #[test]
    fn offset_tuning_selects_signed_fractional_carriers_without_block_phase_resets() {
        for mode in MODES {
            for offset in [-2000.25, 2000.25, 0.001] {
                let rate = 48_840.130763116205;
                let (a, task_a, request_a, raw) = tuned_fixture(mode, rate, 30000, offset, 1237);
                let (b, task_b, request_b, _) = tuned_fixture(mode, rate, 30000, offset, 8192);
                let result_a = render(&a, &task_a, &request_a, &AtomicBool::new(false)).unwrap();
                let result_b = render(&b, &task_b, &request_b, &AtomicBool::new(false)).unwrap();
                let pcm = |r: &Render| {
                    r.wav[56..]
                        .chunks_exact(4)
                        .map(|s| f32::from_le_bytes(s.try_into().unwrap()))
                        .collect::<Vec<_>>()
                };
                let pa = pcm(&result_a);
                let pb = pcm(&result_b);
                assert_eq!(pa.len(), pb.len());
                assert!(
                    pa.iter().zip(&pb).all(|(a, b)| (a - b).abs() < 1e-6),
                    "{mode} {offset}: block partition changed PCM"
                );
                let tail = &pa[4800..];
                let projection = |hz: f64| {
                    let (mut re, mut im) = (0.0, 0.0);
                    for (n, &s) in tail.iter().enumerate() {
                        let phase = std::f64::consts::TAU * hz * n as f64 / RATE as f64;
                        re += f64::from(s) * phase.cos();
                        im += f64::from(s) * phase.sin();
                    }
                    2.0 * re.hypot(im) / tail.len() as f64
                };
                assert!(projection(731.0) > 0.05, "{mode} {offset}");
                assert!(projection(731.0) > projection(1100.0) * 30.0);
                let manifest: Value = serde_json::from_slice(&result_a.manifest).unwrap();
                assert_eq!(manifest["processing"]["tuning"]["offset_hz"], offset);
                assert_eq!(
                    manifest["processing"]["tuning"]["frequency_hz"],
                    100e6 + offset
                );
                assert_eq!(
                    manifest["source"]["cf32_sha256"],
                    format!("{:x}", Sha256::digest(&raw[137 * 8..]))
                );
                let source: Value = serde_json::from_slice(&result_a.source).unwrap();
                assert_eq!(source["captures"][0]["core:frequency"], 100e6);
                assert_eq!(
                    source["annotations"][0]["websa:clock"]["first_input_index"],
                    (u64::MAX - 10_000_000).to_string()
                );
                std::fs::remove_dir_all(a).unwrap();
                std::fs::remove_dir_all(b).unwrap();
            }
        }
        // The file path must not inherit the live mixer's near-zero tolerance.
        let mut nco = Nco::new();
        nco.set_frequency(0.001, 48000.0);
        let mut iq = vec![1.0, 0.0, 1.0, 0.0];
        nco.mix_exact(&mut iq);
        assert!(iq[3] < -1e-8);
    }
    #[test]
    fn shifted_filter_checks_sideband_edges_and_nyquist_without_wrapping() {
        let (path, mut task, mut request, _) = fixture("am", 12000.0, 2000);
        let cancel = AtomicBool::new(false);
        for (mode, valid, invalid_offset) in [
            ("am", 4800.0, 4800.001),
            ("nfm", -4800.0, -4800.001),
            ("usb", 3600.0, 3600.001),
            ("lsb", -3600.0, -3600.001),
        ] {
            request.mode = mode.into();
            request.offset = valid;
            assert!(render(&path, &task, &request, &cancel).is_ok(), "{mode}");
            request.offset = invalid_offset;
            assert!(render(&path, &task, &request, &cancel).is_err(), "{mode}");
        }
        // A forged wide passband must not admit aliased frequencies outside IQ Nyquist.
        task["tracks"][0]["segments"][0]["baseband"]["passband_hz"] = json!(100000);
        request.mode = "am".into();
        request.offset = 12000.0;
        assert!(render(&path, &task, &request, &cancel).is_err());
        std::fs::remove_dir_all(path).unwrap();
        for offset in ["NaN", "inf", "-inf", "", "bad"] {
            let params = HashMap::from([
                ("segment".into(), "s".into()),
                ("start".into(), "0".into()),
                ("count".into(), "100".into()),
                ("mode".into(), "am".into()),
                ("if_bw".into(), "1500".into()),
                ("offset_hz".into(), offset.into()),
            ]);
            assert!(Request::parse(&params).is_err());
        }
    }
    #[test]
    fn analogue_file_modes_recover_tones_at_real_rates_and_keep_original_clock_strings() {
        for (mode, rate) in [
            ("am", 48_840.130763116205),
            ("usb", 24_000.0),
            ("lsb", 24_000.0),
            ("nfm", 48_000.0),
        ] {
            let (path, task, request, raw) = fixture(mode, rate, rate as usize);
            let result = render(&path, &task, &request, &AtomicBool::new(false)).unwrap();
            let meta: Value = serde_json::from_slice(&result.source).unwrap();
            assert_eq!(meta["annotations"][0]["websa:skip_samples"], "137");
            assert_eq!(meta["annotations"][0]["websa:clock"]["fraction_den"], "3");
            assert_eq!(
                meta["annotations"][0]["websa:clock"]["first_input_index"],
                (u64::MAX - 10_000_000).to_string()
            );
            let manifest: Value = serde_json::from_slice(&result.manifest).unwrap();
            assert_eq!(
                manifest["source"]["cf32_sha256"],
                format!("{:x}", Sha256::digest(&raw[137 * 8..]))
            );
            assert_eq!(manifest["task"]["receiver_uid"], "retired-uid");
            let pcm: Vec<f64> = result.wav[56..]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()) as f64)
                .collect();
            assert!(
                (pcm.len() as f64 / RATE as f64 - request.selection.count as f64 / rate).abs()
                    < 2.0 / RATE as f64
            );
            let tail = &pcm[4800..];
            let projection = |hz: f64| -> f64 {
                let (mut re, mut im) = (0.0, 0.0);
                for (i, s) in tail.iter().enumerate() {
                    let phase = std::f64::consts::TAU * hz * i as f64 / RATE as f64;
                    re += s * phase.cos();
                    im += s * phase.sin();
                }
                2.0 * re.hypot(im) / tail.len() as f64
            };
            assert!(projection(731.0) > 0.05, "{mode}: {}", projection(731.0));
            assert!(projection(731.0) > projection(1100.0) * 30.0);
            let mut tar = Vec::new();
            result.emit(&mut tar, &AtomicBool::new(false)).unwrap();
            assert_eq!(tar.len() as u64, result.length());
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[test]
    fn file_render_rejects_lost_bandwidth_nonfinite_corrupt_and_cancelled_inputs() {
        let (path, mut task, mut request, _) = fixture("am", 12000.0, 2000);
        request.bandwidth = 6000.0;
        assert!(render(&path, &task, &request, &AtomicBool::new(false)).is_err());
        request.bandwidth = 2400.0;
        task["tracks"][0]["segments"][0]["baseband"]["passband_hz"] = json!(1000);
        assert!(render(&path, &task, &request, &AtomicBool::new(false)).is_err());
        task["tracks"][0]["segments"][0]["baseband"]["passband_hz"] = json!(12000);
        assert_eq!(
            render(&path, &task, &request, &AtomicBool::new(true))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::ConnectionAborted
        );
        use std::io::{Seek, SeekFrom};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path.join("iq-000000.wbiq"))
            .unwrap();
        file.seek(SeekFrom::Start(30)).unwrap();
        file.write_all(b"X").unwrap();
        assert!(render(&path, &task, &request, &AtomicBool::new(false)).is_err());
        std::fs::remove_dir_all(path).unwrap();
        let (path, task, mut request, _) = fixture("nonfinite", 12000.0, 2000);
        request.mode = "am".into();
        assert!(render(&path, &task, &request, &AtomicBool::new(false))
            .err()
            .unwrap()
            .to_string()
            .contains("finite"));
        std::fs::remove_dir_all(path).unwrap();
        for extra in [
            ("mode", "ft8"),
            ("if_bw", "NaN"),
            ("count", "1048577"),
            ("format", "wav"),
        ] {
            let mut params = HashMap::from([
                ("segment".into(), "s".into()),
                ("start".into(), "0".into()),
                ("count".into(), "100".into()),
                ("mode".into(), "am".into()),
                ("if_bw".into(), "1500".into()),
            ]);
            params.insert(extra.0.into(), extra.1.into());
            assert!(Request::parse(&params).is_err());
        }
        for mode in ["usb", "lsb"] {
            let mut params = HashMap::from([
                ("segment".into(), "s".into()),
                ("start".into(), "0".into()),
                ("count".into(), "100".into()),
                ("mode".into(), mode.into()),
                ("if_bw".into(), "100".into()),
            ]);
            assert!(Request::parse(&params).is_err());
            params.insert("if_bw".into(), "101".into());
            assert!(Request::parse(&params).is_ok());
        }
    }
}
pub(super) async fn export(
    State(app): State<Arc<App>>,
    Path(task): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let request = match Request::parse(&params) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":e.to_string()})),
            )
                .into_response()
        }
    };
    super::recording_export::download(
        app,
        task,
        "iq-demod.tar",
        move |path, task, cancel| {
            let result = render(path, task, &request, cancel)?;
            Ok((result.length(), result))
        },
        |output, _, _, result, cancel| result.emit(output, cancel),
    )
    .await
}
