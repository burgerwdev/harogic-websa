//! Compare restored production DFN PCM against the preserved pre-guard baseline.
//! nr_dfn_baseline NATIVE_BASELINE_DIR REPORT.json
use std::{fs,path::Path};
use websa_channelizer::nr::{NoiseReduction,NrAlgorithm};
fn floats(path:&Path)->Result<Vec<f32>,Box<dyn std::error::Error>> {
    let bytes=fs::read(path)?;
    if bytes.len()%4!=0 {return Err("partial float".into());}
    Ok(bytes.chunks_exact(4).map(|b|f32::from_le_bytes(b.try_into().unwrap())).collect())
}
fn main()->Result<(),Box<dyn std::error::Error>> {
    let args:Vec<_>=std::env::args().collect();
    if args.len()!=3 {return Err("nr_dfn_baseline NATIVE_BASELINE_DIR REPORT.json".into());}
    let root=Path::new(&args[1]);let input=floats(&root.join("nr_input.f32"))?;
    let mut reports=Vec::new();
    for db in [10,13,20] {
        let mut nr=NoiseReduction::new(48000);nr.configure(48000,NrAlgorithm::Dfn,0.25,db as f32);
        if nr.status().effective!=Some(NrAlgorithm::Dfn) {return Err(format!("DFN unavailable: {:?}",nr.status()).into());}
        let delay=nr.latency_samples().unwrap() as usize;
        if delay!=1440 {return Err(format!("unexpected restored DFN delay {delay}").into());}
        let mut padded=input.clone();padded.resize(input.len()+delay+2048,0.0);
        let mut output=Vec::new();let mut block=Vec::new();
        for chunk in padded.chunks(1799) {nr.process(chunk,&mut block)?;output.extend(block.iter().map(|x|2.0*x));}
        let expected=floats(&root.join(format!("dfn{db}.f32")))?;
        if expected.len()!=input.len() || output.len()<delay+input.len() {return Err("baseline sample count mismatch".into());}
        let actual=&output[delay..delay+input.len()];
        let changed=actual.iter().zip(&expected).filter(|(a,b)|a.to_bits()!=b.to_bits()).count();
        let error=actual.iter().zip(&expected).map(|(a,b)|(a-b).abs()).fold(0.0f32,f32::max);
        reports.push(serde_json::json!({"atten_db":db,"model":nr.model_id(),"samples":actual.len(),
            "latency_samples":delay,"bitwise_different_samples":changed,"max_absolute_error":error}));
        println!("{}",reports.last().unwrap());
        if changed!=0 {return Err(format!("DFN{db} differs from preserved production baseline in {changed} samples").into());}
    }
    fs::write(&args[2],serde_json::to_vec_pretty(&reports)?)?;Ok(())
}
