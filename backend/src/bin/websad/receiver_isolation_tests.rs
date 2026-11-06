use super::*;
use websa_channelizer::execution_probe::{self,Probe};

fn until(mut check:impl FnMut()->bool) {
    let began=Instant::now();
    while !check() {assert!(began.elapsed()<Duration::from_secs(3),"bounded test deadline");thread::sleep(Duration::from_millis(1));}
}
fn setup(channels:&[u32])->(ReceiverWorkers,Vec<ChannelConfig>,Vec<ChannelRegistryEntry>) {
    let configs:Vec<_>=channels.iter().map(|&channel|ChannelConfig {channel,enabled:true,freq:100e6+channel as f64*1000.0,
        squelch:-200.0,..ChannelConfig::default()}).collect();
    let registry:Vec<_>=configs.iter().map(|config|ChannelRegistryEntry {channel:config.channel,channel_id:100+config.channel,
        epoch:1,frequency_hz:config.freq,output_rate:48000.0,input_rate:48000.0,group_delay_samples:0.0,state:"active"}).collect();
    let mut workers=ReceiverWorkers::new().unwrap();workers.configure(&configs).unwrap();
    for &channel in channels {workers.bind_identity(channel,&format!("uid-{channel}"),"test-service");send(&mut workers,&registry,channel,0);}
    workers.finish().unwrap();(workers,configs,registry)
}
fn send(workers:&mut ReceiverWorkers,registry:&[ChannelRegistryEntry],channel:u32,sequence:u32) {
    let row=registry.iter().find(|row|row.channel==channel).unwrap();
    let samples=if sequence==0 {Vec::new()} else {(0..4096).flat_map(|index|[0.3+0.1*(index as f32*0.17).sin(),0.0]).collect()};
    let frame=websa_channelizer::encode_baseband(websa_channelizer::BasebandMagic::Primary,sequence,row.output_rate,row.frequency_hz,
        row.channel_id as u64,row.epoch as u64,sequence.saturating_sub(1) as u64*4096,0.0,&samples).unwrap();
    workers.submit(&frame,registry).unwrap();
}
fn payload_count(frames:&ReceiverFrames,id:u32)->usize {
    frames.audio.iter().filter(|frame|frame.len()>96 && u32::from_le_bytes(frame[32..36].try_into().unwrap())==id).count()
}
struct RecordingProbe {manager:super::super::recording::Manager,root:std::path::PathBuf}
impl RecordingProbe {
    fn new()->Self {
        let root=std::env::temp_dir().join(format!("websa-isolation-recording-{}-{}",std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut manager=super::super::recording::Manager::open(root.clone()).unwrap();
        manager.start_kind("uid-1","test-service","slow-neighbour-recording","audio").unwrap().wait().unwrap();
        Self {manager,root}
    }
    fn observe(&self,frames:&ReceiverFrames) {
        for (frame,config) in frames.audio.iter().zip(&frames.audio_configs) {
            if config.channel!=1 {continue;}
            let source=serde_json::to_vec(&serde_json::json!({"receiver_uid":"uid-1","service_id":"test-service",
                "baseband_stream":{"channel_id":"101","epoch":"1"},"config":**config})).unwrap().into();
            self.manager.admit_audio("uid-1",super::super::recording::audio::Input {bytes:frame.clone().into(),source});
        }
    }
    fn check(&mut self) {
        for task in self.manager.snapshots() {self.manager.stop(task["task_id"].as_str().unwrap()).unwrap();}
        self.manager.shutdown();let tasks=self.manager.snapshots();let task=&tasks[0];
        assert!(task["fault"].is_null(),"healthy recording failed: {task}");
        let mut bytes=0;
        super::super::recording::audio::visit(&self.root.join(task["task_id"].as_str().unwrap()),task,&AtomicBool::new(false),
            |_,_,pcm,_,_|{bytes+=pcm.len();Ok(())}).unwrap();
        assert!(bytes>8000*4,"independent archive did not retain healthy PCM");
    }
}
impl Drop for RecordingProbe {
    fn drop(&mut self) {self.manager.shutdown();let _=std::fs::remove_dir_all(&self.root);}
}

#[test]
fn isolation_inside_synchronous_call_and_lifecycle_does_not_wait_for_pool() {
    for busy in [false,true] {
        let (mut workers,mut configs,mut registry)=setup(&[1,3,5,7]);
        let mut recording=RecordingProbe::new();
        let (entered,started)=mpsc::sync_channel(1);let release=Arc::new(AtomicBool::new(false));
        execution_probe::register("receiver",100003000,if busy {Probe::Busy {entered,duration:Duration::from_millis(500)}}
            else {Probe::Wait {entered,release:release.clone(),limit:Duration::from_secs(3)}});
        send(&mut workers,&registry,3,1);started.recv_timeout(Duration::from_secs(1)).unwrap();
        let began=Instant::now();let mut healthy=ReceiverFrames::default();
        for sequence in 1..=3 {
            for channel in [1,5,7] {send(&mut workers,&registry,channel,sequence);}
            until(||{let output=workers.poll().unwrap();recording.observe(&output);healthy.append(output);
                [101,105,107].iter().all(|&id|payload_count(&healthy,id)>=sequence as usize)});
        }
        assert!(began.elapsed()<Duration::from_millis(450),"old same-worker neighbours waited for C3: {:?}",began.elapsed());
        let healthy_seconds=began.elapsed().as_secs_f64();
        if !busy {
            let control=Instant::now();workers.retire_channels(&[3]).unwrap();
            configs[1].enabled=false;configs[0].volume=0.3;workers.configure(&configs).unwrap();
            assert!(control.elapsed()<Duration::from_millis(100));
            assert!(workers.restart_channel(3).unwrap_err().contains("still retiring"));
            assert_eq!(workers.stats()["owner_threads"],4,"no replacement thread while native call is active");
            send(&mut workers,&registry,1,4);
            until(||{let output=workers.poll().unwrap();recording.observe(&output);healthy.append(output);payload_count(&healthy,101)>=4});
            release.store(true,Ordering::Release);
            until(||workers.threads[2].as_ref().unwrap().is_finished());
            workers.restart_channel(3).unwrap();configs[1].enabled=true;workers.configure(&configs).unwrap();
            registry[1].epoch=2;registry[1].channel_id=203;send(&mut workers,&registry,3,0);send(&mut workers,&registry,3,1);
            until(||{healthy.append(workers.poll().unwrap());payload_count(&healthy,203)>0});
        } else {until(||workers.shared.pending.load(Ordering::Acquire)==0);healthy.append(workers.poll().unwrap());}
        for channel in [1,5,7] {
            let audio:Vec<_>=healthy.audio.iter().filter(|frame|u32::from_le_bytes(frame[32..36].try_into().unwrap())==100+channel).collect();
            let mut next=0_u64;
            for (index,frame) in audio.iter().enumerate() {
                assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()) as usize,index);
                assert_eq!(u64::from_le_bytes(frame[64..72].try_into().unwrap()),next);
                next+=u32::from_le_bytes(frame[12..16].try_into().unwrap()) as u64;
            }
            assert!(next>8000);assert!(workers.channel_failure(channel).is_none());
        }
        recording.check();
        println!("inside_call_busy={busy} healthy_three_lanes_seconds={:.6}",healthy_seconds);
    }
}

#[test]
fn isolation_inference_error_and_panic_fail_only_original_uid_and_recover() {
    for panic in [false,true] {
        let (mut workers,_,mut registry)=setup(&[1,3]);
        execution_probe::register("receiver",100003000,if panic {Probe::Panic} else {Probe::Error("controlled inference failure".into())});
        send(&mut workers,&registry,3,1);send(&mut workers,&registry,1,1);
        let mut output=ReceiverFrames::default();
        until(||{output.append(workers.poll().unwrap());workers.channel_failure(3).is_some()&&payload_count(&output,101)>0});
        assert!(workers.shared.check().is_ok());assert!(workers.channel_failure(1).is_none());
        assert_eq!(output.faults.len(),1);assert_eq!(output.faults[0].receiver_uid.as_deref(),Some("uid-3"));
        until(||workers.threads[2].as_ref().unwrap().is_finished());workers.restart_channel(3).unwrap();
        registry[1].epoch=2;registry[1].channel_id=203;send(&mut workers,&registry,3,0);send(&mut workers,&registry,3,1);
        until(||{output.append(workers.poll().unwrap());payload_count(&output,203)>0});
        assert!(workers.channel_failure(3).is_none());
        let stats=workers.channel_stats(3);
        assert_eq!(stats[0]["packets"]["failed"],1);assert_eq!(stats[0]["packets"]["successful_samples"],0);
    }
}

#[test]
fn isolation_output_full_and_input_full_leave_healthy_lane_running() {
    let (mut workers,_,registry)=setup(&[1,3]);
    for sequence in 1..=RESULT_CAPACITY as u32+1 {
        send(&mut workers,&registry,3,sequence);
        until(||workers.shared.pending.load(Ordering::Acquire)==0);
    }
    assert!(workers.channel_failure(3).unwrap().error.contains("output queue full"));
    send(&mut workers,&registry,1,1);until(||workers.shared.pending.load(Ordering::Acquire)==0);
    let output=workers.poll().unwrap();assert!(payload_count(&output,101)>0);assert!(workers.channel_failure(1).is_none());
    assert!(workers.shared.branches.iter().all(|branch|branch.output_pending.load(Ordering::Acquire)==0));

    let (mut workers,_,registry)=setup(&[1,3]);
    let (entered,started)=mpsc::sync_channel(1);let release=Arc::new(AtomicBool::new(false));
    execution_probe::register("receiver",100003000,Probe::Wait {entered,release:release.clone(),limit:Duration::from_secs(3)});
    send(&mut workers,&registry,3,1);started.recv_timeout(Duration::from_secs(1)).unwrap();
    for sequence in 2..=INPUT_CAPACITY as u32+2 {send(&mut workers,&registry,3,sequence);}
    assert!(workers.channel_failure(3).unwrap().error.contains("input queue failure"));
    send(&mut workers,&registry,1,1);let mut output=ReceiverFrames::default();
    until(||{output.append(workers.poll().unwrap());payload_count(&output,101)>0});
    assert!(workers.channel_failure(1).is_none());release.store(true,Ordering::Release);
    until(||workers.shared.pending.load(Ordering::Acquire)==0);
}

#[test]
fn isolation_external_child_exit_keeps_other_receiver_running_and_reaps_child() {
    assert!(websa_channelizer::catalog::mode("aprs").unwrap().available,"APRS dependency required for this host gate");
    let mut workers=ReceiverWorkers::new().unwrap();
    let configs=[ChannelConfig {freq:144800000.0,squelch:-200.0,..ChannelConfig::default()},
        ChannelConfig {channel:3,enabled:true,freq:144803000.0,mode:"aprs".into(),if_bw:20000.0,
            squelch:-200.0,..ChannelConfig::default()}];
    workers.install_prepared(PreparedDecoders::new(&configs).unwrap());workers.configure(&configs).unwrap();
    let registry:Vec<_>=configs.iter().map(|config|ChannelRegistryEntry {channel:config.channel,channel_id:100+config.channel,epoch:1,
        frequency_hz:config.freq,output_rate:if config.channel==3 {250000.0}else {48000.0},
        input_rate:if config.channel==3 {250000.0}else {48000.0},group_delay_samples:0.0,state:"active"}).collect();
    for channel in [1,3] {workers.bind_identity(channel,&format!("external-uid-{channel}"),"external-test");send(&mut workers,&registry,channel,0);}
    workers.finish().unwrap();send(&mut workers,&registry,3,1);workers.finish().unwrap();
    let stats=workers.channel_stats(3);let pid=stats[0]["receiver"]["external"]["pid"].as_u64().expect("owned decoder pid");
    assert!(std::process::Command::new("kill").args(["-TERM",&pid.to_string()]).status().unwrap().success());
    let mut healthy=ReceiverFrames::default();
    for sequence in 2..30 {
        send(&mut workers,&registry,1,sequence-1);send(&mut workers,&registry,3,sequence);
        healthy.append(workers.finish().unwrap());
        if workers.channel_failure(3).is_some() {break;}
        thread::sleep(Duration::from_millis(10));
    }
    assert!(workers.channel_failure(3).is_some());assert!(workers.channel_failure(1).is_none());
    assert!(payload_count(&healthy,101)>0);assert!(workers.shared.check().is_ok());
    until(||workers.threads[2].as_ref().unwrap().is_finished());
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(),"test decoder must be reaped");
}

fn search_job(known:bool)->(Receiver,Ft8Job) {
    let config=ChannelConfig {mode:"ft8".into(),if_bw:3200.0,..ChannelConfig::default()};
    let rate=if known {48000.0} else {12000.0};let mut receiver=Receiver::new(config.receiver_config(rate)).unwrap();
    let mut iq=if known {super::super::service_fixture::fixture("ft8/ft8_cq_iq.bin").unwrap().to_vec()} else {Vec::new()};
    iq.resize((rate*16.0) as usize*2,0.0);
    for (index,block) in iq.chunks(16384).enumerate() {
        if let Some(job)=receiver.process(block,InputClock {first_sample_index:(index*8192) as u64,input_rate:rate,delay_input_samples:0.0},None).unwrap().ft8_job {return (receiver,job);}
    }
    panic!("complete search window missing")
}

#[test]
fn isolation_search_inside_call_and_full_return_mailbox_are_per_uid() {
    let (workers,_,_)=setup(&[]);let shared=workers.shared.clone();
    let mut destinations=Vec::new();let mut results=Vec::new();
    for _ in 0..MAX_CHANNELS {let (tx,rx)=mpsc::sync_channel(SEARCH_CAPACITY);destinations.push(tx);results.push(rx);}
    let (_,slow)=search_job(false);let (mut fast_receiver,fast)=search_job(true);
    let (entered,started)=mpsc::sync_channel(1);let release=Arc::new(AtomicBool::new(false));
    execution_probe::register("search",slow.execution_identity(),Probe::Wait {entered,release:release.clone(),limit:Duration::from_secs(30)});
    let mut threads=Vec::new();let mut inputs=Vec::new();
    for slot in [0,2] {
        let (tx,rx)=mpsc::sync_channel(SEARCH_CAPACITY);inputs.push(tx);
        let shared=shared.clone();let destinations=destinations.clone();
        threads.push(thread::spawn(move||run_search(rx,destinations,shared,Some(slot))));
    }
    let submit=|tx:&mpsc::SyncSender<SearchJob>,slot,job|tx.send(SearchJob {slot,generation:1,channel_id:slot as u32+101,epoch:1,job,pending:Pending::new(&shared)}).unwrap_or_else(|_|panic!("search input disconnected"));
    submit(&inputs[1],2,slow);started.recv_timeout(Duration::from_secs(1)).unwrap();
    submit(&inputs[0],0,fast);
    let PendingWork {work,pending}=results[0].recv_timeout(Duration::from_secs(20)).expect("healthy known search was blocked by slow C3");
    drop(pending);
    let Work::Search(result)=work else {panic!("search result missing")};
    assert!(!release.load(Ordering::Acquire));assert_eq!(result.channel_id,101);
    // Keep the real decoded payload; output cannot be replaced by an enqueue ack.
    let decoded=fast_receiver.accept_ft8(result.result);
    assert!(decoded.iter().any(|event|event["type"]=="ft8" && event["text"].as_str().is_some_and(|text|text.contains("CQ"))),"known CQ not decoded: {decoded:?}");
    release.store(true,Ordering::Release);drop(inputs);
    for handle in threads {handle.join().unwrap();}
    assert!(shared.branch_failure(0).is_none());
    while results[2].try_recv().is_ok() {}

    // A full C3 result mailbox neither uses C1 capacity nor latches a pool error.
    for _ in 0..SEARCH_CAPACITY {
        let (reply,_)=mpsc::sync_channel(1);
        destinations[2].try_send(PendingWork {work:Work::Deactivate {slot:2,generation:1,reply},pending:Pending::new(&shared)})
            .unwrap_or_else(|_|panic!("test return mailbox already full"));
    }
    let (tx,rx)=mpsc::sync_channel(SEARCH_CAPACITY);submit(&tx,2,search_job(false).1);drop(tx);
    run_search(rx,destinations,shared.clone(),Some(2));
    assert!(shared.branch_failure(2).unwrap().error.contains("FT8 result queue failure"));assert!(shared.branch_failure(0).is_none());
    drop(results);assert_eq!(shared.pending.load(Ordering::Acquire),0);
}

#[test]
fn isolation_late_search_is_cancelled_before_reused_slot_receives_it() {
    let (mut workers,_,_)=setup(&[]);let shared=workers.shared.clone();
    workers.bind_identity(3,"old-uid","old-service");
    let (_,job)=search_job(false);let (entered,started)=mpsc::sync_channel(1);
    let release=Arc::new(AtomicBool::new(false));
    execution_probe::register("search",job.execution_identity(),Probe::Wait {entered,release:release.clone(),limit:Duration::from_secs(3)});
    let (tx,rx)=mpsc::sync_channel(SEARCH_CAPACITY);let mut destinations=Vec::new();let mut results=Vec::new();
    for _ in 0..MAX_CHANNELS {let (tx,rx)=mpsc::sync_channel(SEARCH_CAPACITY);destinations.push(tx);results.push(rx);}
    tx.send(SearchJob {slot:2,generation:1,channel_id:103,epoch:1,job,pending:Pending::new(&shared)}).unwrap_or_else(|_|panic!("test search input"));drop(tx);
    let thread_shared=shared.clone();let handle=thread::spawn(move||run_search(rx,destinations,thread_shared,Some(2)));
    started.recv_timeout(Duration::from_secs(1)).unwrap();
    shared.generations[2].fetch_add(1,Ordering::AcqRel);workers.bind_identity(3,"new-uid","new-service");
    release.store(true,Ordering::Release);handle.join().unwrap();
    assert!(results[2].try_recv().is_err());assert!(shared.branch_failure(2).is_none());
    assert_eq!(shared.pending.load(Ordering::Acquire),0);
}

#[test]
fn isolation_iq_only_definitions_allocate_no_processing_owner() {
    let mut workers=ReceiverWorkers::new().unwrap();
    let configs:Vec<_>=(1..=MAX_CHANNELS as u32).map(|channel|ChannelConfig {channel,enabled:true,processing_enabled:false,
        ..ChannelConfig::default()}).collect();
    workers.configure(&configs).unwrap();assert_eq!(workers.stats()["owner_threads"],0);
    assert!(workers.threads.iter().all(Option::is_none));
}

#[test]
fn isolation_reused_slot_statistics_and_recording_retention_follow_original_uid() {
    let (mut workers,mut configs,mut registry)=setup(&[1,3]);
    send(&mut workers,&registry,3,1);workers.finish().unwrap();
    execution_probe::register("receiver",100003000,Probe::Error("old UID error".into()));
    send(&mut workers,&registry,3,2);workers.finish().unwrap();
    assert_eq!(workers.channel_failure(3).unwrap().receiver_uid.as_deref(),Some("uid-3"));
    until(||workers.threads[2].as_ref().unwrap().is_finished());
    workers.retire_channels(&[3]).unwrap();configs[1].enabled=false;workers.configure(&configs).unwrap();
    until(||workers.threads[2].as_ref().unwrap().is_finished());workers.finish().unwrap();
    workers.bind_identity(3,"new-uid","test-service");
    configs[1].enabled=true;workers.configure(&configs).unwrap();registry[1].epoch=2;registry[1].channel_id=203;
    send(&mut workers,&registry,3,0);send(&mut workers,&registry,3,1);
    until(||workers.shared.pending.load(Ordering::Acquire)==0);
    assert!(!workers.retiring_uid(3,"uid-3"),"new output must not keep the deleted UID's recordings alive");
    assert!(workers.retiring_uid(3,"new-uid"));
    let stats=workers.uid_stats(3,"new-uid");assert_eq!(stats.as_array().unwrap().len(),1);
    assert_eq!(stats[0]["packets"]["accepted"],2);assert_eq!(stats[0]["packets"]["successful_samples"],4096);
    workers.finish().unwrap();assert!(!workers.retiring_uid(3,"new-uid"));
    execution_probe::register("receiver",100003000,Probe::Error("new UID error".into()));
    send(&mut workers,&registry,3,2);workers.finish().unwrap();
    assert_eq!(workers.channel_failure(3).unwrap().receiver_uid.as_deref(),Some("new-uid"));
}

#[test]
fn iq_transfer_hotspot_measurement() {
    for native in [false,true] {for samples in [4096,65536] {
        let mut workers=ReceiverWorkers::new().unwrap();
        workers.configure(&[ChannelConfig {freq:100001000.0,agc:false,squelch:-200.0,..ChannelConfig::default()}]).unwrap();
        let registry=[ChannelRegistryEntry {channel:1,channel_id:1,epoch:1,frequency_hz:100001000.0,
            output_rate:48000.0,input_rate:48000.0,group_delay_samples:0.0,state:"active"}];
        let mut framer=websa_channelizer::BasebandFramer::new(1,1,100001000.0,48000.0).unwrap();
        let iq:Vec<_>=(0..samples).flat_map(|index|[(0.3+0.1*(index as f32*0.17).sin())*32768.0,0.0]).collect();
        let (mut framing,mut submission,mut bytes,mut audio_samples)=(0_u128,0_u128,0_usize,0_u64);
        for sequence in 0..36 {
            let began=Instant::now();
            let fine=websa_channelizer::FineOutput {channel_id:1,epoch:1,bin:0,seq:sequence,
                stamp:sequence*samples as u64,input_index:sequence*samples as u64,output_index:sequence*samples as u64,
                output_rate:48000.0,center_offset_hz:None,delay_samples:0.0,iq:&iq};
            let frames=if native {framer.push_native(&fine).unwrap()} else {framer.push(&fine).unwrap().into_iter().map(websa_channelizer::IqFrame::from).collect()};
            if sequence>=4 {framing+=began.elapsed().as_nanos();}
            for frame in frames {
                let began=Instant::now();workers.submit(&frame,&registry).unwrap();
                if sequence>=4 {submission+=began.elapsed().as_nanos();bytes+=frame.iq_header().len()+frame.native_iq().map_or(0,|native|native.samples.bytes());}
                if native {assert!(!frame.wire_ready(),"native receiver submission encoded wire");}
            }
            let output=workers.finish().unwrap();
            audio_samples+=output.audio.iter().map(|frame|u32::from_le_bytes(frame[12..16].try_into().unwrap()) as u64).sum::<u64>();
        }
        assert!(audio_samples>samples as u64*30);
        println!("iq_transfer_path={} samples_per_block={samples} measured_blocks=32 wire_equivalent_bytes={bytes} framing_ns={framing} submit_decode_copy_ns={submission} framing_ns_per_sample={} submit_ns_per_sample={}",
            if native {"native"} else {"wire"},framing as f64/(samples*32) as f64,submission as f64/(samples*32) as f64);
    }}
}

#[test]
fn isolation_real_ddc_control_does_not_drain_unrelated_slow_owner() {
    use super::super::{DevState,Ddc,SendDdc,IQ_FS};
    let mut state=DevState::new(serde_json::json!({"preset_defaults":{}}),serde_json::json!({}));
    state.swp.mode="sdr".into();state.sdr_center=100e6;state.sdr_listen=100001000.0;
    state.sdr_demod="am".into();state.sdr_if_bw=6000.0;state.sdr_squelch=-200.0;
    state.sdr_channels[2]=ChannelConfig {channel:3,enabled:true,freq:100003000.0,squelch:-200.0,..ChannelConfig::default()};
    for config in &mut state.sdr_channels {config.baseband=Some(BasebandConfig {passband_hz:16000.0,transition_hz:1000.0,minimum_rate_hz:48000.0});}
    state.hardware_shim=Some("numerical-control-test".into());
    state.ddc=Some(SendDdc(Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&state.channel_configs()).unwrap()));
    let registry=state.stream_registry();
    let frame=|channel,sequence| {
        let row=registry.iter().find(|row|row.channel==channel).unwrap();let step=(row.input_rate/row.output_rate).round() as u64;
        let iq=if sequence==0 {Vec::new()} else {vec![0.3,0.0].repeat(4096)};
        websa_channelizer::encode_baseband(websa_channelizer::BasebandMagic::Primary,sequence,row.output_rate,row.frequency_hz,
            row.channel_id as u64,row.epoch as u64,sequence.saturating_sub(1) as u64*4096*step,0.0,&iq).unwrap()
    };
    state.process_receiver_frames_with_registry(&[frame(1,0),frame(3,0)],&registry,true).unwrap();
    let (entered,started)=mpsc::sync_channel(1);let release=Arc::new(AtomicBool::new(false));
    execution_probe::register("receiver",100003000,Probe::Wait {entered,release:release.clone(),limit:Duration::from_secs(3)});
    state.submit_receiver_frames(&[frame(3,1)]).unwrap();started.recv_timeout(Duration::from_secs(1)).unwrap();
    let control=Instant::now();state.apply_channel_config(&serde_json::json!({"channel":3,"enabled":false})).unwrap();
    state.apply_channel_config(&serde_json::json!({"channel":1,"volume":0.4})).unwrap();
    assert!(control.elapsed()<Duration::from_millis(200),"real DDC control waited for inference: {:?}",control.elapsed());
    let mut recording=RecordingProbe::new();let mut output=ReceiverFrames::default();
    for sequence in 1..=3 {
        state.submit_receiver_frames(&[frame(1,sequence)]).unwrap();
        until(||{let frames=state.drain_receiver_frames().unwrap();recording.observe(&frames);output.append(frames);
            output.audio.iter().zip(&output.audio_configs).filter(|(frame,config)|config.channel==1&&frame.len()>96).count()>=sequence as usize});
    }
    assert!(state.receiver_failure.is_none());assert!(!release.load(Ordering::Acquire));
    recording.check();release.store(true,Ordering::Release);
    until(||state.receiver_workers.as_ref().unwrap().threads[2].as_ref().unwrap().is_finished());
}

#[test]
fn native_iq_executing_reference_stays_accounted_and_wire_is_not_created() {
    use websa_channelizer::{IqFrame,BasebandMagic};
    let (mut workers,_,registry)=setup(&[1,3]);
    let frame=IqFrame::native(BasebandMagic::Aux,1,48000.0,100003000.0,103,1,0,0.0,48000.0,
        vec![0.3,0.0].repeat(4096)).unwrap();
    let weak=Arc::downgrade(&frame.native_iq().unwrap().samples);
    let (entered,started)=mpsc::sync_channel(1);let release=Arc::new(AtomicBool::new(false));
    execution_probe::register("receiver",100003000,Probe::Wait {entered,release:release.clone(),limit:Duration::from_secs(3)});
    workers.submit(&frame,&registry).unwrap();started.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(!frame.wire_ready());drop(frame);
    assert!(weak.upgrade().is_some(),"dequeue cannot release the running IQ allocation");
    let rows=workers.uid_stats(3,"uid-3");assert_eq!(rows[0]["occupancy"]["executing"]["bytes"],4096*8);
    for sequence in 1..=2 {send(&mut workers,&registry,1,sequence);}
    until(||payload_count(&workers.poll().unwrap(),101)>0);
    release.store(true,Ordering::Release);workers.finish().unwrap();
    assert!(weak.upgrade().is_none());assert_eq!(workers.uid_stats(3,"uid-3")[0]["held_reference_bytes"],0);
}

#[test]
fn resource_selected_model_prepares_on_owner_and_health_continues() {
    for (algorithm,kind,key) in [("dfn","prepare-dfn",0),("gtcrn","prepare-onnx",0),
        ("ul-unas","prepare-onnx",1),("fastenhancer","prepare-onnx",2)] {
        let (mut workers,mut configs,mut registry)=setup(&[1]);
        configs.push(ChannelConfig {channel:3,enabled:true,freq:100003000.0,nr:true,nr_algorithm:algorithm.into(),
            squelch:-200.0,..ChannelConfig::default()});
        registry.push(ChannelRegistryEntry {channel:3,channel_id:103,epoch:1,frequency_hz:100003000.0,
            output_rate:48000.0,input_rate:48000.0,group_delay_samples:0.0,state:"active"});
        workers.bind_identity(3,"cold-uid-3","cold-service");workers.configure(&configs).unwrap();
        let (entered,started)=mpsc::sync_channel(1);let release=Arc::new(AtomicBool::new(false));
        execution_probe::register(kind,key,Probe::Wait {entered,release:release.clone(),limit:Duration::from_secs(3)});
        let began=Instant::now();send(&mut workers,&registry,3,0);started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(workers.channel_preparing(3));assert_eq!(workers.stats()["owner_threads"],2);
        let mut output=ReceiverFrames::default();
        for sequence in 1..=3 {
            send(&mut workers,&registry,1,sequence);
            until(||{output.append(workers.poll().unwrap());payload_count(&output,101)>=sequence as usize});
        }
        assert!(!release.load(Ordering::Acquire));assert!(workers.channel_failure(1).is_none());
        release.store(true,Ordering::Release);
        until(||!workers.channel_preparing(3));
        assert!(workers.channel_failure(3).is_none(),"selected model failed: {:?}",workers.channel_failure(3));
        for sequence in 1..=3 {send(&mut workers,&registry,3,sequence);output.append(workers.finish().unwrap());}
        assert!(payload_count(&output,103)>0);
        let rows=workers.uid_stats(3,"cold-uid-3");let model=&rows[0]["receiver"];
        assert_eq!(model["nr_requested"],algorithm);assert_eq!(model["nr_effective"],algorithm);
        assert_eq!(model["nr_state"],"ready");
        assert_eq!(model["model_sessions"]["dfn_templates"],usize::from(algorithm=="dfn"));
        assert_eq!(model["model_sessions"]["onnx_sessions"],usize::from(algorithm!="dfn"));
        println!("first_selected_model={algorithm} elapsed_seconds={} resources={} preparations={}",
            began.elapsed().as_secs_f64(),websa_channelizer::host_resources::snapshot(),websa_channelizer::model_preparation::snapshot());
    }
}

#[test]
fn resource_model_preparation_failure_never_substitutes_wiener_and_can_retry() {
    let (mut workers,mut configs,mut registry)=setup(&[1]);
    configs.push(ChannelConfig {channel:3,enabled:true,freq:100003000.0,nr:true,nr_algorithm:"fastenhancer".into(),
        squelch:-200.0,..ChannelConfig::default()});
    registry.push(ChannelRegistryEntry {channel:3,channel_id:103,epoch:1,frequency_hz:100003000.0,
        output_rate:48000.0,input_rate:48000.0,group_delay_samples:0.0,state:"active"});
    workers.bind_identity(3,"failed-model-uid","model-service");workers.configure(&configs).unwrap();
    execution_probe::register("prepare-onnx",2,Probe::Error("controlled model initialization failure".into()));
    send(&mut workers,&registry,3,0);send(&mut workers,&registry,1,1);let mut output=ReceiverFrames::default();
    until(||{output.append(workers.poll().unwrap());workers.channel_failure(3).is_some()&&payload_count(&output,101)>0});
    assert!(workers.channel_failure(3).unwrap().error.contains("selected fastenhancer model preparation failed"));
    assert_eq!(payload_count(&output,103),0);assert!(workers.channel_failure(1).is_none());
    assert_eq!(workers.configs[2].nr_algorithm,"fastenhancer");
    until(||workers.threads[2].as_ref().unwrap().is_finished());workers.restart_channel(3).unwrap();
    registry[1].epoch=2;registry[1].channel_id=203;send(&mut workers,&registry,3,0);workers.finish().unwrap();
    send(&mut workers,&registry,3,1);let output=workers.finish().unwrap();assert!(payload_count(&output,203)>0);
    assert!(workers.channel_failure(3).is_none());assert_eq!(workers.uid_stats(3,"failed-model-uid")[1]["receiver"]["nr_effective"],"fastenhancer");
}

#[test]
fn resource_all_eight_owners_run_without_a_cpu_count_cap() {
    let (mut workers,_,registry)=setup(&[1,2,3,4,5,6,7,8]);let mut output=ReceiverFrames::default();
    for sequence in 1..=3 {
        for channel in 1..=8 {send(&mut workers,&registry,channel,sequence);}
        output.append(workers.finish().unwrap());
    }
    for channel in 1..=8 {assert!(payload_count(&output,100+channel)>=3);assert!(workers.channel_failure(channel).is_none());}
    assert_eq!(workers.stats()["owner_threads"],8);
    println!("eight_owners_resources={}",websa_channelizer::host_resources::snapshot());
}

#[test]
fn resource_first_model_retains_paced_input_and_gain_agc_do_not_prepare_again() {
    let (mut workers,mut configs,mut registry)=setup(&[1]);
    configs.push(ChannelConfig {channel:3,enabled:true,freq:100003000.0,nr:true,nr_algorithm:"dfn".into(),
        squelch:-200.0,..ChannelConfig::default()});
    registry.push(ChannelRegistryEntry {channel:3,channel_id:103,epoch:1,frequency_hz:100003000.0,
        output_rate:48000.0,input_rate:48000.0,group_delay_samples:0.0,state:"active"});
    workers.bind_identity(3,"paced-cold-uid","paced-service");workers.configure(&configs).unwrap();
    send(&mut workers,&registry,3,0);let mut output=ReceiverFrames::default();
    for sequence in 1..=15 {
        for channel in [1,3] {send(&mut workers,&registry,channel,sequence);}
        thread::sleep(Duration::from_millis(86));output.append(workers.poll().unwrap());
    }
    output.append(workers.finish().unwrap());
    assert!(workers.channel_failure(3).is_none(),"cold preparation lost real input: {:?}",workers.channel_failure(3));
    for channel in [1,3] {assert!(payload_count(&output,100+channel)>0);}
    assert_eq!(workers.uid_stats(3,"paced-cold-uid")[0]["packets"]["successful_samples"],15*4096);
    assert_eq!(workers.shared.branches[2].input_bytes.load(Ordering::Acquire),0);
    assert_eq!(workers.shared.branches[2].input_media_ns.load(Ordering::Acquire),0);
    let before=websa_channelizer::model_preparation::snapshot();
    configs[1].volume=0.3;configs[1].agc=false;workers.configure(&configs).unwrap();
    send(&mut workers,&registry,3,16);workers.finish().unwrap();
    assert_eq!(websa_channelizer::model_preparation::snapshot(),before,"ordinary controls must not run model preparation");
}


#[test]
fn control_receipts_wait_for_data_and_flood_does_not_starve_healthy_audio() {
    let (mut workers,mut configs,registry)=setup(&[1,8]);
    configs[1].volume=0.25;workers.configure(&configs).unwrap();
    workers.queue_control(8,42,"gain-eight",false,false,None,None).unwrap();
    assert_eq!(workers.control_receipts(8,"uid-8")[0]["status"],"queued");
    send(&mut workers,&registry,1,1);send(&mut workers,&registry,8,1);
    let output=workers.finish().unwrap();assert!(payload_count(&output,101)>0);
    let receipt=workers.control_receipts(8,"uid-8");let receipt=&receipt[0];
    assert_eq!(receipt["status"],"applied");assert_eq!(receipt["revision"],"42");
    assert_eq!(receipt["receiver_uid"],"uid-8");assert_eq!(receipt["service_id"],"test-service");
    assert_eq!(receipt["boundary"]["input_index"],"0");assert_eq!(receipt["boundary"]["precision"],"block");
    // Actions cannot coalesce. Each data step executes at most 64; a full
    // control mailbox explicitly rejects without touching input or other owners.
    for index in 0..64 {workers.queue_control(8,43,&format!("action-{index}"),true,false,Some(&format!("action-{index}")),Some(Duration::from_secs(5))).unwrap();}
    assert!(workers.queue_control(8,43,"overflow",true,false,Some("overflow"),None).is_err());
    send(&mut workers,&registry,1,2);send(&mut workers,&registry,8,2);
    let output=workers.finish().unwrap();assert!(payload_count(&output,101)>0);
    assert!(workers.channel_failure(1).is_none());assert!(workers.channel_failure(8).is_none());
    assert_eq!(workers.control_receipts(8,"uid-8").as_array().unwrap().iter().filter(|r|r["status"]=="queued").count(),0);
    workers.bind_identity(8,"new-uid","new-service");
    assert!(workers.control_receipts(8,"new-uid").as_array().unwrap().is_empty());
    assert!(!workers.control_receipts(8,"uid-8").as_array().unwrap().iter().any(|r|r["status"]=="queued"));
}
