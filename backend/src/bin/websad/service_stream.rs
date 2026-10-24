use super::*;

#[derive(Default)]
pub(super) struct DecodeSnapshots { channels:BTreeMap<u32,DecodeSnapshot> }
struct DecodeSnapshot { envelope:Value, serial:String, width:u32, height:u32, lines:u32, rgb:Vec<u8>, meta:Value }
#[derive(serde::Deserialize)]
struct SnapshotHeader<'a> { #[serde(borrow)] mode:&'a str, #[serde(borrow)] event:&'a serde_json::value::RawValue }
#[derive(serde::Deserialize)]
struct SnapshotImage { width:u32,height:u32,row:u32,rows:u32,rgb:Vec<u8>,meta:Value }
#[derive(serde::Deserialize)]
struct EventKind<'a> { #[serde(rename="type",borrow)] kind:&'a str }
impl DecodeSnapshots {
    pub(super) fn clear_channel(&mut self,channel:u32) {self.channels.remove(&channel);}
    pub(super) fn observe(&mut self,frames:&[Vec<u8>])->Result<(),String> {
        for frame in frames {
            let header:SnapshotHeader<'_>=serde_json::from_slice(&frame[8..]).map_err(|e|e.to_string())?;
            if header.mode!="sstv" {continue;}
            let event:EventKind<'_>=serde_json::from_str(header.event.get()).map_err(|e|e.to_string())?;
            let event_offset=header.event.get().as_ptr() as usize-frame.as_ptr() as usize;
            let mut metadata=Vec::with_capacity(frame.len()-header.event.get().len()+4);
            metadata.extend_from_slice(&frame[8..event_offset]);metadata.extend_from_slice(b"null");
            metadata.extend_from_slice(&frame[event_offset+header.event.get().len()..]);
            let envelope:Value=serde_json::from_slice(&metadata).map_err(|e|e.to_string())?;
            let channel=envelope["channel"].as_u64().ok_or("decode snapshot channel absent")? as u32;
            if !(1..=3).contains(&channel) {return Err("decode snapshot channel invalid".into());}
            let entry=self.channels.entry(channel).or_insert_with(||DecodeSnapshot {
                envelope:Value::Null,serial:String::new(),width:0,height:0,lines:0,rgb:Vec::new(),meta:Value::Null });
            if event.kind=="start" {entry.rgb.clear();entry.lines=0;entry.serial.clear();}
            if event.kind=="sstv-image" {
                let image:SnapshotImage=serde_json::from_str(header.event.get()).map_err(|e|e.to_string())?;
                let (width,height,row,rows)=(image.width,image.height,image.row,image.rows);
                let size=(width as usize).checked_mul(height as usize).and_then(|v|v.checked_mul(3))
                    .ok_or("SSTV snapshot geometry overflow")?;
                if width==0 || height==0 || size>2*1024*1024 || row.checked_add(rows).is_none_or(|end|end>height) {
                    return Err("SSTV snapshot geometry exceeds bound".into());
                }
                let serial=image.meta["serial"].as_str().ok_or("SSTV snapshot serial absent")?;
                if entry.serial!=serial || entry.width!=width || entry.height!=height {
                    entry.serial=serial.to_owned();entry.width=width;entry.height=height;entry.lines=0;
                    entry.rgb.clear();entry.rgb.resize(size,0);
                }
                if row!=entry.lines {return Err("SSTV snapshot row discontinuity".into());}
                if image.rgb.len()!=width as usize*rows as usize*3 {return Err("SSTV snapshot RGB length mismatch".into());}
                let begin=row as usize*width as usize*3;
                entry.rgb[begin..begin+image.rgb.len()].copy_from_slice(&image.rgb);
                entry.lines=row+rows;entry.meta=image.meta;
            }
            entry.envelope=envelope;
        }
        Ok(())
    }
    pub(super) fn initial(&self,channel:Option<u32>,registry:&[sdr_pipeline::ChannelRegistryEntry],
        subscriber:&mut subscriber_stream::SubscriberStream)->Result<Vec<Message>,String> {
        let mut frames=Vec::new();
        for (&slot,snapshot) in &self.channels {
            if channel.is_some_and(|c|c!=slot) {continue;}
            let id=snapshot.envelope["channel_id"].as_u64().unwrap_or(0) as u32;
            let epoch=snapshot.envelope["epoch"].as_u64().unwrap_or(0) as u32;
            if !registry.iter().any(|r|r.channel==slot&&r.channel_id==id&&r.epoch==epoch) {continue;}
            let source=snapshot.envelope["seq"].as_u64().ok_or("snapshot sequence absent")? as u32;
            let mut envelope=snapshot.envelope.clone();
            let mut sequence=0u32;
            envelope["seq"]=json!(0);envelope["event"]=json!({"type":"start"});
            frames.push(Message::Binary(websa_channelizer::encode_decode_event(&serde_json::to_vec(&envelope).map_err(|e|e.to_string())?).map_err(str::to_owned)?.into()));
            for row in (0..snapshot.lines).step_by(64) {
                let end=(row+64).min(snapshot.lines);
                let mut meta=snapshot.meta.clone();meta["lines"]=json!(end);
                meta["progress"]=json!(end*100/snapshot.height.max(1));
                meta["complete"]=json!(end==snapshot.height&&snapshot.meta["complete"]==true);
                sequence+=1;envelope["seq"]=json!(sequence);
                envelope["event"]=json!({"type":"sstv-image","width":snapshot.width,"height":snapshot.height,
                    "row":row,"rows":end-row,"rgb":&snapshot.rgb[row as usize*snapshot.width as usize*3..end as usize*snapshot.width as usize*3],"meta":meta});
                frames.push(Message::Binary(websa_channelizer::encode_decode_event(&serde_json::to_vec(&envelope).map_err(|e|e.to_string())?).map_err(str::to_owned)?.into()));
            }
            subscriber.seed_decode(slot,(id,epoch),source,sequence);
        }
        Ok(frames)
    }
}

// A missing ordered IQ block ends the session. Do not reuse its sticky worker
// failure or restart capture against the same identity after losing samples.
pub(super) fn fail_receiver_session(state:&mut DevState,app:&App,error:String)->Option<Value> {
    if state.receiver_failure.is_some() {return None;}
    state.receiver_failure=Some(error.clone());
    if state.pipeline.is_some() || state.ddc_error.as_ref()!=Some(&error) {
        state.hardware_presentation_failed(error.clone());
    }
    if let Some(workers)=state.receiver_workers.take() {
        let diagnostics=state.ddc_failure_diagnostics.get_or_insert_with(||json!({"error":&error}));
        diagnostics["receiver_workers"]=workers.stats();
        drop(workers);
    }
    // Healthy command retirement still drains in order. A terminal failure cannot
    // promise a complete tail; retain its diagnosis instead of publishing a gap.
    state.pending_iq.clear();
    state.pending_receiver=receiver_workers::ReceiverFrames::default();
    state.consumed_receiver_iq.clear();
    state.decode_snapshots=DecodeSnapshots::default();
    state.fixture_sources=None;
    state.retiring_registry=Arc::from([]);
    state.channel_power=[None;3];
    for client in app.clients.lock().iter().filter(|c|c.audio||c.decode) {
        client.receiver_failed.store(true,Ordering::Release);
        client.wake.notify_one();
    }
    Some(json!({"cmd":"ERROR","code":"receiver_failed","msg":error}))
}

pub(super) fn deliver_retiring(state:&mut DevState,app:&App) {
    let output=std::mem::take(&mut state.pending_receiver);
    deliver_receiver(state,app,output,true);
}

// The caller holds state from receiver drain through enqueue. Commands finish
// newer output under that same lock, so their retiring tail cannot overtake it.
pub(super) fn deliver_receiver(state:&DevState,app:&App,output:receiver_workers::ReceiverFrames,retiring:bool) {
    let epoch=app.epoch.load(Ordering::Acquire);
    let clients=app.clients.lock();
    for (audio,frames) in [(true,output.audio),(false,output.decode)] {
        for frame in frames {
            let message=Message::Binary(frame.into());
            let identity=message_identity(&message);
            for client in clients.iter().filter(|c| if audio {c.audio} else {c.decode}) {
                if retiring && client.channel.is_some_and(|slot| !state.retiring_registry.iter().any(|r|
                    r.channel==slot&&identity==Some(identity_key(r.channel_id,r.epoch)))) {continue;}
                enqueue(client,Outbound {epoch,message:message.clone(),identity:if retiring {None} else {identity},
                    status_revision:None,display_version:None});
            }
        }
    }
}
pub(super) fn identity_key(channel_id:u32,epoch:u32)->u64 { (u64::from(epoch)<<32)|u64::from(channel_id) }

#[derive(serde::Deserialize)]
struct EventIdentity {channel_id:u32,epoch:u32}

pub(super) fn message_identity(message:&Message)->Option<u64> {
    let Message::Binary(bytes)=message else{return None};
    if bytes.starts_with(b"AUD2") && bytes.len()>=96 {
        Some(identity_key(u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
            u32::from_le_bytes(bytes[36..40].try_into().unwrap())))
    } else if bytes.starts_with(b"SDE2") && bytes.len()>=8 {
        serde_json::from_slice::<EventIdentity>(&bytes[8..]).ok()
            .map(|value|identity_key(value.channel_id,value.epoch))
    } else {None}
}
pub(super) fn identity_current(identity:Option<u64>,identities:&[AtomicU64;3],channel:Option<u32>)->bool {
    match identity {
        None=>true,
        Some(identity)=>match channel {
            Some(slot)=>identities.get(slot as usize-1).is_some_and(|current|current.load(Ordering::Acquire)==identity),
            None=>identities.iter().any(|current|current.load(Ordering::Acquire)==identity),
        }
    }
}

impl DevState {
    pub(super) fn stream_registry(&self)->Arc<[sdr_pipeline::ChannelRegistryEntry]> {
        if self.swp.mode!="sdr" {Arc::from([])}
        else if let Some(pipeline)=&self.pipeline {pipeline.snapshot.registry.clone()}
        else if let Some(source)=&self.fixture_sources {source.registry()}
        else {self.ddc.as_ref().map_or_else(||Arc::from([]),|d|d.0.channel_registry())}
    }
    pub(super) fn publish_identities(&self,identities:&[AtomicU64;3]) {
        let registry=self.stream_registry();
        for (index,target) in identities.iter().enumerate() {
            let key=registry.iter().find(|r|r.channel==index as u32+1)
                .map_or(0,|r|identity_key(r.channel_id,r.epoch));
            target.store(key,Ordering::Release);
        }
    }
    pub(super) fn process_receiver_frames(&mut self,frames:&[Vec<u8>])->Result<receiver_workers::ReceiverFrames,String> {
        let registry=self.stream_registry();
        self.process_receiver_frames_with_registry(frames,&registry,false)
    }
    pub(super) fn submit_receiver_frames(&mut self,frames:&[Vec<u8>])->Result<(),String> {
        let registry=self.stream_registry();
        self.submit_receiver_frames_with_registry(frames,&registry,false).map(|_|())
    }
    pub(super) fn drain_receiver_frames(&mut self)->Result<receiver_workers::ReceiverFrames,String> {
        if self.receiver_failure.is_some() {return Ok(receiver_workers::ReceiverFrames::default());}
        let output=match self.receiver_workers.as_mut() {
            Some(workers)=>workers.poll()?,
            None=>receiver_workers::ReceiverFrames::default(),
        };
        self.decode_snapshots.observe(&output.decode)?;
        Ok(output)
    }
    pub(super) fn process_receiver_frames_with_registry(&mut self,frames:&[Vec<u8>],
        registry:&[sdr_pipeline::ChannelRegistryEntry],finish:bool)->Result<receiver_workers::ReceiverFrames,String> {
        let mut output=self.submit_receiver_frames_with_registry(frames,registry,finish)?;
        let drained=self.drain_receiver_frames()?;
        output.audio.extend(drained.audio);output.decode.extend(drained.decode);
        Ok(output)
    }
    fn submit_receiver_frames_with_registry(&mut self,frames:&[Vec<u8>],
        registry:&[sdr_pipeline::ChannelRegistryEntry],finish:bool)->Result<receiver_workers::ReceiverFrames,String> {
        if let Some(error)=&self.receiver_failure {
            return if frames.is_empty() {Ok(receiver_workers::ReceiverFrames::default())} else {Err(error.clone())};
        }
        let configs=self.channel_configs();
        let power=self.fresh_channel_power();
        if self.receiver_workers.is_none() {self.receiver_workers=Some(receiver_workers::ReceiverWorkers::new()?);}
        let workers=self.receiver_workers.as_mut().ok_or("receiver workers unavailable")?;
        workers.configure(&configs)?;
        workers.set_power(power);
        let mut output=receiver_workers::ReceiverFrames::default();
        for frame in frames {
            if frame.len()<56 || !(frame.starts_with(b"IQBF")||frame.starts_with(b"IQAX")) {continue;}
            let id=u32::from_le_bytes(frame[32..36].try_into().unwrap());
            let epoch=u32::from_le_bytes(frame[36..40].try_into().unwrap());
            let sequence=u32::from_le_bytes(frame[8..12].try_into().unwrap());
            let consumed=(id,epoch,sequence);
            if finish && self.consumed_receiver_iq.contains(&consumed) {continue;}
            if !finish && self.consumed_receiver_iq.remove(&consumed) {continue;}
            if !registry.iter().any(|r|r.channel_id==id && r.epoch==epoch) {continue;}
            workers.submit(frame,registry)?;
            if finish {
                let drained=workers.finish()?;
                output.audio.extend(drained.audio);output.decode.extend(drained.decode);
            }
        }
        if finish {
            let drained=workers.finish()?;
            output.audio.extend(drained.audio);output.decode.extend(drained.decode);
        }
        self.decode_snapshots.observe(&output.decode)?;
        Ok(output)
    }
}

async fn stream_ws(ws:WebSocketUpgrade,app:Arc<App>,params:HashMap<String,String>,headers:HeaderMap,decode:bool)->Response {
    if !browser_access(&headers) {return StatusCode::FORBIDDEN.into_response();}
    let channel=match params.get("channel") {
        None=>None,
        Some(value)=>match value.parse::<u32>() {Ok(slot @ 1..=3)=>Some(slot),_=>return StatusCode::BAD_REQUEST.into_response()},
    };
    ws.on_upgrade(move|socket|client_loop(app,socket,false,false,!decode,decode,channel))
}
pub(super) async fn audio_ws_handler(ws:WebSocketUpgrade,State(app):State<Arc<App>>,Query(params):Query<HashMap<String,String>>,headers:HeaderMap)->Response {
    stream_ws(ws,app,params,headers,false).await
}
pub(super) async fn decode_ws_handler(ws:WebSocketUpgrade,State(app):State<Arc<App>>,Query(params):Query<HashMap<String,String>>,headers:HeaderMap)->Response {
    stream_ws(ws,app,params,headers,true).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(id:u32,seq:u32)->receiver_workers::ReceiverFrames {
        let samples=if seq==0 {Vec::new()} else {vec![id as f32+seq as f32/8.0]};
        let audio=websa_channelizer::encode_audio(websa_channelizer::AudioFrameMetadata {
            seq,rate:48_000.0,frequency_hz:100e6,channel_id:id,epoch:7,
            input_index:u64::from(seq),delay_seconds:0.0,channels:1,audio_index:u64::from(seq.saturating_sub(1)),
            input_rate:48_000.0,input_fraction_num:0,input_fraction_den:1,delay_known:true,causal_presentation:true,
        },&samples).unwrap();
        let event=if seq==0 {json!({"type":"start"})}
            else {json!({"type":"drm","status":{"locked":true},"block":seq})};
        let decode=websa_channelizer::encode_decode_event(&serde_json::to_vec(&json!({
            "channel":1,"channel_id":id,"epoch":7,"seq":seq,"mode":"drm","event":event,
        })).unwrap()).unwrap();
        receiver_workers::ReceiverFrames {audio:vec![audio],decode:vec![decode]}
    }

    fn registry(id:u32)->Arc<[sdr_pipeline::ChannelRegistryEntry]> {
        Arc::from([sdr_pipeline::ChannelRegistryEntry {channel:1,channel_id:id,epoch:7,
            frequency_hz:100e6,output_rate:48_000.0,input_rate:48_000.0,
            group_delay_samples:0.0,state:"active"}])
    }

    #[test]
    fn worker_failure_retires_once_and_explicit_mode_change_recovers() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({"commands":{"SET_MODE":{}}}));
        state.swp.mode="sdr".into();
        state.sdr_listen=100e6;
        state.sdr_demod="nfm".into();
        state.sdr_if_bw=12_500.0;
        state.link_up=true;
        let app=App {
            state:StateMutex::new(DevState::new(json!({"preset_defaults":{}}),json!({}))),
            clients:PLMutex::new(Vec::new()),next_client:AtomicU64::new(0),
            epoch:Arc::new(AtomicU64::new(0)),display_version:Arc::new(AtomicU64::new(0)),
            identities:Arc::new(std::array::from_fn(|_|AtomicU64::new(0))),
        };
        let mut receivers=Vec::new();
        for audio in [true,false] {
            let (tx,rx)=mpsc::channel(64);
            app.clients.lock().push(Client {
                id:receivers.len() as u64,tx,dropped:Arc::new(AtomicU64::new(0)),
                overflow:Arc::new(AtomicBool::new(false)),receiver_failed:Arc::new(AtomicBool::new(false)),
                wake:Arc::new(Notify::new()),epoch:app.epoch.clone(),status_revision:app.state.revision(),
                display_version:app.display_version.clone(),iq:false,aux:false,audio,decode:!audio,channel:Some(1),
                identities:app.identities.clone(),latest:Arc::new(PLMutex::new((None,None))),
            });
            receivers.push(rx);
        }
        let frame=|id,seq| websa_channelizer::encode_baseband(websa_channelizer::BasebandMagic::Primary,
            seq,48_000.0,100e6,id,7,0,0.0,&if seq==0 {Vec::new()} else {vec![0.25,0.0].repeat(4096)}).unwrap();
        let old=registry(11);
        state.process_receiver_frames_with_registry(&[frame(11,0)],&old,true).unwrap();
        // A valid wire packet with a missing sequence is rejected by the real
        // asynchronous worker, rather than faking the service's failure result.
        let error=state.process_receiver_frames_with_registry(&[frame(11,2)],&old,true).err().unwrap();
        assert!(error.contains("receiver IQ sequence or clock discontinuity"));
        let message=fail_receiver_session(&mut state,&app,error.clone()).unwrap();
        assert_eq!(message["code"],"receiver_failed");
        assert_eq!(message["msg"],error);
        assert!(state.receiver_workers.is_none());
        assert!(state.link_up,"DSP failure must not report physical disconnection");
        assert_eq!(state.ddc_error.as_deref(),Some(error.as_str()));
        assert_eq!(state.ddc_failure_diagnostics.as_ref().unwrap()["receiver_workers"]["submitted"],2);
        for client in app.clients.lock().iter() {
            assert!(client.receiver_failed.load(Ordering::Acquire));
            assert!(!client.overflow.load(Ordering::Acquire),"receiver failure is not consumer overflow");
        }
        state.hardware_shim=Some("must-not-open-after-failure".into());
        let diagnostics=state.ddc_failure_diagnostics.clone();
        state.preset().unwrap();
        assert_eq!(state.ddc_failure_diagnostics,diagnostics,"preset must retain the terminal diagnosis");
        assert_eq!(state.receiver_failure.as_deref(),Some(error.as_str()));
        assert_eq!(state.ddc_error.as_deref(),Some(error.as_str()));
        assert!(state.pipeline.is_none() && state.receiver_workers.is_none());
        for _ in 0..3 {
            assert!(state.take_hardware_presentation(1).unwrap().is_none());
            assert!(state.drain_receiver_frames().unwrap().decode.is_empty());
            assert_eq!(state.process_receiver_frames_with_registry(&[frame(11,3)],&old,false).err().as_deref(),Some(error.as_str()));
            let empty=state.process_receiver_frames_with_registry(&[],&old,false).unwrap();
            assert!(empty.audio.is_empty() && empty.decode.is_empty());
            assert!(fail_receiver_session(&mut state,&app,"later failure".into()).is_none());
            assert!(state.receiver_workers.is_none(),"polling must not silently start a new session");
        }
        assert_eq!(state.receiver_failure.as_deref(),Some(error.as_str()));
        assert_eq!(state.ddc_error_count,1);
        state.hardware_shim=None;
        assert!(!dispatch(&mut state,"SET_MODE",&json!({"mode":"sdr"})).unwrap());
        assert!(state.receiver_failure.is_some(),"same-mode no-op cannot hide a lost block");
        dispatch(&mut state,"SET_MODE",&json!({"mode":"std"})).unwrap();
        dispatch(&mut state,"SET_MODE",&json!({"mode":"sdr"})).unwrap();
        assert!(state.receiver_failure.is_none());
        state.sdr_listen=100e6;
        state.sdr_demod="nfm".into();
        state.sdr_if_bw=12_500.0;
        let fresh=registry(12);
        let start=state.process_receiver_frames_with_registry(&[frame(12,0)],&fresh,true).unwrap();
        let envelope:Value=serde_json::from_slice(&start.decode[0][8..]).unwrap();
        assert_eq!(envelope["channel_id"],12);
        assert_eq!(envelope["seq"],0);
        assert_eq!(envelope["event"]["type"],"start");
        let output=state.process_receiver_frames_with_registry(&[frame(12,1)],&fresh,true).unwrap();
        assert!(output.audio.iter().any(|frame|u32::from_le_bytes(frame[12..16].try_into().unwrap())>0),
            "fresh session must process real IQ into PCM after retiring the failed workers");
        let next_error=state.process_receiver_frames_with_registry(&[frame(12,3)],&fresh,true).err().unwrap();
        let next_message=fail_receiver_session(&mut state,&app,next_error.clone()).unwrap();
        assert_eq!(next_message["msg"],next_error);
        assert!(next_error.contains("identity=12/7"));
    }

    #[test]
    fn acquisition_delivery_precedes_command_retirement_for_pcm_and_events() {
        // An established subscriber rejects out-of-order source sequences rather
        // than concealing a delivery gap.
        for audio in [true,false] {
            let mut subscriber=subscriber_stream::SubscriberStream::default();
            let frame=|seq| {
                let mut batch=output(11,seq);
                Message::Binary(if audio {batch.audio.remove(0)} else {batch.decode.remove(0)}.into())
            };
            subscriber.frames(frame(0)).unwrap();
            let delayed=frame(1);
            assert_eq!(subscriber.frames(frame(2)).unwrap_err(),"subscriber source sequence discontinuity");
            assert_eq!(subscriber.frames(delayed).unwrap().len(),1);
        }

        let app=Arc::new(App {
            state:StateMutex::new(DevState::new(json!({"preset_defaults":{}}),json!({}))),
            clients:PLMutex::new(Vec::new()),next_client:AtomicU64::new(0),
            epoch:Arc::new(AtomicU64::new(0)),display_version:Arc::new(AtomicU64::new(0)),
            identities:Arc::new(std::array::from_fn(|index|AtomicU64::new(if index==0 {identity_key(11,7)} else {0}))),
        });
        let mut receivers=Vec::new();
        for channel in [None,Some(1)] {
            for audio in [true,false] {
                let (tx,rx)=mpsc::channel(64);
                app.clients.lock().push(Client {
                    id:receivers.len() as u64,tx,dropped:Arc::new(AtomicU64::new(0)),
                    overflow:Arc::new(AtomicBool::new(false)),wake:Arc::new(Notify::new()),
                    receiver_failed:Arc::new(AtomicBool::new(false)),
                    epoch:app.epoch.clone(),status_revision:app.state.revision(),
                    display_version:app.display_version.clone(),iq:false,aux:false,audio,decode:!audio,channel,
                    identities:app.identities.clone(),latest:Arc::new(PLMutex::new((None,None))),
                });
                receivers.push((audio,rx));
            }
        }
        let state=app.state.lock().unwrap();
        deliver_receiver(&state,&app,output(11,0),false);
        let drained=output(11,1);
        let (attempted_tx,attempted_rx)=std::sync::mpsc::sync_channel(0);
        let command_app=app.clone();
        let command=std::thread::spawn(move || {
            attempted_tx.send(()).unwrap();
            let mut state=command_app.state.lock().unwrap();
            state.pending_receiver=output(11,2);
            state.retiring_registry=registry(11);
            deliver_retiring(&mut state,&command_app);
            command_app.identities[0].store(identity_key(12,7),Ordering::Release);
            deliver_receiver(&state,&command_app,output(12,0),false);
            deliver_receiver(&state,&command_app,output(12,1),false);
        });
        // The command is ready to retire seq 2 at the drain/fanout boundary.
        // Keeping state through enqueue forces seq 1 ahead of that retirement.
        attempted_rx.recv().unwrap();
        deliver_receiver(&state,&app,drained,false);
        drop(state);
        command.join().unwrap();

        for (audio,mut rx) in receivers {
            let mut subscriber=subscriber_stream::SubscriberStream::default();
            let mut pcm=Vec::new();
            for (id,seq) in [(11,0),(11,1),(11,2),(12,0),(12,1)] {
                let packet=rx.try_recv().expect("every old tail and new start must be queued");
                let mut batch=output(id,seq);
                let expected=if audio {batch.audio.remove(0)} else {batch.decode.remove(0)};
                let frames=subscriber.frames(packet.message).expect("ordered subscriber stream");
                assert_eq!(frames.len(),1);
                let Message::Binary(bytes)=&frames[0] else {panic!("expected binary receiver frame")};
                assert_eq!(bytes.as_ref(),expected.as_slice());
                if audio {pcm.extend(bytes[96..].chunks_exact(4).map(|v|f32::from_le_bytes(v.try_into().unwrap())));}
            }
            assert!(matches!(rx.try_recv(),Err(mpsc::error::TryRecvError::Empty)));
            if audio {assert_eq!(pcm,vec![11.125,11.25,12.125]);}
        }
        for client in app.clients.lock().iter() {
            assert!(!client.overflow.load(Ordering::Acquire));
            assert_eq!(client.dropped.load(Ordering::Relaxed),0);
        }
    }
}
