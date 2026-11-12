use super::*;

fn shaped_noise(meta:display_fft::Meta,n:usize)->Vec<f64>{
    let mut seed=971u64;
    (0..n).map(|i|{
        let hz=meta.center+(i as f64-n as f64/2.0)*meta.rate/n as f64;
        let baseline=1.0+63.0*(-((hz-6e6)/8e6).powi(2)).exp()+8.0*(-((hz-16e6)/4e6).powi(2)).exp();
        let mut sum=0.0;
        for _ in 0..4 {seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;let u=((seed>>11) as f64+0.5)/(1u64<<53) as f64;sum-=u.ln();}
        baseline*sum/4.0
    }).collect()
}
fn meta()->display_fft::Meta {display_fft::Meta{epoch:1,generation:1,center:17_854_668.510262143,rate:62_500_000.0,bandwidth:50e6,scale:1.0}}

#[test]
fn frequency_dependent_noise_is_not_an_active_wide_signal(){
    for m in [meta(),display_fft::Meta{center:11e6,rate:7_812_500.0,bandwidth:6_250_000.0,..meta()}] {
        let p=shaped_noise(m,discovery_fft(m.rate));let rows=detect(m,&p,(9e6,10e6),8.0).0;
        assert!(rows.is_empty(),"noise classified as a signal: {rows:?}");
    }
}

#[test]
fn narrow_stations_remain_separate_above_a_raised_noise_floor(){
    let m=meta();let n=discovery_fft(m.rate);let bin=m.rate/n as f64;let start=m.center-m.rate/2.0;let mut p=shaped_noise(m,n);
    let stations=[9.5e6,9.57e6,9.62e6,9.685e6,9.81e6,9.875e6];
    for hz in stations {for offset in [-2317.0,-713.0,0.0,713.0,2317.0] {
        let peak=(hz+offset-start)/bin;let at=peak.round() as usize;
        for i in at-4..=at+4 {p[i]+=(if offset==0.0 {100000.0}else{4000.0})*(-0.5*((i as f64-peak)/0.7).powi(2)).exp();}
    }}
    let rows=detect(m,&p,(9e6,10e6),8.0).0;
    for hz in stations {assert!(rows.iter().any(|r|r.lo<hz&&r.hi>hz),"missing station {hz}: {rows:?}");}
    assert!(rows.iter().all(|r|r.hi-r.lo<30000.0),"merged into noise: {rows:?}");
}

#[test]
fn remote_quiet_spectrum_cannot_change_the_hf_noise_reference(){
    let m=meta();let n=discovery_fft(m.rate);let mut p=shaped_noise(m,n);let before=estimate_floor(m,&p);
    for (i,v) in p.iter_mut().enumerate(){let hz=m.center+(i as f64-n as f64/2.0)*m.rate/n as f64;if hz>30e6 {*v*=1e-6;}}
    let after=estimate_floor(m,&p);
    for i in 0..n {let hz=m.center+(i as f64-n as f64/2.0)*m.rate/n as f64;if (9e6..10e6).contains(&hz){assert_eq!(before[i],after[i]);}}
}

#[test]
fn a_noise_level_change_cannot_leave_a_stale_low_reference(){
    let m=meta();let n=discovery_fft(m.rate);let mut p=shaped_noise(m,n);let mut psd=Psd::new(n);let before=psd.floor(m,&p).to_vec();
    for value in &mut p {*value*=100.0;}
    let after=psd.floor(m,&p);let rows=detect_floor(m,&p,(9e6,10e6),8.0,Some(after)).0;assert!(rows.is_empty(),"noise rise created {rows:?}");
    for (&a,&b) in before.iter().zip(after){assert!((b/a-100.0).abs()<1e-8);}
    for value in &mut p {*value/=100.0;}
    let down=psd.floor(m,&p);for (&a,&b) in before.iter().zip(down){assert!((b/a-1.0).abs()<1e-8);}
}

#[test]
fn welch_noise_reference_estimates_mean_power_without_quantile_bias(){
    let m=display_fft::Meta{center:10e6,rate:1e6,bandwidth:800000.0,..meta()};let n=FFT;let mut seed=61u64;
    let raw:Vec<i16>=(0..n*5).map(|_|{seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;((seed%20001) as i32-10000) as i16}).collect();
    let mut psd=Psd::new(n);let power=psd.compute(&raw,m.rate,m.scale);let floor=psd.floor(m,&power);
    let first=n/10;let end=n-first;let margin=10.0*(power[first..end].iter().sum::<f64>()/floor[first..end].iter().sum::<f64>()).log10();
    assert!(margin.abs()<0.8,"pure noise mean margin {margin} dB");
}

#[test]
#[ignore="requires explicitly supplied recorded RF IQ outside the checkout"]
fn recorded_hf_noise_and_narrow_stations_do_not_merge(){
    let root=std::path::PathBuf::from(std::env::var_os("WEBSA_SCAN_REPLAY_DIR").expect("set WEBSA_SCAN_REPLAY_DIR"));
    let metadata:Value=serde_json::from_slice(&std::fs::read(root.join("capture-metadata.json")).unwrap()).unwrap();
    let mut model=Psd::new(MAX_DISCOVERY_FFT);
    for block in metadata["blocks"].as_array().unwrap() {
        let path=root.join(format!("capture-{}.ci16",block["index"]));let bytes=std::fs::read(path).unwrap();
        let raw:Vec<i16>=bytes.chunks_exact(2).map(|v|i16::from_le_bytes([v[0],v[1]])).collect();
        let m=display_fft::Meta{center:metadata["center_hz"].as_f64().unwrap(),rate:metadata["input_rate_hz"].as_f64().unwrap(),
            bandwidth:metadata["bandwidth_hz"].as_f64().unwrap(),scale:block["scale_to_v"].as_f64().unwrap() as f32,..meta()};
        let power=model.compute(&raw,m.rate,m.scale);let floor=model.floor(m,&power);let rows=detect_floor(m,&power,(9e6,10e6),8.0,Some(floor)).0;
        assert!(rows.len()>=3,"missing recorded stations: {rows:?}");
        assert!(rows.iter().all(|r|r.hi-r.lo<100000.0),"false wide candidate: {rows:?}");
        assert!(rows.iter().any(|r|r.lo<9.5e6&&r.hi>9.5e6),"missing recorded 9.5 MHz station: {rows:?}");
    }
}
