use super::*;

#[test]
fn wide_capture_resolves_adjacent_hf_stations_and_reports_actual_resolution(){
    let m=display_fft::Meta{epoch:1,generation:1,center:17_854_668.510262143,rate:62_500_000.0,bandwidth:50e6,scale:1e-4};
    let n=discovery_fft(m.rate);assert_eq!(n,262144);
    let frequencies=[9_500_000.0,9_512_000.0,9_524_000.0,9_536_000.0,9_548_000.0,9_560_000.0];
    let mut seed=91u64;let mut raw=Vec::with_capacity(n*5);
    for i in 0..n*5/2 {
        let t=i as f64/m.rate;let a=0.5+0.2*(std::f64::consts::TAU*713.0*t).sin()+0.1*(std::f64::consts::TAU*2317.0*t).cos();
        let mut re=0.0;let mut im=0.0;
        for hz in frequencies {let p=std::f64::consts::TAU*(hz-m.center)*t;re+=3000.0*a*p.cos();im+=3000.0*a*p.sin();}
        seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;let noise=(seed%101) as f64-50.0;
        raw.extend_from_slice(&[(re+noise) as i16,(im-noise) as i16]);
    }
    let power=Psd::new(n).compute(&raw,m.rate,m.scale);
    let region=Region{lo:9e6,hi:19e6,origin:"manual".into(),token:1};
    let ready=prepare_observation(m,&power,false,None,&[],Some(region.clone()),8.0,None);
    for hz in frequencies {assert!(ready.incoming.iter().any(|(r,_)|r.lo<hz&&r.hi>hz&&r.hi-r.lo<20000.0),"missing or merged {hz}: {:?}",ready.incoming);}
    let mut d=Data::default();d.region=Some(region);d.apply_observation(m,ready);
    let snapshot=d.snapshot("test",false);assert_eq!(snapshot["fft_size"],n);assert_eq!(snapshot["bin_spacing_hz"],m.rate/n as f64);
    assert!((d.metrics.observed_seconds-0.01048576).abs()<1e-12);
}

#[test]
fn a_wide_component_cannot_expand_its_merge_gap_into_another_signal(){
    let m=display_fft::Meta{epoch:1,generation:1,center:17.85e6,rate:10e6,bandwidth:8e6,scale:1.0};
    let mut power=vec![1.0;FFT];power[30000..34000].fill(100.0);power[34200..38200].fill(100.0);
    let rows=detect(m,&power,(14e6,21e6),8.0).0;
    assert_eq!(rows.len(),2,"{rows:?}");assert!(rows[1].lo-rows[0].hi>20000.0);
}

#[test]
fn discovery_work_is_bounded_and_optional_pilot_width_falls_back_before_admission(){
    for rate in [62_500_000.0,31_250_000.0,15_625_000.0,7_812_500.0,976_562.5] {
        let n=discovery_fft(rate);assert!(n<=MAX_DISCOVERY_FFT);assert!(rate/n as f64<=250.0);
    }
    let hz=100e6+BIN_SPACING*0.49;let mut s=Signal::new(1,hz-20000.0,hz+20000.0,"region");s.probe_bandwidth_hz=180000.0;
    let (config,bw)=probe_channel(&s,2,IQ_FS,100e6,50e6,4).unwrap();assert_eq!(bw,40000.0);assert_eq!(config.freq,hz);
    assert_eq!(config.baseband.unwrap().passband_hz,40000.0);
    // Reproduce both reported failures without changing any channel geometry.
    for (width,expected) in [(751495.361328125,"baseband requirements exceed coarse rate"),(500000.0,"channel occupied bandwidth and transition exceed capture or coarse passband")] {
        let signal=Signal::new(1,hz-width/2.0,hz+width/2.0,"region");
        let error=probe_channel(&signal,2,IQ_FS,100e6,50e6,16).unwrap_err();assert_eq!(error,expected);assert!(is_probe_passband_error(&error));
    }
}

fn collector_engine(excess:f64)->Arc<Engine>{
    let engine=Engine::new();let started=Instant::now();
    let entry=sdr_pipeline::ChannelRegistryEntry{channel:2,channel_id:7,epoch:1,frequency_hz:10e6,
        output_rate:48000.0,input_rate:48000.0,group_delay_samples:0.0,state:"active"};
    {let mut d=engine.data.lock();d.capture=json!({"epoch":"1"});let mut signal=Signal::new(1,10e6-6000.0,10e6+6000.0,"region");
        signal.identify_state="sampling".into();signal.excess_db=excess;signal.queued_at=Some(started);d.signals.push(signal);
        d.collector=Some(Collector{id:"s1".into(),token:d.token,entry,iq:vec![],next_seq:None,next_index:None,first_index:None,started,at:"0".into(),stage:0,in_flight:false,attempt:0,available_bandwidth:12000.0});}
    engine
}
fn am_frame(seq:u32,first:u64,count:usize)->IqFrame {
    let samples:Vec<f32>=(first..first+count as u64).flat_map(|i|{
        let t=i as f64/48000.0;let a=0.4*(1.0+0.4*(std::f64::consts::TAU*713.0*t).sin());
        let p=std::f64::consts::TAU*300.0*t;[(a*p.cos()) as f32,(a*p.sin()) as f32]
    }).collect();
    IqFrame::native(websa_channelizer::BasebandMagic::Aux,seq,48000.0,10e6,7,1,first,0.0,48000.0,samples).unwrap()
}
fn wait_scan(engine:&Engine,condition:impl Fn(&Data)->bool){
    let deadline=Instant::now()+Duration::from_secs(3);
    while !condition(&engine.data.lock()){assert!(Instant::now()<deadline,"scan checkpoint timed out");thread::sleep(Duration::from_millis(2));}
}

#[test]
fn strong_am_finishes_from_short_contiguous_iq_with_consistent_halves(){
    let engine=collector_engine(20.0);engine.iq(&[am_frame(1,0,7680)]);
    wait_scan(&engine,|d|d.signals[0].identify_state=="complete");
    let d=engine.data.lock();let s=&d.signals[0];assert_eq!(s.suggested_mode.as_deref(),Some("am"));
    assert_eq!(s.evidence["early_exit"],true);assert_eq!(s.evidence["consistent_independent_halves"],true);
    assert_eq!(s.timing["checkpoints"],1);assert_eq!(s.timing["observation_seconds"],0.16);
    assert!(d.collector.is_none());assert_eq!(d.metrics.early_completed,1);
    assert_eq!(s.evidence["phase_features_computed"],false);
}

#[test]
fn weak_evidence_extends_the_same_lease_to_full_observation(){
    let engine=collector_engine(20.0);engine.data.lock().signals[0].excess_db=6.0;
    let started=engine.data.lock().collector.as_ref().unwrap().started;
    engine.iq(&[am_frame(1,0,7680)]);
    wait_scan(&engine,|d|d.collector.as_ref().is_some_and(|c|c.stage==1&&!c.in_flight));
    engine.iq(&[am_frame(2,7680,11520)]);
    wait_scan(&engine,|d|d.collector.as_ref().is_some_and(|c|c.stage==2&&!c.in_flight));
    {let d=engine.data.lock();assert_eq!(d.collector.as_ref().unwrap().started,started);assert_eq!(d.collector.as_ref().unwrap().first_index,Some(0));}
    engine.iq(&[am_frame(3,19200,19200)]);
    wait_scan(&engine,|d|d.signals[0].identify_state=="complete");
    let d=engine.data.lock();assert_eq!(d.signals[0].suggested_mode.as_deref(),Some("am"));
    assert_eq!(d.signals[0].timing["observation_seconds"],0.8);assert_eq!(d.metrics.extended,2);assert_eq!(d.metrics.early_completed,0);
}

#[test]
fn inconsistent_halves_and_digital_hints_cannot_finish_early(){
    let mut analyzer=FeatureAnalyzer::default();
    for rate in [48000.0,192000.0] {
        let n=(rate*0.16) as usize;let mut seed=51u64;
        let iq:Vec<f32>=(0..2*n).map(|_|{seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;(seed%20001) as f32/10000.0-1.0}).collect();
        assert!(!can_finish_early(&analyzer.analyze(&iq,rate,0.0),12000.0,30.0));
    }
    let features=Features{label:"2FSK-like",verdict:"tentative",mode:None,evidence:json!({"envelope_cv":0.0,"carrier_fraction":0.0})};
    assert!(!can_finish_early(&features,12000.0,30.0));
    let carrier=Features{label:"carrier",verdict:"tentative",mode:Some("cw"),evidence:json!({"envelope_cv":0.0,"carrier_fraction":1.0})};
    assert!(!can_finish_early(&carrier,12000.0,30.0),"an AM speech pause must retain the full observation");
    let engine=collector_engine(30.0);let frame=am_frame(1,0,3840);
    let mut samples=frame.native_iq().unwrap().samples.to_vec();let mut seed=75u64;
    samples.extend((0..7680).map(|_|{seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;(seed%20001) as f32/10000.0-1.0}));
    let frame=IqFrame::native(websa_channelizer::BasebandMagic::Aux,1,48000.0,10e6,7,1,0,0.0,48000.0,samples).unwrap();
    engine.iq(&[frame]);wait_scan(&engine,|d|d.collector.as_ref().is_some_and(|c|c.stage==1&&!c.in_flight));
    assert_ne!(engine.data.lock().signals[0].identify_state,"complete");
}

#[test]
fn stereo_fm_uses_three_pilot_windows_and_both_halves_before_early_completion(){
    let engine=collector_engine(20.0);let rate=192000.0;
    {let mut d=engine.data.lock();let c=d.collector.as_mut().unwrap();c.entry.input_rate=rate;c.entry.output_rate=rate;
        d.signals[0].analysis_bandwidth_hz=180000.0;d.signals[0].probe_bandwidth_hz=180000.0;
        d.signals[0].low_hz=10e6-30000.0;d.signals[0].high_hz=10e6+30000.0;d.signals[0].bandwidth_hz=60000.0;}
    let mut phase=0.0;let samples:Vec<f32>=(0..30720).flat_map(|i|{
        let t=i as f64/rate;phase+=std::f64::consts::TAU/rate*(4000.0+8000.0*(std::f64::consts::TAU*1000.0*t).sin()+1500.0*(std::f64::consts::TAU*19000.0*t).cos());
        let a=0.4*(1.0+0.45*(std::f64::consts::TAU*137.0*t).sin());[(a*phase.cos()) as f32,(a*phase.sin()) as f32]
    }).collect();
    let frame=IqFrame::native(websa_channelizer::BasebandMagic::Aux,1,rate,10e6,7,1,0,0.0,rate,samples).unwrap();
    engine.iq(&[frame]);wait_scan(&engine,|d|d.signals[0].identify_state=="complete");
    let d=engine.data.lock();assert_eq!(d.signals[0].label,"FM stereo pilot");assert_eq!(d.signals[0].evidence["consistent_independent_halves"],true);
    assert_eq!(d.signals[0].evidence["phase_features_computed"],false);assert_eq!(d.signals[0].timing["observation_seconds"],0.16);
    let s=&d.signals[0];assert!((s.center_hz-10e6-4000.0).abs()<100.0);
    assert!(s.center_hz+s.probe_bandwidth_hz/2.0<=10e6+90000.0+1e-6);
    assert!(s.center_hz-s.probe_bandwidth_hz/2.0>=10e6-90000.0-1e-6);
}

#[tokio::test]
async fn notification_connections_are_bounded_released_and_check_origin(){
    let app=Arc::new(App{state:StateMutex::new(DevState::new(json!({"connected":false}),json!({}))),
        clients:PLMutex::new(Vec::new()),next_client:AtomicU64::new(0),epoch:Arc::new(AtomicU64::new(0)),
        identities:channel_identities(),display_version:Arc::new(AtomicU64::new(0))});
    let mut connections=Vec::new();
    for _ in 0..8 {let r=events(State(app.clone()),HeaderMap::new()).await;assert_eq!(r.status(),StatusCode::OK);connections.push(r);}
    assert_eq!(events(State(app.clone()),HeaderMap::new()).await.status(),StatusCode::TOO_MANY_REQUESTS);
    connections.pop();assert_eq!(events(State(app.clone()),HeaderMap::new()).await.status(),StatusCode::OK);
    connections.clear();
    assert_eq!(app.state.lock().unwrap().scan.as_ref().unwrap().data.lock().updates.receiver_count(),0);
    let mut headers=HeaderMap::new();headers.insert("host","127.0.0.1".parse().unwrap());headers.insert("origin","https://example.invalid".parse().unwrap());
    assert_eq!(events(State(app),headers).await.status(),StatusCode::FORBIDDEN);
}

#[test]
fn old_checkpoint_cannot_finish_or_extend_a_restarted_collection(){
    let engine=collector_engine(20.0);
    {let mut d=engine.data.lock();let c=d.collector.as_mut().unwrap();c.attempt=1;c.in_flight=true;d.signals[0].identify_state="analyzing".into();}
    let samples=am_frame(1,0,7680).native_iq().unwrap().samples.to_vec();
    let job=|attempt|FeatureJob{id:"s1".into(),token:1,rate:48000.0,offset:0.0,bandwidth:12000.0,
        iq:samples.clone(),at:"0".into(),source:json!({}),stage:0,attempt,enqueued:Instant::now(),sampling_started:Instant::now(),wide_bandwidth:None,limits:(-6000.0,6000.0)};
    engine.features.send(job(0)).unwrap();wait_scan(&engine,|d|d.metrics.feature_jobs==1);
    {let d=engine.data.lock();assert_eq!(d.signals[0].identify_state,"analyzing");let c=d.collector.as_ref().unwrap();assert_eq!(c.attempt,1);assert_eq!(c.stage,0);assert!(c.in_flight);}
    engine.features.send(job(1)).unwrap();wait_scan(&engine,|d|d.signals[0].identify_state=="complete");
    assert_eq!(engine.data.lock().metrics.early_completed,1);
}

#[test]
fn cancellation_does_not_reset_analysis_cpu_debt(){
    let mut d=Data::default();d.feature_budget.charge(0.05,Instant::now());d.cancel();
    assert!(d.feature_budget.wait(Instant::now())>Duration::from_millis(250));
}

#[test]
fn initially_weak_candidate_spends_no_cpu_on_ineligible_short_checkpoints(){
    let engine=collector_engine(6.0);engine.data.lock().collector.as_mut().unwrap().stage=first_observation_stage(6.0);
    engine.iq(&[am_frame(1,0,19200)]);
    {let d=engine.data.lock();assert_eq!(d.metrics.feature_jobs,0);assert_eq!(d.signals[0].identify_state,"sampling");assert!(!d.collector.as_ref().unwrap().in_flight);}
    engine.iq(&[am_frame(2,19200,19200)]);wait_scan(&engine,|d|d.signals[0].identify_state=="complete");
    let d=engine.data.lock();assert_eq!(d.metrics.feature_jobs,1);assert_eq!(d.signals[0].timing["observation_seconds"],0.8);
    assert_eq!(d.signals[0].suggested_mode.as_deref(),Some("am"));
}

#[test]
fn broad_detection_keeps_measured_width_and_respects_selected_probe_bounds(){
    let meta=display_fft::Meta{epoch:1,generation:1,center:100e6,rate:1e6,bandwidth:800000.0,scale:1e-4};
    for half in [35000.0,110000.0] {
        let mut d=Data::default();d.region=Some(Region{lo:100e6-half,hi:100e6+half,origin:"manual".into(),token:d.token});
        d.apply_observation(meta,Observation{incoming:vec![(Detection{lo:100e6-30000.0,hi:100e6+30000.0,excess:20.0,power:1.0,edge:false},"region")],reliable:true,truncated:0,origin:Some("manual".into()),fft_size:FFT,empty_region:false});
        let s=&d.signals[0];assert_eq!(s.bandwidth_hz,60000.0);assert_eq!(s.analysis_bandwidth_hz,60000.0);
        assert_eq!(s.probe_bandwidth_hz,if half>90000.0{180000.0}else{60000.0});
        assert!(s.center_hz-s.probe_bandwidth_hz/2.0>=100e6-half);
        assert!(s.center_hz+s.probe_bandwidth_hz/2.0<=100e6+half);
    }
}

#[test]
fn wide_pilot_check_recovers_fm_without_relabeling_other_modulations_or_a_neighbour(){
    let rate=325520.8333333333;let mut phase=0.0;
    let iq:Vec<f32>=(0..(rate*0.16) as usize).flat_map(|i|{
        let t=i as f64/rate;phase+=std::f64::consts::TAU/rate*(65000.0*(std::f64::consts::TAU*1000.0*t).sin()+6500.0*(std::f64::consts::TAU*19000.0*t).cos());
        [(0.4*phase.cos()) as f32,(0.4*phase.sin()) as f32]
    }).collect();
    let mut job=FeatureJob{id:"s1".into(),token:1,rate,offset:0.0,bandwidth:56000.0,iq,at:"0".into(),source:json!({}),
        stage:0,attempt:0,enqueued:Instant::now(),sampling_started:Instant::now(),wide_bandwidth:Some(180000.0),limits:(-28000.0,28000.0)};
    let mut analyzer=FeatureAnalyzer::default();let (result,_,_,_,bw)=analyzer.observation(&job).unwrap();
    assert_eq!(result.label,"FM stereo pilot");assert_eq!(result.evidence["wide_pilot_check"],true);assert_eq!(bw,180000.0);
    // The same FM outside the measured component is clipped by the wider
    // filter; its pilot must not take over the original candidate.
    let original=job.iq.clone();
    for (i,v) in job.iq.chunks_exact_mut(2).enumerate(){
        let phase=std::f64::consts::TAU*40000.0*i as f64/rate;
        let (sin,cos)=phase.sin_cos();let (re,im)=(v[0] as f64,v[1] as f64);
        v[0]=(re*cos-im*sin) as f32;v[1]=(re*sin+im*cos) as f32;
    }
    let (result,_,_,_,bw)=analyzer.observation(&job).unwrap();assert_ne!(result.evidence["wide_pilot_check"],true);assert_eq!(bw,56000.0);
    job.iq=original;
    job.iq=am_frame(1,0,7680).native_iq().unwrap().samples.to_vec();job.rate=48000.0;job.bandwidth=12000.0;job.limits=(-6000.0,6000.0);
    let (result,_,_,_,bw)=analyzer.observation(&job).unwrap();assert_eq!(result.label,"AM-like");assert_eq!(bw,12000.0);
}

#[test]
fn final_result_from_an_old_lease_cannot_overwrite_a_new_identification(){
    let engine=collector_engine(20.0);let started=engine.data.lock().collector.as_ref().unwrap().started;
    {let mut d=engine.data.lock();d.signals[0].sampling_started=Some(started);d.signals[0].identify_state="analyzing".into();d.collector.as_mut().unwrap().in_flight=true;}
    let iq=am_frame(1,0,7680).native_iq().unwrap().samples.to_vec();
    let job=|stage,at|FeatureJob{id:"s1".into(),token:1,rate:48000.0,offset:0.0,bandwidth:12000.0,iq:iq.clone(),at:"0".into(),source:json!({}),stage,attempt:0,
        enqueued:Instant::now(),sampling_started:at,wide_bandwidth:None,limits:(-6000.0,6000.0)};
    engine.features.send(job(2,started-Duration::from_secs(1))).unwrap();wait_scan(&engine,|d|d.metrics.feature_jobs==1);
    assert_eq!(engine.data.lock().signals[0].identify_state,"analyzing");assert!(engine.data.lock().collector.is_some());
    engine.features.send(job(0,started)).unwrap();wait_scan(&engine,|d|d.signals[0].identify_state=="complete");
    assert_eq!(engine.data.lock().signals[0].evidence["early_exit"],true);
}

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
