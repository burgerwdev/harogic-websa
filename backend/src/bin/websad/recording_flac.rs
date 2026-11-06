//! Offline export of published audio as explicitly quantized signed-24 PCM FLAC.
//! One 4096-sample encoder frame is retained. Original Float32 journals stay untouched.
use super::*;
use flacenc::{
    bitsink::ByteSink,
    component::{BitRepr, Stream, StreamInfo},
    error::{Verified, Verify},
    source::{Fill, FrameBuf},
};
use md5::{Digest, Md5};
use sha2::Sha512;

const BLOCK: usize = 4096;
const SCALE: f64 = 8_388_608.0;
const MAX_PLAN: usize = 2 * 1024 * 1024;

/// Fixed scale, nearest integer with ties away from zero, then saturate. No dither,
/// automatic gain, normalization or resampling. Count values outside the PCM range.
fn quantize(value: f32) -> (i32, bool) {
    let rounded = (f64::from(value) * SCALE).round();
    (
        rounded.clamp(-SCALE, SCALE - 1.0) as i32,
        !(-SCALE..=SCALE - 1.0).contains(&rounded),
    )
}

#[derive(Debug, PartialEq)]
pub(super) struct Segment {
    id: String,
    pub(super) header: Vec<u8>,
    bytes: u64,
    samples: u64,
    pub(super) clipped: u64,
    hash: [u8; 64],
}
pub(super) struct Encoder {
    id: String,
    config: Verified<flacenc::config::Encoder>,
    info: StreamInfo,
    buffer: FrameBuf,
    samples: Vec<i32>,
    frames: usize,
    bytes: u64,
    byte_limit: Option<u64>,
    clipped: u64,
    md5: Md5,
    hash: Sha512,
}
impl Encoder {
    pub(super) fn new(id: &str, audio: &Value) -> io::Result<Self> {
        let channels = audio["channels"]
            .as_u64()
            .ok_or_else(|| io_error("missing audio channels"))? as usize;
        let rate = audio["rate_hz"]
            .as_f64()
            .ok_or_else(|| io_error("missing audio rate"))? as usize;
        let mut config = flacenc::config::Encoder::default();
        config.multithread = false;
        // Fixed predictors and Rice coding bound per-frame CPU; no LPC search pool.
        config.subframe_coding.use_lpc = false;
        // Estimate-only selection can badly underestimate abrupt 24-bit signals
        // and emit enormous unary residuals. Compare actual Rice bit counts so
        // verbatim coding bounds both output size and recording-worker time.
        config.subframe_coding.fixed.order_sel = flacenc::config::OrderSel::BitCount;
        Ok(Self {
            id: id.to_owned(),
            config: config
                .into_verified()
                .map_err(|e| io_error(format!("FLAC config: {e:?}")))?,
            info: StreamInfo::new(rate, channels, 24).map_err(io_error)?,
            buffer: FrameBuf::with_size(channels, BLOCK).map_err(io_error)?,
            samples: Vec::with_capacity(BLOCK * channels),
            frames: 0,
            bytes: 0,
            byte_limit: None,
            clipped: 0,
            md5: Md5::new(),
            hash: Sha512::new(),
        })
    }
    pub(super) fn push(&mut self, pcm: &[u8], output: &mut (impl Write + ?Sized)) -> io::Result<()> {
        for bytes in pcm.chunks_exact(4) {
            let value = f32::from_le_bytes(bytes.try_into().unwrap());
            if !value.is_finite() {
                return Err(io_error("non-finite FLAC input"));
            }
            let (sample, clipped) = quantize(value);
            self.clipped += u64::from(clipped);
            self.md5.update(&sample.to_le_bytes()[..3]);
            self.samples.push(sample);
            if self.samples.len() == BLOCK * self.info.channels() {
                self.flush(output)?;
            }
        }
        Ok(())
    }
    fn flush(&mut self, output: &mut (impl Write + ?Sized)) -> io::Result<()> {
        if self.samples.is_empty() {
            return Ok(());
        }
        self.buffer
            .fill_interleaved(&self.samples)
            .map_err(io_error)?;
        let mut frame =
            flacenc::encode_fixed_size_frame(&self.config, &self.buffer, self.frames, &self.info)
                .map_err(io_error)?;
        // The encoder's partition cost tables use narrow integer arithmetic.
        // Check the actual representation before allocating/writing its bitstream.
        if frame.count_bits() > self.samples.len() * 24 + 1024 {
            let mut fallback = flacenc::config::Encoder::default();
            fallback.multithread = false;
            fallback.subframe_coding.use_fixed = false;
            fallback.subframe_coding.use_lpc = false;
            let fallback = fallback.into_verified().map_err(|e| io_error(format!("FLAC config: {e:?}")))?;
            frame = flacenc::encode_fixed_size_frame(&fallback, &self.buffer, self.frames, &self.info).map_err(io_error)?;
        }
        let mut sink = ByteSink::new();
        frame.write(&mut sink).map_err(io_error)?;
        let next_bytes = self.bytes + sink.as_slice().len() as u64;
        // A changed source may compress to more bytes. Never let it fill the HTTP
        // Content-Length before verification can reject it and withhold the tar tail.
        if self.byte_limit.is_some_and(|limit| next_bytes > limit) {
            return Err(io_error("FLAC encoded size exceeds verified member length"));
        }
        self.info.update_frame_info(&frame);
        self.hash.update(sink.as_slice());
        self.bytes = next_bytes;
        output.write_all(sink.as_slice())?;
        self.frames += 1;
        self.samples.clear();
        Ok(())
    }
    pub(super) fn finish(mut self, output: &mut (impl Write + ?Sized)) -> io::Result<Segment> {
        self.flush(output)?;
        // RFC 9639 excludes the final short frame from the minimum block size.
        // Fixed-block streams declare the chosen block size even for a one-sample file.
        self.info.set_block_sizes(BLOCK, BLOCK).map_err(io_error)?;
        self.info.set_md5_digest(&self.md5.finalize().into());
        let samples = self.info.total_samples() as u64;
        let mut sink = ByteSink::new();
        Stream::with_stream_info(self.info)
            .write(&mut sink)
            .map_err(io_error)?;
        let header = sink.into_inner();
        Ok(Segment {
            id: self.id,
            bytes: self.bytes + header.len() as u64,
            header,
            samples,
            clipped: self.clipped,
            hash: self.hash.finalize().into(),
        })
    }
}
fn pad(bytes: u64) -> usize {
    ((512 - bytes % 512) % 512) as usize
}

fn finish_segment(
    encoder: Encoder,
    output: &mut (impl Write + ?Sized),
    plans: &mut Vec<Segment>,
    expected: Option<&[Segment]>,
) -> io::Result<()> {
    let segment = encoder.finish(output)?;
    if let Some(expected) = expected {
        if expected.get(plans.len()) != Some(&segment) {
            return Err(io_error(
                "FLAC source or encoding changed after verification",
            ));
        }
        output.write_all(&[0; 512][..pad(segment.bytes)])?;
    }
    plans.push(segment);
    if plans.len() > MAX_SEGMENTS {
        return Err(io_error("FLAC segment count exceeds bound"));
    }
    Ok(())
}

/// With no plan, encode to a sink to compute exact lengths, STREAMINFO/MD5 and
/// quantization statistics. Emission re-encodes the same checked source in order.
fn encode(
    path: &FsPath,
    task: &Value,
    cancel: &AtomicBool,
    expected: Option<&[Segment]>,
    output: &mut (impl Write + ?Sized),
) -> io::Result<(Vec<Segment>, u64, [u8; 64])> {
    let mut plans = Vec::new();
    let mut encoder: Option<Encoder> = None;
    let (_, lines, hash) = super::audio::visit(path, task, cancel, |_, row, pcm, id, _| {
        if pcm.is_empty() {
            return Ok(());
        }
        if encoder.as_ref().is_none_or(|e| e.id != id) {
            if let Some(previous) = encoder.take() {
                finish_segment(previous, output, &mut plans, expected)?;
            }
            let mut next = Encoder::new(id, &row["audio"])?;
            if let Some(expected) = expected {
                let segment = expected
                    .get(plans.len())
                    .filter(|s| s.id == id)
                    .ok_or_else(|| io_error("FLAC segment order changed"))?;
                super::super::recording_export::header(
                    output,
                    &format!("{id}.flac"),
                    segment.bytes,
                )?;
                output.write_all(&segment.header)?;
                next.byte_limit = Some(segment.bytes - segment.header.len() as u64);
            }
            encoder = Some(next);
        }
        encoder.as_mut().unwrap().push(pcm, output)
    })?;
    if let Some(encoder) = encoder {
        finish_segment(encoder, output, &mut plans, expected)?;
    }
    let declared = task["tracks"][0]["segments"]
        .as_array()
        .ok_or_else(|| io_error("missing FLAC source segments"))?;
    let nonempty: Vec<_> = declared
        .iter()
        .filter(|s| number(&s["samples"]) > 0)
        .collect();
    if nonempty.len() != plans.len()
        || plans
            .iter()
            .zip(nonempty)
            .any(|(p, s)| s["segment_id"] != p.id || number(&s["samples"]) != p.samples)
        || expected.is_some_and(|p| p != plans)
    {
        return Err(io_error("FLAC source sample count changed"));
    }
    Ok((plans, lines, hash))
}
pub(super) struct Plan {
    segments: Vec<Segment>,
    manifest: Vec<u8>,
    lines: u64,
    hash: [u8; 64],
}
pub(super) fn prepare(path: &FsPath, task: &Value, cancel: &AtomicBool) -> io::Result<(u64, Plan)> {
    let (segments, lines, hash) = encode(path, task, cancel, None, &mut io::sink())?;
    if lines > 0o77777777777 {
        return Err(io_error("FLAC timeline exceeds POSIX tar limit"));
    }
    let summaries: Vec<_> = segments.iter().map(|s| json!({"segment_id":s.id,"file":format!("{}.flac",s.id),
        "samples":s.samples.to_string(),"flac_bytes":s.bytes.to_string(),"clipped_values":s.clipped.to_string()})).collect();
    let manifest = serde_json::to_vec(&json!({"format":"websa-audio-flac-export-v1","task":task,
        "source":"published_audio","protocol_input":false,"encoding":"FLAC signed 24-bit PCM",
        "quantization":{"scale":8388608,"rounding":"nearest, ties away from zero","minimum":-8388608,"maximum":8388607,
            "clipping":"saturate rounded values outside signed 24-bit range; count per interleaved channel value",
            "dither":false,"normalization":false,"resampling":false,"float32_bit_exact":false},
        "local_controls":"browser volume/pan/mute/solo excluded; shared backend processing included",
        "time":"original AUD2 per-frame provenance in frames.jsonl; segment-local sample zero; no implied UTC or gap filling",
        "segments":summaries}))?;
    if manifest.len()
        + segments
            .iter()
            .map(|s| s.header.len() + s.id.len() + std::mem::size_of::<Segment>())
            .sum::<usize>()
        > MAX_PLAN
    {
        return Err(io_error("FLAC export plan exceeds 2 MiB"));
    }
    let mut length = 2048
        + manifest.len() as u64
        + pad(manifest.len() as u64) as u64
        + lines
        + pad(lines) as u64;
    for segment in &segments {
        if segment.bytes > 0o77777777777 {
            return Err(io_error("FLAC member exceeds POSIX tar limit"));
        }
        length = length
            .checked_add(512 + segment.bytes + pad(segment.bytes) as u64)
            .ok_or_else(|| io_error("FLAC tar length overflow"))?;
    }
    Ok((
        length,
        Plan {
            segments,
            manifest,
            lines,
            hash,
        },
    ))
}
pub(super) fn emit(
    output: &mut (impl Write + ?Sized),
    path: &FsPath,
    task: &Value,
    plan: &Plan,
    cancel: &AtomicBool,
) -> io::Result<()> {
    use super::super::recording_export::header;
    header(output, "task.json", plan.manifest.len() as u64)?;
    output.write_all(&plan.manifest)?;
    output.write_all(&[0; 512][..pad(plan.manifest.len() as u64)])?;
    header(output, "frames.jsonl", plan.lines)?;
    let mut timeline_bytes = 0u64;
    let (_, lines, hash) = super::audio::visit(path, task, cancel, |line, _, _, _, _| {
        timeline_bytes += line.len() as u64 + 1;
        if timeline_bytes > plan.lines {
            return Err(io_error("FLAC timeline exceeds verified member length"));
        }
        output.write_all(line)?;
        output.write_all(b"\n")
    })?;
    if lines != plan.lines || hash != plan.hash {
        return Err(io_error("FLAC timeline changed during export"));
    }
    output.write_all(&[0; 512][..pad(lines)])?;
    let (_, _, hash) = encode(path, task, cancel, Some(&plan.segments), output)?;
    if hash != plan.hash {
        return Err(io_error("FLAC source changed during export"));
    }
    output.write_all(&[0; 1024])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_scale_quantization_handles_ties_signed_zero_subnormals_and_clipping() {
        let cases = [
            (0.0, 0, false),
            (-0.0, 0, false),
            (f32::from_bits(1), 0, false),
            (0.5, 4194304, false),
            (-0.5, -4194304, false),
            (1.0, 8388607, true),
            (-1.0, -8388608, false),
            (2.0, 8388607, true),
            (-2.0, -8388608, true),
            (f32::MAX, 8388607, true),
            (0.5 / 8388608.0, 1, false),
            (-0.5 / 8388608.0, -1, false),
            (8388607.0 / 8388608.0, 8388607, false),
        ];
        for (value, sample, clipped) in cases {
            assert_eq!(quantize(value), (sample, clipped), "{value}");
        }
    }
    #[test]
    fn larger_reencoding_cannot_fill_the_verified_http_length_before_rejection() {
        let audio = json!({"channels":1,"rate_hz":48000.0});
        let pcm: Vec<_> = (0..4096)
            .flat_map(|i| (i as f32 * 0.137).sin().to_le_bytes())
            .collect();
        let mut encoder = Encoder::new("segment", &audio).unwrap();
        encoder.byte_limit = Some(16);
        let mut output = Vec::new();
        assert!(encoder.push(&pcm, &mut output).is_err());
        assert!(output.is_empty());
    }
    #[test]
    fn encoder_is_independent_of_packet_sizes_and_retains_a_single_short_final_frame() {
        for (channels, count) in [(1, 1), (2, 3), (1, 4097), (2, 8195)] {
            let pcm: Vec<_> = (0..count * channels)
                .flat_map(|i| (((i as f32 * 0.031).sin()) * 1.1).to_le_bytes())
                .collect();
            let audio = json!({"channels":channels,"rate_hz":24000.0});
            let mut a = Encoder::new("segment", &audio).unwrap();
            let mut bytes = Vec::new();
            a.push(&pcm, &mut bytes).unwrap();
            let plan = a.finish(&mut bytes).unwrap();
            assert_eq!(plan.samples, count as u64);
            assert_eq!(plan.bytes, bytes.len() as u64 + 42);
            assert_eq!(&plan.header[..8], b"fLaC\x80\0\0\x22");
            assert_eq!(&plan.header[8..12], &[0x10, 0, 0x10, 0]);
            let mut b = Encoder::new("segment", &audio).unwrap();
            let mut partitioned = Vec::new();
            for chunk in pcm.chunks(7 * channels * 4) {
                b.push(chunk, &mut partitioned).unwrap();
            }
            assert_eq!(b.finish(&mut partitioned).unwrap(), plan);
            assert_eq!(partitioned, bytes);
        }
    }
}
pub(crate) async fn export(
    State(app): State<Arc<App>>,
    Path(task): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if !params.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if super::audio_files::task_is_direct(&app, &task) {
        return super::audio_files::export(app, task, Some("flac")).await;
    }
    super::super::recording_export::download(
        app,
        task,
        "audio-flac-s24.tar",
        prepare,
        |output, path, task, plan, cancel| emit(output, path, task, plan, cancel),
    )
    .await
}
