//! Offline interleaved f32le IQ decoding through the same receiver used by the service.
use serde::Serialize;
use serde_json::Value;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use websa_channelizer::receiver::{AudioBlock, InputClock, Receiver, ReceiverConfig, ReceiverOutput};

const BLOCK_COMPLEX: usize = 16_384;
const MAX_EVENTS: usize = 100_000;
const MAX_EVENT_JSON_BYTES: usize = 64 * 1024 * 1024;
const MAX_FORMATS: usize = 256;
const HELP: &str = "Usage: sdr_decode --input IQ.f32 --mode MODE --input-rate HZ --bandwidth HZ\n\
    [--pitch HZ] [--audio-rate HZ] [--output-pcm PCM.f32] [--report-json REPORT.json]\n\
    [--first-sample-index N] [--delay-input-samples N]\n\n\
Modes: am dsb usb lsb cw nfm wfm pm ft8 sstv drm drmplus\n\
Input: little-endian interleaved f32 I,Q pairs at the stated input rate.\n\
Output: little-endian interleaved f32 PCM, with actual sample rate and channel count\n\
recorded in formats. A PCM file requires a single format; a format change is an error.\n\
The JSON report goes to stdout unless --report-json is supplied. Events are retained\n\
up to 100000 events / 64 MiB serialized JSON; exceeding either limit is an error.\n\
FT8 jobs run synchronously, including the final buffered window at EOF. Input windows\n\
must include any desired caller-supplied leading/trailing padding; none is added.\n\
Listener defaults: 48000 Hz, pitch 700 Hz, AGC enabled, squelch open, NR disabled.\n";

struct Args {
    input: PathBuf,
    output_pcm: Option<PathBuf>,
    report_json: Option<PathBuf>,
    config: ReceiverConfig,
    clock: InputClock,
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
    let mut config = ReceiverConfig { squelch_dbm: -200.0, ..ReceiverConfig::default() };
    let mut clock = InputClock { first_sample_index: 0, input_rate: 0.0, delay_input_samples: 0.0 };
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            print!("{HELP}");
            return Ok(None);
        }
        if !matches!(flag.as_str(), "--input" | "--output-pcm" | "--report-json" | "--mode"
            | "--input-rate" | "--bandwidth" | "--pitch" | "--audio-rate"
            | "--first-sample-index" | "--delay-input-samples") {
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
            _ => unreachable!(),
        }
    }
    config.mode = mode.ok_or("--mode is required")?;
    config.input_rate = input_rate.ok_or("--input-rate is required")?;
    config.if_bw = bandwidth.ok_or("--bandwidth is required")?;
    clock.input_rate = config.input_rate;
    if !clock.delay_input_samples.is_finite() || clock.delay_input_samples < 0.0 {
        return Err("--delay-input-samples must be finite and nonnegative".into());
    }
    if output_pcm.as_ref().is_some_and(|p| p.extension().and_then(|s| s.to_str()) != Some("f32")) {
        return Err("--output-pcm must have .f32 extension (interleaved f32le PCM)".into());
    }
    Ok(Some(Args {
        input: input.ok_or("--input is required")?, output_pcm, report_json, config, clock,
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
        if self.pcm_spans.last().is_none_or(|span| span.format_index != index) {
            if self.pcm_spans.len() >= MAX_EVENTS {
                return Err("PCM span report limit exceeded (100000 spans); split the input capture".into());
            }
            self.pcm_spans.push(PcmSpan {
                format_index: index,
                first_frame_index: audio.first_frame_index,
                clock: audio.clock,
                frames: 0,
                samples: 0,
            });
        }
        let span = self.pcm_spans.last_mut().unwrap();
        span.frames += (audio.samples.len() / audio.channels as usize) as u64;
        span.samples += audio.samples.len() as u64;
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

fn resolved_path(path: &Path) -> io::Result<PathBuf> {
    if path.exists() { return fs::canonicalize(path); }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected a file path"))?;
    Ok(fs::canonicalize(parent)?.join(name))
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let input_path = resolved_path(&args.input)?;
    let pcm_path = args.output_pcm.as_deref().map(resolved_path).transpose()?;
    let report_path = args.report_json.as_deref().map(resolved_path).transpose()?;
    if pcm_path.as_ref() == Some(&input_path) || report_path.as_ref() == Some(&input_path)
        || (pcm_path.is_some() && pcm_path == report_path) {
        return Err("input, PCM output, and JSON report must use distinct files".into());
    }
    let mut receiver = Receiver::new(args.config.clone())?;
    let input_file = File::open(&args.input)?;
    if input_file.metadata()?.len() % 8 != 0 {
        return Err("IQ file must contain complete little-endian f32 I,Q pairs (8 bytes each)".into());
    }
    let mut input = BufReader::new(input_file);
    let pcm = args.output_pcm.as_ref().map(File::create).transpose()?.map(BufWriter::new);
    let mut sink = Sink { pcm, formats: Vec::new(), pcm_spans: Vec::new(), events: Vec::new(), event_bytes: 0, pcm_bytes: Vec::new() };
    let mut bytes = vec![0_u8; BLOCK_COMPLEX * 8];
    let mut iq = Vec::with_capacity(BLOCK_COMPLEX * 2);
    let mut complex_samples = 0_u64;
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
            first_sample_index: args.clock.first_sample_index.checked_add(complex_samples).ok_or("source sample index overflow")?,
            ..args.clock
        };
        let output = receiver.process(&iq, clock, None)?;
        sink.output(&mut receiver, output)?;
        complex_samples = complex_samples.checked_add((iq.len() / 2) as u64).ok_or("input sample count overflow")?;
    }
    if let Some(job) = receiver.flush_ft8() { sink.events(receiver.accept_ft8(job.decode()))?; }
    if let Some(pcm) = sink.pcm.as_mut() { pcm.flush()?; }
    let report = Report {
        schema_version: 1,
        config: receiver.config(),
        input: InputReport { path: &args.input, encoding: "f32le", interleaved: true, complex_samples, source_clock: args.clock },
        output_pcm: args.output_pcm.as_deref().map(|path| PcmReport { path, encoding: "f32le", interleaved: true }),
        formats: sink.formats,
        pcm_spans: sink.pcm_spans,
        events: sink.events,
        event_limits: EventLimits { max_events: MAX_EVENTS, max_json_bytes: MAX_EVENT_JSON_BYTES, max_pcm_spans: MAX_EVENTS, max_formats: MAX_FORMATS },
        ft8: Ft8Report { padding: "caller-supplied; no automatic padding", eof_flush: args.config.mode == "ft8" },
    };
    let mut report_writer: Box<dyn Write> = match &args.report_json {
        Some(path) => Box::new(BufWriter::new(File::create(path)?)),
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
