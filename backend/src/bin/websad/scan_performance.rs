use super::*;

#[test]
fn supplied_background_is_borrowed_and_preserves_detections() {
    let meta = display_fft::Meta { epoch: 1, generation: 1, center: 100e6,
        rate: 1e6, bandwidth: 800000.0, scale: 1e-4 };
    let mut power=vec![1.0;FFT];
    power[20000..20010].fill(100.0); power[22000..25000].fill(50.0);
    let floor=estimate_floor(meta,&power);
    for range in [(99.6e6,100.4e6),(99.8e6,99.9e6),(100.35e6,100.45e6)] {
        let (expected,background,reliable)=detect(meta,&power,range,8.0);
        let (actual,borrowed,observed)=detect_floor(meta,&power,range,8.0,Some(&floor));
        assert!(matches!(borrowed,std::borrow::Cow::Borrowed(_)));
        assert_eq!(borrowed.as_ref(),background); assert_eq!(observed,reliable);
        assert_eq!(actual.len(),expected.len());
        for (a,b) in actual.iter().zip(expected) {
            assert_eq!((a.lo,a.hi,a.excess,a.power,a.edge),(b.lo,b.hi,b.excess,b.power,b.edge));
        }
    }
}

#[test]
fn reused_feature_workspaces_do_not_leak_power_or_sample_geometry() {
    let mut analyzer=FeatureAnalyzer::default();
    for (n,rate,offset) in [(38400,48000.0,300.0),(153600,192000.0,-1500.0),
        (4096,8000.0,0.0),(76800,96000.0,100.0),(38400,48000.0,300.0)] {
        let iq:Vec<f32>=(0..n).flat_map(|i| {
            let t=i as f64/rate;let phase=std::f64::consts::TAU*offset*t;
            let amplitude=0.4*(1.0+0.4*(std::f64::consts::TAU*700.0*t).sin());
            [(amplitude*phase.cos()) as f32,(amplitude*phase.sin()) as f32]
        }).collect();
        for samples in [iq,vec![0.0;2*n]] {
            let expected=features_iq(&samples,rate,offset);
            let result=analyzer.analyze(&samples,rate,offset);
            assert_eq!((result.label,result.verdict,result.mode),(expected.label,expected.verdict,expected.mode));
            assert_eq!(result.evidence,expected.evidence);
        }
    }
    assert!(analyzer.ffts.len()<=5);
    assert!(analyzer.ffts.keys().all(|&size|size<=16384));
    assert!(analyzer.fm.capacity()<=MAX_RETAINED_FEATURE_SAMPLES);
    analyzer.analyze(&vec![0.0;2*(MAX_RETAINED_FEATURE_SAMPLES+2)],1e6,0.0);
    assert_eq!(analyzer.fm.capacity(),0);
}

#[test]
fn zero_offset_preserves_signed_zero_discriminator_evidence() {
    let rate=8000.0;let mut phase=0.0;
    let iq:Vec<f32>=(0..6400).flat_map(|i| {
        let frequency=if i/100%2==0 {500.0}else{-500.0};
        phase+=std::f64::consts::TAU*frequency/rate;
        [(0.0*phase.cos()) as f32,(0.0*phase.sin()) as f32]
    }).collect();
    let result=features_iq(&iq,rate,0.0);
    assert_eq!(result.label,"unknown");
    assert!((result.evidence["residual_frequency_hz"].as_f64().unwrap()-240.03751253994153).abs()<1e-8);
    assert!((result.evidence["frequency_deviation_rms_hz"].as_f64().unwrap()-950.0168785089792).abs()<1e-8);
}

#[test]
#[ignore = "manual CPU benchmark"]
fn observation_and_identification_cost() {
    let meta = display_fft::Meta { epoch: 1, generation: 1, center: 100e6,
        rate: 1e6, bandwidth: 800000.0, scale: 1e-4 };
    let mut seed = 17u64;
    let raw: Vec<i16> = (0..BLOCK * 2).map(|_| {
        seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
        ((seed % 2001) as i32 - 1000) as i16
    }).collect();
    let mut psd = Psd::new(FFT);
    let mut costs = Vec::new();
    for _ in 0..51 {
        let start = Instant::now();
        let spectrum = psd.compute(&raw, meta.rate, meta.scale);
        let floor = psd.floor(meta, &spectrum);
        std::hint::black_box(prepare_observation(meta, &spectrum, true, None, &[], None, 8.0, Some(&floor)));
        costs.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    costs.remove(0); costs.sort_by(f64::total_cmp);
    println!("observation median_ms={:.3} p95_ms={:.3}", costs[25], costs[47]);
    let rate = 192000.0;
    let mut phase = 0.0;
    let iq: Vec<f32> = (0..153600).flat_map(|i| {
        let t = i as f64 / rate;
        phase += std::f64::consts::TAU / rate * (8000.0 * (std::f64::consts::TAU * 1000.0 * t).sin()
            + 1500.0 * (std::f64::consts::TAU * 19000.0 * t).cos());
        [(0.4 * phase.cos()) as f32, (0.4 * phase.sin()) as f32]
    }).collect();
    let mut analyzer = FeatureAnalyzer::default();
    let mut costs = Vec::new();
    for _ in 0..21 {
        let start = Instant::now();
        let result = analyzer.analyze(&iq, rate, 0.0);
        assert_eq!(result.label, "FM stereo pilot");
        costs.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    costs.remove(0); costs.sort_by(f64::total_cmp);
    println!("identify median_ms={:.3} p95_ms={:.3}", costs[10], costs[18]);
}
