//! Offline interleaved f32le IQ decoding through the same receiver used by the service.
use serde::Serialize;
use serde_json::Value;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use websa_channelizer::receiver::{AudioBlock, InputClock, Receiver, ReceiverConfig, ReceiverOutput, SamplePosition};

const BLOCK_COMPLEX: usize = 16_384;
const MAX_EVENTS: usize = 100_000;
const MAX_EVENT_JSON_BYTES: usize = 64 * 1024 * 1024;
const MAX_FORMATS: usize = 256;
const HELP: &str = "Usage: sdr_decode --input IQ.f32 --mode MODE --input-rate HZ --bandwidth HZ\n\
    [--pitch HZ] [--audio-rate HZ] [--output-pcm PCM.f32] [--report-json REPORT.json]\n\
    [--first-sample-index N] [--delay-input-samples N] [--center-hz HZ] [--realtime]\n\
    [--generated-utc-slot]\n\n\
Modes: am dsb usb lsb cw nfm wfm pm ft8 sstv drm drmplus radiofax;\n\
external: pocsag512 pocsag1200 pocsag2400 flex flex_next eas afsk1200 afsk2400\n\
afsk2400_2 afsk2400_3 hapn4800 fsk9600 dtmf zvei1 zvei2 zvei3 dzvei pzvei\n\
eea eia ccir morse_cw ufsk1200 clipfsk ism ais aprs ft4 wspr meteor adsb gps rtty rtty-reverse.\n\
Input: little-endian interleaved f32 I,Q pairs at the stated input rate.\n\
Output: little-endian interleaved f32 PCM, with actual sample rate and channel count\n\
recorded in formats. A PCM file requires a single format; a format change is an error.\n\
The JSON report goes to stdout unless --report-json is supplied. Events are retained\n\
up to 100000 events / 64 MiB serialized JSON; exceeding either limit is an error.\n\
FT8 jobs run synchronously, including the final buffered window at EOF. Input windows\n\
must include any desired caller-supplied leading/trailing padding; none is added.\n\
External decoders require configured executable dependencies. stdin providers drain at EOF;\n\
OnAir reports its bounded drain without claiming upstream completion. --center-hz sets RF metadata.\n\
--realtime paces input by its sample clock for live or wall-clock decoder providers.\n\
OnAir FT4/WSPR recordings are rejected. For a generated complete-slot fixture only,\n\
--generated-utc-slot --realtime schedules sample zero at the next UTC slot, feeding\n\
explicit silence beforehand. This relabels synthetic signal time, not recording UTC.\n\
Listener defaults: 48000 Hz, pitch 700 Hz, AGC enabled, squelch open, NR disabled.\n";

struct Args {
    input: PathBuf,
    output_pcm: Option<PathBuf>,
    report_json: Option<PathBuf>,
    config: ReceiverConfig,
    clock: InputClock,
    realtime: bool,
    generated_utc_slot: bool,
}

fn value<T: std::str::FromStr>(flag: &str, text: String) -> Result<T, String> {
    text.parse().map_err(|_| format!("invalid value for {flag}: {text}"))
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut input = None;
    let mut output_pcm = None;
    let mut report_json = None;
    let mut mode = None;
    let mut input_rate = None;
    let mut bandwidth = None;
    let mut realtime = false;
    let mut generated_utc_slot = false;
    let mut config = ReceiverConfig { squelch_dbm: -200.0, ..ReceiverConfig::default() };
    let mut clock = InputClock { first_sample_index: 0, input_rate: 0.0, delay_input_samples: 0.0 };
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            print!("{HELP}");
            return Ok(None);
        }
        if flag == "--realtime" { realtime = true; continue; }
        if flag == "--generated-utc-slot" { generated_utc_slot = true; continue; }
        if !matches!(flag.as_str(), "--input" | "--output-pcm" | "--report-json" | "--mode"
            | "--input-rate" | "--bandwidth" | "--pitch" | "--audio-rate"
            | "--first-sample-index" | "--delay-input-samples" | "--center-hz") {
            return Err(format!("unknown argument {flag}; use --help"));
        }
        let text = args.next().ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--input" => input = Some(PathBuf::from(text)),
            "--output-pcm" => output_pcm = Some(PathBuf::from(text)),
            "--report-json" => report_json = Some(PathBuf::from(text)),
            "--mode" => mode = Some(text),
            "--input-rate" => input_rate = Some(value::<f64>(&flag, text)?),
            "--bandwidth" => bandwidth = Some(value::<f64>(&flag, text)?),
            "--pitch" => config.pitch = value(&flag, text)?,
            "--audio-rate" => config.audio_rate = value(&flag, text)?,
            "--first-sample-index" => clock.first_sample_index = value(&flag, text)?,
            "--delay-input-samples" => clock.delay_input_samples = value(&flag, text)?,
            "--center-hz" => config.center_hz = value(&flag, text)?,
            _ => unreachable!(),
        }
    }
    config.mode = mode.ok_or("--mode is required")?;
    if matches!(config.mode.as_str(), "ft4" | "wspr") && !generated_utc_slot {
        return Err("OnAir FT4/WSPR require UTC-aligned live IQ; arbitrary recording replay is unsupported, and --realtime does not restore source UTC alignment".into());
    }
    config.input_rate = input_rate.ok_or("--input-rate is required")?;
    if generated_utc_slot && (!realtime || !matches!(config.mode.as_str(), "ft4" | "wspr")) {
        return Err("--generated-utc-slot requires --realtime and mode ft4 or wspr".into());
    }
    config.if_bw = bandwidth.ok_or("--bandwidth is required")?;
    clock.input_rate = config.input_rate;
    if !clock.delay_input_samples.is_finite() || clock.delay_input_samples < 0.0 {
        return Err("--delay-input-samples must be finite and nonnegative".into());
    }
    if output_pcm.as_ref().is_some_and(|p| p.extension().and_then(|s| s.to_str()) != Some("f32")) {
        return Err("--output-pcm must have .f32 extension (interleaved f32le PCM)".into());
    }
    Ok(Some(Args {
        input: input.ok_or("--input is required")?, output_pcm, report_json, config, clock, realtime, generated_utc_slot,
    }))
}

#[derive(Serialize)]
struct FormatCount {
    sample_rate: u32,
    channels: u16,
    frames: u64,
    samples: u64,
}

#[derive(Serialize)]
struct PcmSpan {
    format_index: usize,
    first_frame_index: u64,
    clock: InputClock,
    frames: u64,
    samples: u64,
    source_position: SamplePosition,
    timing_kind: String,
    codec_delay_known: bool,
}

#[derive(Serialize)]
struct InputReport<'a> {
    path: &'a Path,
    encoding: &'static str,
    interleaved: bool,
    complex_samples: u64,
    source_clock: InputClock,
}

#[derive(Serialize)]
struct PcmReport<'a> {
    path: &'a Path,
    encoding: &'static str,
    interleaved: bool,
}

#[derive(Serialize)]
struct EventLimits {
    max_events: usize,
    max_json_bytes: usize,
    max_pcm_spans: usize,
    max_formats: usize,
}

#[derive(Serialize)]
struct Ft8Report {
    padding: &'static str,
    eof_flush: bool,
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u32,
    config: &'a ReceiverConfig,
    input: InputReport<'a>,
    output_pcm: Option<PcmReport<'a>>,
    formats: Vec<FormatCount>,
    pcm_spans: Vec<PcmSpan>,
    events: Vec<Value>,
    event_limits: EventLimits,
    ft8: Ft8Report,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_slot: Option<GeneratedSlot>,
}

#[derive(Serialize)]
struct GeneratedSlot {
    timing: &'static str,
    slot_utc_seconds: f64,
    period_seconds: f64,
    preroll_complex_samples: u64,
    maximum_pacing_lateness_seconds: f64,
}

fn pace(started: std::time::Instant, samples: u64, rate: f64, strict: bool,
    maximum_lateness: &mut f64) -> Result<(), Box<dyn std::error::Error>> {
    let target = std::time::Duration::from_secs_f64(samples as f64 / rate);
    let elapsed = started.elapsed();
    let lateness = elapsed.saturating_sub(target).as_secs_f64();
    *maximum_lateness = maximum_lateness.max(lateness);
    if strict && lateness > 0.1 { return Err("generated UTC replay fell over 100 ms behind its sample clock".into()); }
    if let Some(wait) = target.checked_sub(elapsed) { std::thread::sleep(wait); }
    Ok(())
}

#[derive(Default)]
struct ByteCount(usize);
impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

struct Sink {
    pcm: Option<BufWriter<File>>,
    formats: Vec<FormatCount>,
    pcm_spans: Vec<PcmSpan>,
    events: Vec<Value>,
    event_bytes: usize,
    pcm_bytes: Vec<u8>,
    next_source: Option<SamplePosition>,
}
impl Sink {
    fn events(&mut self, events: Vec<Value>) -> Result<(), Box<dyn std::error::Error>> {
        for event in events {
            let mut count = ByteCount::default();
            serde_json::to_writer(&mut count, &event)?;
            if self.events.len() >= MAX_EVENTS || count.0 > MAX_EVENT_JSON_BYTES - self.event_bytes {
                return Err("decoded-event report limit exceeded (100000 events / 64 MiB); split the input capture".into());
            }
            self.event_bytes += count.0;
            self.events.push(event);
        }
        Ok(())
    }

    fn audio(&mut self, audio: AudioBlock) -> Result<(), Box<dyn std::error::Error>> {
        if audio.samples.is_empty() { return Ok(()); }
        if audio.rate == 0 || audio.channels == 0 || audio.samples.len() % audio.channels as usize != 0 {
            return Err("receiver emitted invalid PCM geometry".into());
        }
        if audio.samples.iter().any(|sample| !sample.is_finite())
            || !audio.clock.input_rate.is_finite() || audio.clock.input_rate <= 0.0
            || !audio.clock.delay_input_samples.is_finite() || audio.clock.delay_input_samples < 0.0 {
            return Err("receiver emitted invalid PCM samples or clock".into());
        }
        let source = audio.source_position.ok_or("receiver PCM lacks IQ provenance")?;
        if source.fraction_den == 0 || source.fraction_num >= source.fraction_den {
            return Err("receiver emitted invalid PCM source fraction".into());
        }
        let frames = (audio.samples.len()/audio.channels as usize) as u64;
        let previous = self.pcm_spans.last();
        let same_format = previous.is_some_and(|span| {
            let format = &self.formats[span.format_index];
            format.sample_rate == audio.rate && format.channels == audio.channels
        });
        let contiguous_source = same_format && self.next_source.is_some_and(|next| position_cmp(source,next).is_eq());
        if same_format {
            let previous = previous.unwrap();
            if audio.first_frame_index != previous.first_frame_index.checked_add(previous.frames).ok_or("PCM frame index overflow")?
                || audio.clock.input_rate != previous.clock.input_rate {
                return Err("receiver emitted discontinuous PCM frame index or source rate".into());
            }
            if self.next_source.is_some_and(|next| position_cmp(source,next).is_lt()) {
                return Err("receiver emitted overlapping or backward PCM provenance".into());
            }
        } else if audio.first_frame_index != 0 {
            return Err("receiver PCM format starts with a nonzero frame index".into());
        }
        let next_source = if audio.timing_kind == "sample" {
            source.checked_advance(frames,audio.clock.input_rate,audio.rate)?
        } else { source };
        let index = self.formats.iter().position(|format|
            format.sample_rate == audio.rate && format.channels == audio.channels);
        let index = match index {
            Some(index) => index,
            None => {
                if self.pcm.is_some() && !self.formats.is_empty() {
                    return Err("PCM format changed; a single .f32 output cannot describe multiple formats".into());
                }
                if self.formats.len() >= MAX_FORMATS { return Err("PCM format limit exceeded".into()); }
                self.formats.push(FormatCount { sample_rate: audio.rate, channels: audio.channels, frames: 0, samples: 0 });
                self.formats.len() - 1
            }
        };
        let format = &mut self.formats[index];
        format.frames += (audio.samples.len() / audio.channels as usize) as u64;
        format.samples += audio.samples.len() as u64;
        if !contiguous_source || audio.timing_kind != "sample"
            || self.pcm_spans.last().is_none_or(|span| span.format_index != index
                || span.clock.delay_input_samples != audio.clock.delay_input_samples
                || span.timing_kind != audio.timing_kind || span.codec_delay_known != audio.codec_delay_known) {
            if self.pcm_spans.len() >= MAX_EVENTS {
                return Err("PCM span report limit exceeded (100000 spans); split the input capture".into());
            }
            self.pcm_spans.push(PcmSpan {
                format_index: index,
                first_frame_index: audio.first_frame_index,
                clock: audio.clock,
                frames: 0,
                samples: 0,
                source_position: source,
                timing_kind: audio.timing_kind,
                codec_delay_known: audio.codec_delay_known,
            });
        }
        let span = self.pcm_spans.last_mut().unwrap();
        span.frames += (audio.samples.len() / audio.channels as usize) as u64;
        span.samples += audio.samples.len() as u64;
        self.next_source = Some(next_source);
        if let Some(pcm) = self.pcm.as_mut() {
            self.pcm_bytes.clear();
            self.pcm_bytes.reserve(audio.samples.len() * 4);
            for sample in audio.samples { self.pcm_bytes.extend_from_slice(&sample.to_le_bytes()); }
            pcm.write_all(&self.pcm_bytes)?;
        }
        Ok(())
    }

    fn output(&mut self, receiver: &mut Receiver, output: ReceiverOutput) -> Result<(), Box<dyn std::error::Error>> {
        self.events(output.events)?;
        for audio in output.audio { self.audio(audio)?; }
        if let Some(job) = output.ft8_job { self.events(receiver.accept_ft8(job.decode()))?; }
        Ok(())
    }
}

fn position_cmp(a: SamplePosition, b: SamplePosition) -> std::cmp::Ordering {
    a.index.cmp(&b.index).then_with(||
        (a.fraction_num as u128 * b.fraction_den as u128).cmp(&(b.fraction_num as u128 * a.fraction_den as u128)))
}

fn resolved_path(path: &Path) -> io::Result<PathBuf> {
    if path.exists() { return fs::canonicalize(path); }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected a file path"))?;
    Ok(fs::canonicalize(parent)?.join(name))
}

fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let input_file = File::open(&args.input)?;
    // /dev/stdin may resolve to a pipe:[inode], which has no canonical filesystem
    // path. Compare the opened descriptor identities below for every input kind.
    let input_path = input_file.metadata()?.is_file().then(|| resolved_path(&args.input)).transpose()?;
    let pcm_path = args.output_pcm.as_deref().map(resolved_path).transpose()?;
    let report_path = args.report_json.as_deref().map(resolved_path).transpose()?;
    if input_path.as_ref().is_some_and(|input| pcm_path.as_ref() == Some(input) || report_path.as_ref() == Some(input))
        || (pcm_path.is_some() && pcm_path == report_path) {
        return Err("input, PCM output, and JSON report must use distinct files".into());
    }
    if input_file.metadata()?.len() % 8 != 0 {
        return Err("IQ file must contain complete little-endian f32 I,Q pairs (8 bytes each)".into());
    }
    let period = if args.config.mode == "ft4" { 7.5 } else { 120.0 };
    if args.generated_utc_slot {
        let length = input_file.metadata()?.len();
        let slot_bytes = ((period * args.config.input_rate).round() as u64).checked_mul(8)
            .ok_or("generated slot byte count exceeds supported bounds")?;
        if !input_file.metadata()?.is_file() || length == 0 || slot_bytes == 0 || length % slot_bytes != 0
            || args.clock.first_sample_index != 0 || args.clock.delay_input_samples != 0.0 {
            return Err("generated UTC fixtures must be regular files of complete slots, with sample zero at slot origin and no source delay".into());
        }
    }
    // Open without truncation so aliases are checked against the actual files,
    // including hard links and paths changed after the canonical-path check.
    let open_output = |path: &PathBuf| OpenOptions::new().write(true).create(true).truncate(false).open(path);
    let pcm_file = args.output_pcm.as_ref().map(open_output).transpose()?;
    let report_file = args.report_json.as_ref().map(open_output).transpose()?;
    let input_identity = file_identity(&input_file)?;
    let pcm_identity = pcm_file.as_ref().map(file_identity).transpose()?;
    let report_identity = report_file.as_ref().map(file_identity).transpose()?;
    if pcm_identity == Some(input_identity) || report_identity == Some(input_identity)
        || (pcm_identity.is_some() && pcm_identity == report_identity) {
        return Err("input, PCM output, and JSON report must use distinct files".into());
    }
    let mut receiver = Receiver::new(args.config.clone())?;
    if let Some(file) = &pcm_file { file.set_len(0)?; }
    let mut input = BufReader::new(input_file);
    let pcm = pcm_file.map(BufWriter::new);
    let mut sink = Sink { pcm, formats: Vec::new(), pcm_spans: Vec::new(), events: Vec::new(), event_bytes: 0, pcm_bytes: Vec::new(), next_source: None };
    let mut bytes = vec![0_u8; BLOCK_COMPLEX * 8];
    let mut iq = Vec::with_capacity(BLOCK_COMPLEX * 2);
    let mut complex_samples = 0_u64;
    let started = std::time::Instant::now();
    let started_utc = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs_f64();
    let mut maximum_lateness = 0.0;
    let mut generated_slot = args.generated_utc_slot.then(|| {
        let slot = ((started_utc + 1.0) / period).ceil() * period;
        GeneratedSlot { timing:"synthetic sample zero rescheduled to host UTC; not original recording UTC",
            slot_utc_seconds:slot,period_seconds:period,
            preroll_complex_samples:((slot-started_utc)*args.config.input_rate).round() as u64,
            maximum_pacing_lateness_seconds:0.0 }
    });
    let preroll = generated_slot.as_ref().map_or(0,|slot|slot.preroll_complex_samples);
    let mut silence_samples=0;
    while silence_samples < preroll {
        let count=(preroll-silence_samples).min(BLOCK_COMPLEX as u64) as usize;
        iq.clear();iq.resize(count*2,0.0);
        let output=receiver.process(&iq,InputClock {first_sample_index:silence_samples,..args.clock},None)?;
        sink.output(&mut receiver,output)?;
        silence_samples+=count as u64;
        pace(started,silence_samples,args.config.input_rate,true,&mut maximum_lateness)?;
    }
    loop {
        let mut filled = 0;
        while filled < bytes.len() {
            match input.read(&mut bytes[filled..]) {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        if filled == 0 { break; }
        if filled % 8 != 0 { return Err("IQ file ended with an incomplete f32 I,Q pair".into()); }
        iq.clear();
        for sample in bytes[..filled].chunks_exact(4) {
            iq.push(f32::from_le_bytes(sample.try_into()?));
        }
        let clock = InputClock {
            first_sample_index: args.clock.first_sample_index.checked_add(preroll).and_then(|n|n.checked_add(complex_samples)).ok_or("source sample index overflow")?,
            ..args.clock
        };
        let output = receiver.process(&iq, clock, None)?;
        sink.output(&mut receiver, output)?;
        complex_samples = complex_samples.checked_add((iq.len() / 2) as u64).ok_or("input sample count overflow")?;
        if args.realtime {
            pace(started,preroll+complex_samples,args.config.input_rate,args.generated_utc_slot,&mut maximum_lateness)?;
            if args.generated_utc_slot {
                let utc=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs_f64();
                if (utc-started_utc-started.elapsed().as_secs_f64()).abs()>0.1 {
                    return Err("host UTC stepped during generated-slot replay".into());
                }
            }
        }
    }
    if let Some(job) = receiver.flush_ft8()? { sink.events(receiver.accept_ft8(job.decode()))?; }
    receiver.finish_radiofax()?;
    sink.events(receiver.take_radiofax_events()?)?;
    sink.events(receiver.finish_external()?)?;
    if let Some(pcm) = sink.pcm.as_mut() { pcm.flush()?; }
    if let Some(slot)=generated_slot.as_mut() {slot.maximum_pacing_lateness_seconds=maximum_lateness;}
    let report = Report {
        schema_version: 1,
        config: receiver.config(),
        input: InputReport { path: &args.input, encoding: "f32le", interleaved: true, complex_samples,
            source_clock:InputClock {first_sample_index:args.clock.first_sample_index+preroll,..args.clock} },
        output_pcm: args.output_pcm.as_deref().map(|path| PcmReport { path, encoding: "f32le", interleaved: true }),
        formats: sink.formats,
        pcm_spans: sink.pcm_spans,
        events: sink.events,
        event_limits: EventLimits { max_events: MAX_EVENTS, max_json_bytes: MAX_EVENT_JSON_BYTES, max_pcm_spans: MAX_EVENTS, max_formats: MAX_FORMATS },
        ft8: Ft8Report { padding: "caller-supplied; no automatic padding", eof_flush: args.config.mode == "ft8" },
        generated_slot,
    };
    let mut report_writer: Box<dyn Write> = match report_file {
        Some(file) => {
            file.set_len(0)?;
            Box::new(BufWriter::new(file))
        }
        None => Box::new(BufWriter::new(io::stdout().lock())),
    };
    serde_json::to_writer(&mut report_writer, &report)?;
    report_writer.write_all(b"\n")?;
    report_writer.flush()?;
    Ok(())
}

fn main() {
    let result = match parse_args() {
        Ok(Some(args)) => run(args),
        Ok(None) => return,
        Err(error) => Err(error.into()),
    };
    if let Err(error) = result {
        eprintln!("sdr_decode: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_input_decodes_without_a_canonical_filesystem_target() {
        use std::os::fd::{FromRawFd, AsRawFd};
        let mut fds=[0;2];
        assert_eq!(unsafe {libc::pipe(fds.as_mut_ptr())},0);
        let input=unsafe {File::from_raw_fd(fds[0])};
        let mut output=unsafe {File::from_raw_fd(fds[1])};
        let writer=std::thread::spawn(move || {
            for _ in 0..128 {output.write_all(&[0.25_f32.to_le_bytes(),0.0_f32.to_le_bytes()].concat()).unwrap();}
        });
        let report=env::temp_dir().join(format!("websa-pipe-decode-{}.json",std::process::id()));
        run(Args {input:PathBuf::from(format!("/proc/self/fd/{}",input.as_raw_fd())),
            output_pcm:None,report_json:Some(report.clone()),
            config:ReceiverConfig {mode:"am".into(),input_rate:48000.0,if_bw:6000.0,..ReceiverConfig::default()},
            clock:InputClock {first_sample_index:0,input_rate:48000.0,delay_input_samples:0.0},
            realtime:false,generated_utc_slot:false}).unwrap();
        writer.join().unwrap();
        let value:Value=serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
        fs::remove_file(report).unwrap();
        assert_eq!(value["input"]["complex_samples"],128);
    }

    #[cfg(unix)]
    #[test]
    fn decode_output_boundary_preserves_hardlinked_input() {
        struct Directory(PathBuf);
        impl Drop for Directory { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
        let dir = Directory(env::temp_dir().join(format!("websa-decode-alias-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())));
        fs::create_dir(&dir.0).unwrap();
        let original: Vec<u8> = (0..128).flat_map(|_| [0.25f32, 0.0].into_iter().flat_map(f32::to_le_bytes)).collect();
        let mut failures = Vec::new();
        for pcm in [true, false] {
            let input = dir.0.join(format!("source-{pcm}.iq"));
            let output = dir.0.join(if pcm { "linked.f32" } else { "linked.json" });
            fs::write(&input, &original).unwrap();
            fs::hard_link(&input, &output).unwrap();
            let result = run(Args {
                input: input.clone(), output_pcm: pcm.then_some(output.clone()),
                report_json: Some(if pcm {dir.0.join("report.json")} else {output}),
                config: ReceiverConfig { mode:"am".into(), input_rate:48_000.0,
                    if_bw:6_000.0, ..ReceiverConfig::default() },
                clock: InputClock { first_sample_index:0, input_rate:48_000.0, delay_input_samples:0.0 },
                realtime:false,generated_utc_slot:false,
            });
            if result.is_ok() || fs::read(&input).unwrap() != original {
                failures.push(if pcm { "PCM output truncated its IQ source" } else { "JSON report overwrote its IQ source" });
            }
        }
        let input = dir.0.join("distinct-source.iq");
        let pcm = dir.0.join("output.f32");
        let report = dir.0.join("output.json");
        fs::write(&input, &original).unwrap();
        let previous = b"existing output must survive alias rejection";
        fs::write(&pcm, previous).unwrap();
        fs::hard_link(&pcm, &report).unwrap();
        let args = || Args {
            input:input.clone(), output_pcm:Some(pcm.clone()), report_json:Some(report.clone()),
            config:ReceiverConfig { mode:"am".into(), input_rate:48_000.0,
                if_bw:6_000.0, ..ReceiverConfig::default() },
            clock:InputClock { first_sample_index:0, input_rate:48_000.0, delay_input_samples:0.0 },
            realtime:false,generated_utc_slot:false,
        };
        assert!(run(args()).is_err(), "PCM and report cannot share an inode");
        assert_eq!(fs::read(&pcm).unwrap(), previous);
        assert_eq!(fs::read(&report).unwrap(), previous);
        assert_eq!(fs::read(&input).unwrap(), original);
        // Distinct existing outputs still use the CLI's overwrite semantics.
        fs::remove_file(&report).unwrap();
        fs::write(&report, vec![b'X'; 16_384]).unwrap();
        run(args()).unwrap();
        let decoded:Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
        assert_eq!(decoded["input"]["complex_samples"], 128);
        assert_ne!(fs::read(&pcm).unwrap(), previous);
        assert_eq!(fs::read(&input).unwrap(), original);
        assert!(failures.is_empty(), "{}", failures.join("; "));
    }

    fn sink() -> Sink {
        Sink {pcm:None,formats:Vec::new(),pcm_spans:Vec::new(),events:Vec::new(),
            event_bytes:0,pcm_bytes:Vec::new(),next_source:None}
    }

    fn block(frame: u64, source: u64) -> AudioBlock {
        AudioBlock {samples:vec![0.0;128],rate:48_000,channels:1,
            clock:InputClock {first_sample_index:source,input_rate:48_000.0,delay_input_samples:0.0},
            first_frame_index:frame,source_position:Some(SamplePosition {index:source,fraction_num:0,fraction_den:1}),
            timing_kind:"sample".into(),codec_delay_known:true}
    }

    #[test]
    fn source_gaps_split_report_spans_and_overlap_is_rejected() {
        let mut sink = sink();
        sink.audio(block(0,100)).unwrap();
        sink.audio(block(128,228)).unwrap();
        assert_eq!(sink.pcm_spans.len(),1);
        assert_eq!(sink.pcm_spans[0].frames,256);
        sink.audio(block(256,700)).unwrap();
        assert_eq!(sink.pcm_spans.len(),2);
        assert_eq!(sink.pcm_spans[1].source_position.index,700);
        assert_eq!(sink.pcm_spans[1].first_frame_index,256);
        assert!(sink.audio(block(384,827)).is_err());
        assert!(sink.audio(block(385,828)).is_err());
        sink.audio(block(384,828)).unwrap();
    }

    #[test]
    fn nonfinite_pcm_is_rejected_before_report_or_output() {
        let mut sink = sink();
        let mut invalid = block(0,0);
        invalid.samples[1] = f32::NAN;
        assert!(sink.audio(invalid).is_err());
        assert!(sink.pcm_spans.is_empty());
        assert!(sink.formats.is_empty());
    }
}
