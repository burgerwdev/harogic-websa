use super::*;
use receiver_workers::{BasebandConfig, ChannelConfig};
use std::f64::consts::TAU;

fn configs() -> Vec<ChannelConfig> {
    [-20e6, -12e6, -4e6, 0.0, 20_000.0, 40_000.0, 8e6, 20e6]
        .into_iter().enumerate().map(|(slot, offset)| ChannelConfig {
            channel:slot as u32+1, enabled:true, freq:100e6+offset, processing_enabled:false,
            baseband:Some(BasebandConfig {passband_hz:6000.0,transition_hz:1000.0,minimum_rate_hz:12000.0}),
            ..ChannelConfig::default()
        }).collect()
}

fn payload(frames:&[IqFrame], id:u32) -> Vec<[f64;2]> {
    let mut expected_seq=0;
    let mut next_index=None;
    let mut output=Vec::new();
    let mut rate=0.0;
    for frame in frames.iter().filter(|f|u32::from_le_bytes(f[32..36].try_into().unwrap())==id) {
        let seq=u32::from_le_bytes(frame[8..12].try_into().unwrap());
        let count=u32::from_le_bytes(frame[12..16].try_into().unwrap()) as u64;
        let actual=f64::from_le_bytes(frame[16..24].try_into().unwrap());
        let index=u64::from_le_bytes(frame[40..48].try_into().unwrap());
        assert_eq!(seq,expected_seq,"identity {id}");expected_seq+=1;
        if seq==0 {assert_eq!(count,0);rate=actual;} else {
            assert_eq!(actual,rate);
            if let Some(expected)=next_index {assert_eq!(index,expected,"source gap for identity {id}");}
            next_index=Some(index+count*(IQ_FS/rate).round() as u64);
        }
        output.extend(frame[56..].chunks_exact(8).map(|iq|[
            f32::from_le_bytes(iq[..4].try_into().unwrap()) as f64,
            f32::from_le_bytes(iq[4..].try_into().unwrap()) as f64]));
    }
    assert!(rate>=12000.0 && output.len()>1000,"identity {id}: missing actual samples");
    output
}

fn check_tone(iq:&[[f64;2]],rate:f64,tone:f64) -> (f64,f64) {
    let settled=&iq[300..iq.len()-20];
    let rms=(settled.iter().map(|s|s[0]*s[0]+s[1]*s[1]).sum::<f64>()/settled.len() as f64).sqrt();
    let (mut re,mut im)=(0.0,0.0);
    for p in settled.windows(2) {
        re+=p[0][0]*p[1][0]+p[0][1]*p[1][1];
        im+=p[0][0]*p[1][1]-p[0][1]*p[1][0];
    }
    let measured=im.atan2(re)*rate/TAU;
    assert!((measured-tone).abs()<3.0,"expected {tone}, measured {measured}");
    assert!((rms-1800.0/32768.0).abs()<0.002,"rms {rms}");
    (measured,rms)
}

#[test]
fn full_band_four_packet_batches_match_individual_packets_through_retune() {
    let mut configs = configs();
    let mut reference = Ddc::configured_service(100e6, 50e6, &mut None, 16, IQ_FS, &configs).unwrap();
    let mut batched = Ddc::configured_service(100e6, 50e6, &mut None, 16, IQ_FS, &configs).unwrap();
    let mut expected = Vec::new();
    let mut actual = Vec::new();
    let mut seed = 20261105u32;
    for batch in 0..32 {
        if batch == 16 {
            configs[7].freq += 1000.0;
            expected.extend(reference.configure_service_channel(configs[7].clone()).unwrap());
            actual.extend(batched.configure_service_channel(configs[7].clone()).unwrap());
        }
        let raw: Vec<i16> = (0..2 * SDR_BATCH_SAMPLES).map(|_| {
            seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
            (seed >> 8) as i16
        }).collect();
        let first = batch * SDR_BATCH_SAMPLES;
        for (index, packet) in raw.chunks_exact(2 * SDR_PACKET_SAMPLES).enumerate() {
            expected.extend(reference.process_capture_packet(packet,
                1000 + 2 * (first + index * SDR_PACKET_SAMPLES) as u64).unwrap());
        }
        actual.extend(batched.process_capture_packet(&raw, 1000 + 2 * first as u64).unwrap());
    }
    for (ddc, frames) in [(&mut reference, &mut expected), (&mut batched, &mut actual)] {
        ddc.stream.flush_pending().unwrap();
        frames.extend(ddc.drain().unwrap());
        ddc.finish_framers(frames).unwrap();
        // Separate plans allocate separate epochs; every other wire byte,
        // including source position, stream sequence and IQ bits, must match.
        for frame in frames.iter_mut() { frame[36..40].fill(0); }
        frames.sort_by_key(|frame| (u32::from_le_bytes(frame[32..36].try_into().unwrap()),
            u32::from_le_bytes(frame[8..12].try_into().unwrap())));
    }
    assert_eq!(reference.input_pos, 32 * SDR_BATCH_SAMPLES as u64);
    assert_eq!(batched.input_pos, reference.input_pos);
    assert!(actual.iter().map(|frame|frame.len()).sum::<usize>() > 10_000);
    assert_eq!(actual, expected);
}

#[test]
fn full_band_eight_known_signals_keep_content_clocks_and_retiring_order() {
    let mut configs=configs();
    let mut live=Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&configs).unwrap();
    let mut reference=Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&configs).unwrap();
    let initial=live.channel_registry();
    assert_eq!(initial.len(),8);
    assert!(initial.iter().all(|row|row.input_rate==62_500_000.0));
    // A periodic raw ADC waveform contains eight distinct known RF signals.
    // Unlike --fake, this enters before the shared coarse channelizer.
    let period=312_500;
    let raw:Vec<i16>=(0..period).flat_map(|i| {
        let (mut re,mut im)=(0.0,0.0);
        for (slot,config) in configs.iter().enumerate() {
            let phase=TAU*(config.freq-100e6+400.0+slot as f64*200.0)*i as f64/IQ_FS;
            re+=1800.0*phase.cos();im+=1800.0*phase.sin();
        }
        [re.round() as i16,im.round() as i16]
    }).collect();
    let mut frames=Vec::new();let mut expected=Vec::new();
    let mut packet=vec![0;32480];
    for n in 0..1024 {
        if n==512 {
            configs[7].freq=configs[1].freq+1000.0;
            service_cost::admit_service(&configs,IQ_FS,16,live.stream.bins().len()+1).unwrap();
            frames.extend(live.configure_service_channel(configs[7].clone()).unwrap());
        }
        for (i,sample) in packet.iter_mut().enumerate() {*sample=raw[(n*32480+i)%raw.len()];}
        frames.extend(live.process_capture_packet(&packet,(n as u64+1)*16240).unwrap());
        expected.extend(reference.process_capture_packet(&packet,(n as u64+1)*16240).unwrap());
    }
    live.stream.flush_pending().unwrap();frames.extend(live.drain().unwrap());live.finish_framers(&mut frames).unwrap();
    reference.stream.flush_pending().unwrap();expected.extend(reference.drain().unwrap());reference.finish_framers(&mut expected).unwrap();
    let mut report=Vec::new();
    for (slot,row) in initial.iter().enumerate() {
        let actual=payload(&frames,row.channel_id);
        let (tone,rms)=check_tone(&actual,row.output_rate,400.0+slot as f64*200.0);
        report.push(json!({"channel":row.channel,"frequency":row.frequency_hz,"actual_rate":row.output_rate,
            "samples":actual.len(),"tone":tone,"rms":rms}));
        if slot<7 {assert_eq!(actual,payload(&expected,reference.channel_registry()[slot].channel_id),"retune changed healthy channel {}",slot+1);}
    }
    let next=live.channel_registry().iter().find(|row|row.channel==8).unwrap().clone();
    assert_ne!(next.channel_id,initial[7].channel_id);
    let after=payload(&frames,next.channel_id);
    check_tone(&after,next.output_rate,-400.0);
    let id=|frame:&IqFrame|u32::from_le_bytes(frame[32..36].try_into().unwrap());
    assert!(frames.iter().rposition(|f|id(f)==initial[7].channel_id).unwrap()<frames.iter().position(|f|id(f)==next.channel_id).unwrap());
    println!("{}",json!({"input_rate":IQ_FS,"usable_bandwidth":50e6,"taps":16,"channels":report,
        "retuned_channel":8,"retuned_tone":-400,"healthy_channels_bit_exact":7,"ordered_tails":true}));
}

#[test]
fn sparse_service_configs_use_numbers_and_reject_duplicates() {
    let configs:Vec<_>=configs().into_iter().filter(|c|[1,3,8].contains(&c.channel)).collect();
    let mut live=Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&configs).unwrap();
    let mut last=configs[2].clone();last.enabled=false;
    live.configure_service_channel(last.clone()).unwrap();
    assert_eq!(live.channel_registry().iter().map(|r|r.channel).collect::<Vec<_>>(),vec![1,3]);
    last.enabled=true;live.configure_service_channel(last).unwrap();
    assert_eq!(live.channel_registry().iter().map(|r|r.channel).collect::<Vec<_>>(),vec![1,3,8]);
    let mut duplicate=configs.clone();duplicate.push(configs[0].clone());
    assert!(Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&duplicate).is_err());
    let mut invalid=configs[0].clone();invalid.channel=9;
    assert!(invalid.validate().is_err());
}

#[test]
fn original_capture_choices_keep_eight_legal_definitions_and_reject_transition_edges() {
    for power in 0..=11 {
        let display=1<<power;
        let span=50e6/display as f64;
        let mut configs:Vec<_>=(1..=8).map(|channel|ChannelConfig {
            channel,enabled:true,processing_enabled:false,
            freq:100e6+(channel as f64-4.5)*(span/10.0).min(8000.0),
            baseband:Some(BasebandConfig {passband_hz:500.0,transition_hz:500.0,minimum_rate_hz:2000.0}),
            ..ChannelConfig::default()
        }).collect();
        let decimate=service_ddc::service_capture_decimate(100e6,display,16,&configs).unwrap();
        let fs=IQ_FS/decimate as f64;
        let bandwidth=fs*0.8;
        for config in &configs {
            let source=service_ddc::source_plan(fs,config).unwrap();
            service_ddc::validate_source_frequency(100e6,bandwidth,fs,16,config.freq,&source).unwrap();
        }
        let live=Ddc::configured_service(100e6,bandwidth,&mut None,16,fs,&configs).unwrap();
        assert_eq!(live.channel_registry().len(),8);
        let source=service_ddc::source_plan(fs,&configs[7]).unwrap();
        configs[7].freq=100e6+bandwidth/2.0-source.plan().occupied_bandwidth_hz/2.0;
        assert!(service_ddc::validate_source_frequency(100e6,bandwidth,fs,16,configs[7].freq,&source).is_err(),
            "display {display}: transition outside capture was admitted");
        println!("{}",json!({"display_decimate":display,"display_bandwidth":span,"capture_bandwidth":bandwidth,
            "actual_input_rate":fs,"receivers":8,"transition_rejected":true}));
    }
}
