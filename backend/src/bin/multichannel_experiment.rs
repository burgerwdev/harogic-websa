//! P2 offline experiments. No product endpoints, recording jobs, demodulators or SDK.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Cursor},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use websa_channelizer::{
    iq_archive::{read_block_at, scan, ArchiveWriter, IqBlockMeta, SharedIqBlock, MAX_BLOCK_BYTES},
    rate_plan::RatePlan,
    FineBank, PfbStream,
};

fn peak_rss_kib() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|s| s.starts_with("VmHWM:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}
fn dynamic(channels: usize, seconds: f64) -> Result<Value, String> {
    const FS: f64 = 62_500_000.0;
    const PACKET: usize = 16_240;
    let bins: Vec<u32> = (0..channels.min(8) as u32).collect();
    let mut stream =
        PfbStream::with_taps(FS, PACKET, bins.clone(), 7, 16).ok_or("PFB unavailable")?;
    let plan = RatePlan::baseband(stream.sample_rate, 3_000.0, 1_000.0, 6_000.0)?;
    let make = || {
        FineBank::new(
            &stream,
            50e6,
            PACKET / stream.input_hop() + 2,
            channels + 1,
            1e8,
            Duration::from_secs(60),
        )
        .ok_or("fine bank unavailable")
    };
    let mut bank = make()?;
    let mut reference = make()?;
    for n in 0..channels {
        bank.subscribe_planned(
            n as u64 + 1,
            bins[n % bins.len()],
            ((n / bins.len()) as f64 - 8.0) * 1_000.0,
            plan.clone(),
        )?;
    }
    reference.subscribe_planned(1, 0, -8_000.0, plan.clone())?;
    let packets = ((FS * seconds / PACKET as f64).ceil() as usize).max(4);
    let input_samples = packets * PACKET;
    let mut seed = 20261031u32;
    let raw: Vec<i16> = (0..2 * PACKET)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as i16
        })
        .collect();
    let start = Instant::now();
    let mut delivered = 0u64;
    let mut count = BTreeMap::<u64, u64>::new();
    let mut first = BTreeMap::<u64, u64>::new();
    let mut fine_ns = 0u128;
    let mut pfb_ns = 0u64;
    let mut maximum_fine_ns = 0u128;
    let mut retuned = false;
    let mut drain = |stream: &mut PfbStream| -> Result<bool, String> {
        let Some(packet) = stream.take() else {
            return Ok(false);
        };
        if packet.seq != delivered || packet.input_index != delivered * PACKET as u64 {
            return Err("PFB order/clock mismatch".into());
        }
        if delivered as usize == packets / 2 {
            bank.retune_planned(
                channels as u64,
                channels as u64 + 1,
                bins[(channels - 1) % bins.len()],
                9_000.0,
                plan.clone(),
            )?;
            retuned = true;
            // Failed admission must not remove an existing receiver or allocate its identity.
            if bank
                .subscribe_planned(9999, bins[0], 1e9, plan.clone())
                .is_ok()
            {
                return Err("bad geometry accepted".into());
            }
        }
        let mut reference_bytes = Vec::new();
        reference.deliver(&packet, |out| {
            reference_bytes.extend_from_slice(out.iq);
            Ok(())
        })?;
        let began = Instant::now();
        bank.deliver(&packet, |out| {
            let index = count.entry(out.channel_id).or_default();
            if out.output_index != *index || out.output_rate != plan.output_rate || out.epoch != 7 {
                return Err("fine clock/identity mismatch");
            }
            if out.channel_id == 1
                && (out.iq.len() != reference_bytes.len()
                    || out
                        .iq
                        .iter()
                        .zip(&reference_bytes)
                        .any(|(a, b)| a.to_bits() != b.to_bits()))
            {
                return Err("unaffected samples changed");
            }
            if out.iq.iter().any(|x| !x.is_finite()) {
                return Err("nonfinite DSP output");
            }
            if !out.iq.is_empty() {
                let base = first.entry(out.channel_id).or_insert(out.input_index);
                if out.input_index
                    != *base + *index * stream.input_hop() as u64 * plan.decimate as u64
                {
                    return Err("fine source position gap");
                }
            }
            *index += (out.iq.len() / 2) as u64;
            Ok(())
        })?;
        let elapsed = began.elapsed().as_nanos();
        fine_ns += elapsed;
        maximum_fine_ns = maximum_fine_ns.max(elapsed);
        pfb_ns += packet.process_ns;
        delivered += 1;
        Ok(true)
    };
    for seq in 0..packets {
        loop {
            match stream.submit(seq as u64, seq as u64 * PACKET as u64, &raw) {
                Ok(()) => break,
                Err(-2) => {
                    if !drain(&mut stream)? {
                        return Err("backpressure without output".into());
                    }
                }
                Err(code) => return Err(format!("PFB submit {code}")),
            }
        }
        while drain(&mut stream)? {}
    }
    stream.finish()?;
    while drain(&mut stream)? {}
    let wall = start.elapsed().as_secs_f64();
    let hop = stream.input_hop() as u64;
    let total = input_samples as u64 / hop / plan.decimate as u64;
    if delivered != packets as u64
        || !retuned
        || (1..channels as u64).any(|id| count.get(&id) != Some(&total))
    {
        return Err("tail/sample conservation failed".into());
    }
    let before = ((packets / 2 * PACKET) as u64 / hop) / plan.decimate as u64;
    let after = ((input_samples as u64 / hop) - ((packets / 2 * PACKET) as u64 / hop))
        / plan.decimate as u64;
    if count.get(&(channels as u64)) != Some(&before)
        || count.get(&(channels as u64 + 1)) != Some(&after)
    {
        return Err("retuned stream tail accounting failed".into());
    }
    Ok(
        json!({"kind":"offline_dynamic_iq", "channels":channels, "active_coarse_bins":bins.len(),
        "input_rate_hz":FS,"usable_bandwidth_hz":50e6,"taps_per_phase":16,"coarse_rate_hz":stream.sample_rate,
        "passband_hz":3000,"transition_hz":1000,"decimate":plan.decimate,"actual_rate_hz":plan.output_rate,
        "input_samples":input_samples,"packets":packets,"wall_seconds":wall,"input_seconds":input_samples as f64/FS,
        "realtime_factor":wall/(input_samples as f64/FS),"pfb_worker_seconds":pfb_ns as f64/1e9,
        "fine_seconds":fine_ns as f64/1e9,"max_fine_block_ms":maximum_fine_ns as f64/1e6,
        "samples_per_unaffected_channel":total,"sample_counts":count,"retune_old_samples":before,"retune_new_samples":after,
        "unaffected_byte_exact":true,"clock_and_tail":true,"rejected_plan_preserved":true,
        "peak_process_rss_kib":peak_rss_kib(),"product_capacity_accepted":false}),
    )
}
fn metadata(samples: usize, rate: f64, first: usize) -> IqBlockMeta {
    IqBlockMeta {
        receiver_uid: "experiment-r1".into(),
        source_id: "fixture".into(),
        track_id: "track-iq".into(),
        epoch: "7".into(),
        channel_id: "9".into(),
        config_revision: "1".into(),
        first_input_index: (9_007_199_254_740_993u64 + first as u64).to_string(),
        first_output_index: first.to_string(),
        fraction_num: "0".into(),
        fraction_den: "1".into(),
        samples: samples as u32,
        input_rate_hz: rate,
        actual_rate_hz: rate,
        sample_format: "cf32_le".into(),
        amplitude_scale: 1.0,
        delay_seconds: None,
    }
}
fn archive_case(dir: &Path, name: &str, bytes: &[u8], rate: f64, level: i32) -> io::Result<Value> {
    let path = dir.join(format!("{name}-zstd-{level}.wbiq"));
    let started = Instant::now();
    let mut writer = ArchiveWriter::new(File::create(&path)?, level)?;
    let block_bytes = ((rate * 0.25).ceil() as usize * 8).min(MAX_BLOCK_BYTES);
    let mut index = Vec::new();
    let mut first = 0;
    for chunk in bytes.chunks(block_bytes) {
        let block = SharedIqBlock::new(metadata(chunk.len() / 8, rate, first), Arc::from(chunk))?;
        first += chunk.len() / 8;
        index.push(writer.append(&block)?);
    }
    writer.finish()?.sync_all()?;
    let encode = started.elapsed().as_secs_f64();
    let began = Instant::now();
    let decoded = scan(File::open(&path)?)?;
    let decode = began.elapsed().as_secs_f64();
    assert!(decoded.complete);
    assert_eq!(decoded.index, index);
    assert_eq!(
        decoded
            .blocks
            .iter()
            .flat_map(|b| b.cf32_le.iter().copied())
            .collect::<Vec<_>>(),
        bytes
    );
    let file = fs::read(&path)?;
    let last = decoded.index.last().unwrap().offset as usize;
    let truncated = scan(Cursor::new(&file[..last + 20]))?;
    assert!(!truncated.complete);
    assert_eq!(truncated.blocks.len(), decoded.blocks.len() - 1);
    let seek_started = Instant::now();
    let middle = index.len() / 2;
    let block = read_block_at(&mut File::open(&path)?, &index[middle])?;
    assert_eq!(
        block.cf32_le.as_ref(),
        decoded.blocks[middle].cf32_le.as_ref()
    );
    let seek_ms = seek_started.elapsed().as_secs_f64() * 1000.0;
    let raw = bytes.len();
    let payload: usize = index.iter().map(|i| i.stored_bytes as usize).sum();
    Ok(
        json!({"kind":"cf32_lossless_archive","source":name,"level":level,"actual_rate_hz":rate,
        "raw_bytes":raw,"payload_bytes":payload,"file_bytes":file.len(),"payload_ratio":payload as f64/raw as f64,
        "file_ratio":file.len() as f64/raw as f64,"encode_and_write_seconds":encode,"read_and_decode_seconds":decode,
        "blocks":index.len(),"raw_blocks":index.iter().filter(|i|i.codec=="raw").count(),
        "zstd_blocks":index.iter().filter(|i|i.codec=="zstd").count(),"block_byte_limit":block_bytes,
        "random_seek_decode_ms":seek_ms,"byte_exact":true,"metadata_exact":true,"partial_tail_detected":true,"recovered_blocks":truncated.blocks.len(),
        "peak_process_rss_kib":peak_rss_kib()}),
    )
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next()
            .ok_or("usage: multichannel_experiment OUTPUT_DIR [input-seconds, default .05]")?,
    );
    let seconds: f64 = args.next().unwrap_or("0.05".into()).parse()?;
    if !seconds.is_finite() || !(0.001..=10.0).contains(&seconds) {
        return Err("seconds must be .001..10".into());
    }
    fs::create_dir_all(&dir)?;
    let mut report = vec![dynamic(3, seconds)?, dynamic(128, seconds)?];
    let mut seed = 20261031u32;
    let noise: Vec<u8> = (0..262_144)
        .flat_map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            // Finite CF32 components with random mantissas/sign/exponent: deliberately difficult entropy.
            ((seed & 0x807fffff) | (((seed >> 24) % 126 + 1) << 23)).to_le_bytes()
        })
        .collect();
    let tone: Vec<u8> = (0..131_072)
        .flat_map(|n| {
            let p = std::f64::consts::TAU * (n % 64) as f64 / 64.0;
            [(p.cos() * 0.3) as f32, (p.sin() * 0.3) as f32]
                .into_iter()
                .flat_map(f32::to_le_bytes)
        })
        .collect();
    let mut rng = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f64 + 1.0) / (u32::MAX as f64 + 2.0)
    };
    let gaussian: Vec<u8> = (0..131_072)
        .flat_map(|_| {
            let radius = (-2.0 * rng().ln()).sqrt() * 0.02;
            let phase = std::f64::consts::TAU * rng();
            [(radius * phase.cos()) as f32, (radius * phase.sin()) as f32]
                .into_iter()
                .flat_map(f32::to_le_bytes)
        })
        .collect();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let real = fs::read(root.join("tests/fixtures/ft8/real_slot_ja0txu.iq"))?;
    for level in 1..=3 {
        for (name, bytes, rate) in [
            ("synthetic_periodic", &tone[..], 48_000.0),
            ("synthetic_high_entropy", &noise[..], 48_000.0),
            ("synthetic_gaussian_noise", &gaussian[..], 48_000.0),
            ("real_ft8_ja0txu", &real[..], 48_840.130763116205),
        ] {
            report.push(archive_case(&dir, name, bytes, rate, level)?);
        }
    }
    let report = json!({"scope":"P2 offline experiments, no SDK/network/long-running product capacity acceptance",
        "rustc":option_env!("RUSTC_VERSION"),"results":report});
    let json = serde_json::to_string_pretty(&report)?;
    fs::write(dir.join("report.json"), &json)?;
    println!("{json}");
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("experiment failed: {e}");
        std::process::exit(1);
    }
}
