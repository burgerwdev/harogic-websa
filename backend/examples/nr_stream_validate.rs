//! Rust production NR replay / latency / per-callback performance evidence.
//! nr_stream_validate INPUT.f32 OUTDIR [AM|USB] [atten_db]
use std::{fs,io::Write,path::Path,time::Instant};
use websa_channelizer::nr::{NoiseReduction,NrAlgorithm,prepare_models};
fn main()->Result<(),Box<dyn std::error::Error>> {
    let args:Vec<_>=std::env::args().collect();
    if args.len()<3 {return Err("nr_stream_validate INPUT.f32 OUTDIR [am|usb] [atten_db]".into());}
    let bytes=fs::read(&args[1])?;if bytes.len()%4!=0 {return Err("partial float sample".into());}
    let input:Vec<f32>=bytes.chunks_exact(4).map(|b|f32::from_le_bytes(b.try_into().unwrap())).collect();
    if input.iter().any(|x|!x.is_finite()) {return Err("nonfinite input".into());}
    let mode=args.get(3).map(String::as_str).unwrap_or("usb");
    let atten=args.get(4).map(|v|v.parse()).transpose()?.unwrap_or(20.0);
    let root=Path::new(&args[2]);fs::create_dir_all(root)?;
    prepare_models();let mut results=Vec::new();
    for id in ["dfn","gtcrn","ul-unas","fastenhancer"] {
        let algorithm=NrAlgorithm::parse(id).unwrap();let mut nr=NoiseReduction::new(48000);
        nr.configure(48000,algorithm,0.25,atten);
        if nr.status().effective!=Some(algorithm) {return Err(format!("{id}: {:?}",nr.status()).into());}
        let delay=nr.latency_samples().unwrap() as usize;
        let mut padded=input.clone();padded.resize(input.len()+delay+4096,0.0);
        let mut pcm=Vec::new();let mut block=Vec::new();let mut times=Vec::new();let started=Instant::now();
        for chunk in padded.chunks(1799) {let t=Instant::now();nr.process(chunk,&mut block)?;times.push(t.elapsed().as_nanos() as u64);pcm.extend_from_slice(&block);}
        let seconds=started.elapsed().as_secs_f64();
        let aligned=&pcm[delay..delay+input.len()];
        if aligned.iter().any(|x|!x.is_finite()) {return Err("nonfinite output".into());}
        let mut file=std::io::BufWriter::new(fs::File::create(root.join(format!("{id}.f32")))?);
        for v in aligned {file.write_all(&v.to_le_bytes())?;}file.flush()?;
        times.sort_unstable();let t=times.len();
        let record=serde_json::json!({"algorithm":id,"model":nr.model_id(),"status":nr.status().state,
            "input_samples":input.len(),"output_samples":aligned.len(),"mode":mode,"atten_db":atten,
            "latency_samples":delay,"latency_ms":delay as f64/48.0,"seconds":seconds,
            "rtf":seconds/(input.len() as f64/48000.0),"callback_samples":1799,
            "callback_p99_ms":times[t*99/100] as f64/1e6,"callback_max_ms":times[t-1] as f64/1e6,
            "peak":aligned.iter().fold(0.0f32,|p,v|p.max(v.abs())),
            "rms":(aligned.iter().map(|v|(*v as f64).powi(2)).sum::<f64>()/aligned.len() as f64).sqrt()});
        println!("{record}");results.push(record);
    }
    fs::write(root.join("validation.json"),serde_json::to_vec_pretty(&results)?)?;Ok(())
}
