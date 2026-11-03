//! Offline research only. Reuse the native listening demodulator and production NR.
//! Usage: cargo run --release --example offline_ssb_nr -- IQ META OUTDIR
//! Squelch stays open for every branch; volume=2 is applied AFTER NR as in receiver.rs.
use sha2::{Digest, Sha256};
use std::{env, fs, io::Write, path::Path};
use websa_channelizer::nr::{NoiseReduction, NrAlgorithm};
use websa_dsp::analog::{spec_for, AnalogDemod};
use websa_dsp::plugin::{AnalogDemodulator, DemodConfig};

fn save(path: &Path, samples: &[f32]) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::new(fs::File::create(path)?);
    for x in samples {
        out.write_all(&x.to_le_bytes())?;
    }
    out.flush()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        return Err("usage: offline_ssb_nr IQ META OUTDIR".into());
    }
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(&args[2])?)?;
    let rate = meta["global"]["core:sample_rate"]
        .as_f64()
        .ok_or("sample rate")?;
    if meta["global"]["core:datatype"] != "cf32_le" {
        return Err("expected cf32_le".into());
    }
    let bytes = fs::read(&args[1])?;
    if bytes.len() % 8 != 0 {
        return Err("partial IQ sample".into());
    }
    let iq: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    if iq.iter().any(|x| !x.is_finite()) {
        return Err("non-finite IQ".into());
    }
    let blocks = meta["annotations"].as_array().ok_or("block annotations")?;
    let outdir = Path::new(&args[3]);
    fs::create_dir_all(outdir)?;
    let config = DemodConfig::new(rate, 2700.0);
    let mut agc = AnalogDemod::new_prechannelized(spec_for("usb").unwrap(), config, 48000.0);
    let mut raw = AnalogDemod::new_prechannelized(spec_for("usb").unwrap(), config, 48000.0);
    raw.set_agc_enabled(false);
    let (mut offset, mut input, mut unlevelled, mut block) =
        (0, Vec::new(), Vec::new(), Vec::new());
    for row in blocks {
        let start = row["core:sample_start"].as_u64().ok_or("block start")? as usize;
        let n = row["core:sample_count"].as_u64().ok_or("block length")? as usize;
        if start != offset || offset + n > iq.len() / 2 {
            return Err("non-contiguous annotations".into());
        }
        let chunk = &iq[offset * 2..(offset + n) * 2];
        agc.process_into(chunk, &mut block);
        input.extend_from_slice(&block);
        raw.process_into(chunk, &mut block);
        unlevelled.extend_from_slice(&block);
        offset += n;
    }
    if offset * 2 != iq.len() || input.len() != unlevelled.len() {
        return Err("sample count mismatch".into());
    }
    save(&outdir.join("pre_agc.f32"), &unlevelled)?;
    save(&outdir.join("nr_input.f32"), &input)?;
    save(
        &outdir.join("none.f32"),
        &input.iter().map(|x| x * 2.0).collect::<Vec<_>>(),
    )?;
    let mut runs = Vec::new();
    for (name, algorithm, attenuation) in [
        ("dfn10", NrAlgorithm::Dfn, 10.0),
        ("dfn13", NrAlgorithm::Dfn, 13.0),
        ("dfn20", NrAlgorithm::Dfn, 20.0),
        ("wiener025", NrAlgorithm::Wiener, 10.0),
    ] {
        let mut nr = NoiseReduction::new(48000);
        nr.configure(48000, algorithm, 0.25, attenuation);
        if nr.status().effective != algorithm {
            return Err(format!("{name}: {:?}", nr.status()).into());
        }
        let delay = nr.latency_samples().ok_or("unknown NR delay")? as usize;
        let started = std::time::Instant::now();
        let mut enhanced = Vec::new();
        // Finish partial hops and drain causal overlap/lookahead. The zeros are not
        // part of the listening comparison. Each output is trimmed to source length.
        let mut padded = input.clone();
        padded.resize(input.len() + delay + 2048, 0.0);
        for chunk in padded.chunks(1799) {
            nr.process(chunk, &mut block)?;
            enhanced.extend(block.iter().map(|x| x * 2.0));
        }
        if enhanced.len() < delay + input.len() {
            return Err("NR flush too short".into());
        }
        save(
            &outdir.join(format!("{name}.f32")),
            &enhanced[delay..delay + input.len()],
        )?;
        runs.push(serde_json::json!({"name":name,"attenuation_db":attenuation,
            "strength":0.25,"latency_samples":delay,"latency_ms":delay as f64/48.0,
            "effective":format!("{:?}",nr.status().effective),"seconds":started.elapsed().as_secs_f64()}));
    }
    let manifest = serde_json::json!({"constructor":"AnalogDemod::new_prechannelized",
        "input_sha256":format!("{:x}", Sha256::digest(&bytes)),
        "metadata_sha256":format!("{:x}", Sha256::digest(fs::read(&args[2])?)),
        "binary_sha256":format!("{:x}", Sha256::digest(fs::read(env::current_exe()?)?)),
        "input_samples":iq.len()/2,"input_rate_hz":rate,"output_rate_hz":48000,
        "output_samples":input.len(),"mode":"usb","bandwidth_hz":2700,"agc":true,
        "squelch":"open for all branches","volume_after_nr":2.0,
        "demod_delay_samples_at_input_rate":192,"nr":runs});
    fs::write(
        outdir.join("native.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}
