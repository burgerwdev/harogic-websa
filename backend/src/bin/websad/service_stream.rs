use super::*;

#[derive(Default)]
pub(super) struct DecodeSnapshots { channels:BTreeMap<u32,DecodeSnapshot> }
struct DecodeSnapshot { envelope:Value, serial:String, width:u32, height:u32, lines:u32, rgb:Vec<u8>, meta:Value, fax:Option<FaxPage> }
const FAX_ROWS:u32=64;
const FAX_PIXEL_LIMIT:usize=5*1024*1024;
struct FaxPage { gray:Vec<u8>, state:Option<Value> }
#[derive(serde::Deserialize)]
struct SnapshotHeader<'a> { #[serde(borrow)] mode:&'a str, #[serde(borrow)] event:&'a serde_json::value::RawValue }
#[derive(serde::Deserialize)]
struct SnapshotImage { width:u32,height:u32,row:u32,rows:u32,rgb:Vec<u8>,meta:Value }
#[derive(serde::Deserialize)]
struct FaxImage<'a> { width:u32,row:u32,rows:u32,#[serde(borrow)] gray:&'a str,revision:u32,meta:Value }
#[derive(serde::Deserialize)]
struct EventKind<'a> { #[serde(rename="type",borrow)] kind:&'a str }
impl DecodeSnapshots {
    pub(super) fn clear_channel(&mut self,channel:u32) {self.channels.remove(&channel);}
    pub(super) fn observe(&mut self,frames:&[Vec<u8>])->Result<(),String> {
        for frame in frames {
            let header:SnapshotHeader<'_>=serde_json::from_slice(&frame[8..]).map_err(|e|e.to_string())?;
            if !matches!(header.mode,"sstv"|"radiofax") {continue;}
            let event:EventKind<'_>=serde_json::from_str(header.event.get()).map_err(|e|e.to_string())?;
            let event_offset=header.event.get().as_ptr() as usize-frame.as_ptr() as usize;
            let mut metadata=Vec::with_capacity(frame.len()-header.event.get().len()+4);
            metadata.extend_from_slice(&frame[8..event_offset]);metadata.extend_from_slice(b"null");
            metadata.extend_from_slice(&frame[event_offset+header.event.get().len()..]);
            let envelope:Value=serde_json::from_slice(&metadata).map_err(|e|e.to_string())?;
            let channel=envelope["channel"].as_u64().ok_or("decode snapshot channel absent")? as u32;
            if !(1..=3).contains(&channel) {return Err("decode snapshot channel invalid".into());}
            let entry=self.channels.entry(channel).or_insert_with(||DecodeSnapshot {
                envelope:Value::Null,serial:String::new(),width:0,height:0,lines:0,rgb:Vec::new(),meta:Value::Null,fax:None });
            if event.kind=="start" {
                entry.rgb.clear();entry.lines=0;entry.serial.clear();entry.fax=None;
            }
            if header.mode=="sstv" && event.kind=="sstv-image" {
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
                entry.lines=row+rows;entry.meta=image.meta;entry.fax=None;
            }
            if header.mode=="radiofax" && matches!(event.kind,"radiofax-image"|"radiofax-state") {
                entry.rgb.clear();
                let page=entry.fax.get_or_insert_with(||FaxPage {gray:Vec::new(),state:None});
                if event.kind=="radiofax-image" {observe_fax_image(entry,header.event.get())?;}
                else {page.state=Some(serde_json::from_str(header.event.get()).map_err(|e|e.to_string())?);}
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
            if snapshot.envelope["mode"]=="radiofax" {
                if let Some(page)=&snapshot.fax {
                    let lines=(page.gray.len()/snapshot.width.max(1) as usize) as u32;
                    for row in (0..lines).step_by(FAX_ROWS as usize) {
                        let end=(row+FAX_ROWS).min(lines);
                        let pixels=&page.gray[row as usize*snapshot.width as usize..end as usize*snapshot.width as usize];
                        let mut meta=snapshot.meta.clone();
                        meta["lines"]=json!(end);
                        meta["complete"]=json!(end==lines && snapshot.meta["complete"]==true);
                        meta["partial"]=json!(end!=lines || snapshot.meta["partial"]==true);
                        sequence+=1;envelope["seq"]=json!(sequence);
                        envelope["event"]=json!({"type":"radiofax-image","width":snapshot.width,"row":row,
                            "rows":end-row,"gray":fax_base64(pixels),"revision":snapshot.height+1,"meta":meta});
                        frames.push(Message::Binary(websa_channelizer::encode_decode_event(&serde_json::to_vec(&envelope).map_err(|e|e.to_string())?).map_err(str::to_owned)?.into()));
                    }
                    if let Some(state)=&page.state {
                        sequence+=1;envelope["seq"]=json!(sequence);envelope["event"]=state.clone();
                        frames.push(Message::Binary(websa_channelizer::encode_decode_event(&serde_json::to_vec(&envelope).map_err(|e|e.to_string())?).map_err(str::to_owned)?.into()));
                    }
                }
            } else {
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
            }
            subscriber.seed_decode(slot,(id,epoch),source,sequence);
        }
        Ok(frames)
    }
}

fn observe_fax_image(entry:&mut DecodeSnapshot,event:&str)->Result<(),String> {
    let image:FaxImage<'_>=serde_json::from_str(event).map_err(|e|format!("radiofax snapshot image: {e}"))?;
    if image.rows==0 || image.rows>FAX_ROWS {return Err("radiofax snapshot row count exceeds 64".into());}
    let end=image.row.checked_add(image.rows).ok_or("radiofax snapshot row overflow")?;
    let pixels=fax_decode(image.gray).map_err(|_|"radiofax snapshot gray is not valid base64".to_string())?;
    let span=(image.width as usize).checked_mul(image.rows as usize).ok_or("radiofax snapshot geometry overflow")?;
    if image.width==0 || pixels.len()!=span {return Err("radiofax snapshot gray length mismatch".into());}
    if span>FAX_PIXEL_LIMIT {return Err("radiofax snapshot exceeds size bound".into());}
    let serial=image.meta["serial"].as_str().ok_or("radiofax snapshot serial absent")?;
    let page=entry.fax.as_mut().ok_or("radiofax snapshot page absent")?;
    let current=(page.gray.len()/entry.width.max(1) as usize) as u32;
    let same=entry.serial==serial && entry.width==image.width && !entry.serial.is_empty();
    if same {
        if image.revision<=entry.height {return Err("radiofax snapshot revision must increase".into());}
        if image.row>current {return Err("radiofax snapshot row discontinuity".into());}
    } else if image.row!=0 {
        return Err("radiofax snapshot page must start at row zero".into());
    }
    let stored=(image.width as usize).checked_mul(end as usize).ok_or("radiofax snapshot geometry overflow")?;
    if stored>FAX_PIXEL_LIMIT {return Err("radiofax snapshot exceeds size bound".into());}
    if !same {
        entry.serial=serial.to_owned();entry.width=image.width;entry.lines=0;page.gray.clear();
    }
    let begin=image.row as usize*image.width as usize;
    if page.gray.len()<begin+pixels.len() {page.gray.resize(begin+pixels.len(),0);}
    page.gray[begin..begin+pixels.len()].copy_from_slice(&pixels);
    let retained = (page.gray.len()/entry.width.max(1) as usize).max(end as usize);
    entry.lines=retained as u32;entry.height=image.revision;entry.meta=image.meta;
    Ok(())
}

fn fax_decode(text:&str)->Result<Vec<u8>,()> {
    fn value(byte:u8)->Option<u8> {
        match byte {b'A'..=b'Z'=>Some(byte-b'A'),b'a'..=b'z'=>Some(byte-b'a'+26),
            b'0'..=b'9'=>Some(byte-b'0'+52),b'+'=>Some(62),b'/'=>Some(63),_=>None}
    }
    if text.len()%4!=0 {return Err(());}
    let bytes=text.as_bytes();
    let mut out=Vec::with_capacity(text.len()/4*3);
    for chunk in bytes.chunks_exact(4) {
        let pad=chunk.iter().rev().take_while(|&&b|b==b'=').count();
        if pad>2 || chunk[..4-pad].contains(&b'=') {return Err(());}
        let mut word=0u32;
        for &byte in &chunk[..4-pad] {word=(word<<6)|u32::from(value(byte).ok_or(())?);}
        word<<=6*pad as u32;
        out.push((word>>16) as u8);
        if pad<2 {out.push((word>>8) as u8);}
        if pad<1 {out.push(word as u8);}
    }
    Ok(out)
}

fn fax_base64(bytes:&[u8])->String {
    const ALPHA: &[u8;64]=b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out=String::with_capacity(bytes.len().div_ceil(3)*4);
    for chunk in bytes.chunks(3) {
        let mut word=0u32;
        for (index,&byte) in chunk.iter().enumerate() {word|=u32::from(byte)<<((2-index)*8);}
        for index in 0..=chunk.len() {out.push(ALPHA[((word>>((3-index)*6))&63) as usize] as char);}
        out.extend(std::iter::repeat('=').take(3-chunk.len()));
    }
    out
}

// Receiver failure ends every shared decoder lane, never the capture session.
// Publication consumes the notice once, including failures found during controls.
pub(super) fn fail_receiver_session(state:&mut DevState,app:&App,error:String)->Option<Value> {
    state.latch_receiver_failure(error);
    publish_receiver_failure(state,app)
}

pub(super) fn publish_receiver_failure(state:&mut DevState,app:&App)->Option<Value> {
    if !std::mem::take(&mut state.receiver_failure_pending) {return None;}
    let error=state.receiver_failure.as_ref()?;
    for client in app.clients.lock().iter().filter(|c|c.audio||c.decode) {
        client.receiver_failed.store(true,Ordering::Release);
        client.wake.notify_one();
    }
    Some(json!({"cmd":"RECEIVER_ERROR","error":error,"failed":true,"scope":"all_receivers"}))
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
    pub(super) fn latch_receiver_failure(&mut self,error:String) {
        if self.receiver_failure.is_some() {return;}
        self.receiver_failure=Some(error);
        self.receiver_failure_pending=true;
        if let Some(workers)=self.receiver_workers.as_mut() {workers.stop_failed();}
        self.pending_receiver=receiver_workers::ReceiverFrames::default();
        self.consumed_receiver_iq.clear();
        self.decode_snapshots=DecodeSnapshots::default();
        self.retiring_registry=Arc::from([]);
    }

    pub(super) fn reap_failed_receivers(&mut self)->bool {
        if self.receiver_failure.is_none() {return true;}
        if self.receiver_failure_pending {return false;}
        if self.receiver_workers.as_ref().is_some_and(|workers| !workers.stopped()) {return false;}
        self.receiver_workers=None;
        true
    }

    pub(super) fn restart_failed_receivers(&mut self)->Result<(),String> {
        if self.receiver_failure.is_none() {return Ok(());}
        self.ddc_command(|ddc|ddc.restart_service_receivers())?;
        if let Some(source)=self.fixture_sources.as_mut() {source.restart(None)?;}
        self.receiver_failure=None;
        self.receiver_failure_pending=false;
        self.consumed_receiver_iq.clear();
        self.config_version+=1;
        Ok(())
    }

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
        if self.receiver_failure.is_some() {return Ok(receiver_workers::ReceiverFrames::default());}
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
    fn worker_failure_is_latched_without_stopping_iq_and_same_demod_recovers() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({"commands":{"SET_MODE":{}}}));
        state.swp.mode="sdr".into();
        state.sdr_listen=100e6;
        state.sdr_demod="nfm".into();
        state.sdr_if_bw=12_500.0;
        state.link_up=true;
        state.sdr_center=100e6;
        let mut source=service_fixture::FixtureSources::new();
        source.configure(&state.channel_configs()).unwrap();
        let initial_registry=source.registry();
        state.fixture_sources=Some(source);
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
        assert_eq!(message["cmd"],"RECEIVER_ERROR");
        assert_eq!(message["error"],error);
        assert!(state.link_up,"DSP failure must not report physical disconnection");
        assert!(state.ddc_error.is_none());
        assert!(state.ddc_failure_diagnostics.is_none());
        let status=state.status_snapshot().build();
        assert_eq!(status["sdr"]["receiver_error"],error);
        assert!(status["last_error"].is_null());
        for client in app.clients.lock().iter() {
            assert!(client.receiver_failed.load(Ordering::Acquire));
            assert!(!client.overflow.load(Ordering::Acquire),"receiver failure is not consumer overflow");
        }
        let mut last_sequence=None;
        for _ in 0..12 {
            let frames=state.fixture_sources.as_mut().unwrap().step().unwrap();
            for frame in &frames {
                let sequence=u32::from_le_bytes(frame[8..12].try_into().unwrap());
                if let Some(last)=last_sequence {assert_eq!(sequence,last+1);}
                last_sequence=Some(sequence);
            }
            let output=state.process_receiver_frames(&frames).unwrap();
            assert!(output.audio.is_empty() && output.decode.is_empty());
            assert!(state.drain_receiver_frames().unwrap().decode.is_empty());
            assert!(fail_receiver_session(&mut state,&app,"later failure".into()).is_none());
            assert_eq!(state.stream_registry()[0].epoch,initial_registry[0].epoch);
        }
        assert!(last_sequence.unwrap()>1,"raw IQ must continue advancing under the failure latch");
        assert_eq!(state.receiver_failure.as_deref(),Some(error.as_str()));
        assert_eq!(state.ddc_error_count,0);
        assert!(!dispatch(&mut state,"SET_MODE",&json!({"mode":"sdr"})).unwrap());
        assert!(state.receiver_failure.is_some(),"same-mode no-op cannot hide a lost block");
        state.receiver_failure_pending=true;
        assert_eq!(dispatch(&mut state,"SET_SDR_DEMOD",&json!({"mode":"nfm"})).unwrap_err().code,"receiver_retiring");
        dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"freq":100e6+100.0})).unwrap();
        dispatch(&mut state,"SET_SDR_DEMOD",&json!({"ifbw":10_000.0})).unwrap();
        assert_eq!(state.sdr_listen,100e6+100.0);
        assert_eq!(state.sdr_if_bw,10_000.0);
        assert!(state.receiver_failure.is_some(),"tuning and filter edits cannot resume a lost receiver session");
        assert!(publish_receiver_failure(&mut state,&app).is_some());
        let deadline=std::time::Instant::now()+Duration::from_secs(2);
        while !state.reap_failed_receivers() {
            assert!(std::time::Instant::now()<deadline,"stopped workers must retire");
            std::thread::yield_now();
        }
        dispatch(&mut state,"SET_SDR_DEMOD",&json!({"mode":"nfm","ifbw":12_500.0})).unwrap();
        assert!(state.receiver_failure.is_none());
        let fresh=state.stream_registry();
        assert_ne!((fresh[0].channel_id,fresh[0].epoch),(initial_registry[0].channel_id,initial_registry[0].epoch));
        let frames=state.fixture_sources.as_mut().unwrap().step().unwrap();
        let output=state.process_receiver_frames_with_registry(&frames,&fresh,true).unwrap();
        let envelope:Value=serde_json::from_slice(&output.decode[0][8..]).unwrap();
        assert_eq!(envelope["epoch"],fresh[0].epoch);
        assert_eq!(envelope["seq"],0);
        assert_eq!(envelope["event"]["type"],"start");
        assert!(output.audio.iter().any(|frame|u32::from_le_bytes(frame[12..16].try_into().unwrap())>0),
            "fresh session must process real IQ into PCM after retiring the failed workers");
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

    fn fax_frame(seq:u32,event:Value)->Vec<u8> {
        websa_channelizer::encode_decode_event(&serde_json::to_vec(&json!({
            "channel":1,"channel_id":11,"epoch":7,"seq":seq,"mode":"radiofax","event":event,
        })).unwrap()).unwrap()
    }
    fn fax_meta(serial:&str,lines:u32,complete:bool)->Value {
        json!({"serial":serial,"epoch":4,"hz":7.8e6,"ioc":576,"lpm":120,"lines":lines,
            "complete":complete,"partial":!complete,"segmentIndex":1,"transmissionId":"paper"})
    }
    fn fax_image(row:u32,gray:&[u8],revision:u32,serial:&str,lines:u32)->Value {
        json!({"type":"radiofax-image","width":2,"row":row,"rows":gray.len()/2,
            "gray":fax_base64(gray),"revision":revision,"meta":fax_meta(serial,lines,lines==gray.len() as u32/2+row)})
    }

    #[test]
    fn radiofax_snapshot_appends_replaces_and_recovers_for_late_subscribers() {
        let mut snapshots=DecodeSnapshots::default();
        snapshots.observe(&[fax_frame(0,json!({"type":"start"})),
            fax_frame(1,fax_image(0,&[1,2,3,4],1,"A",2)),
            fax_frame(2,fax_image(2,&[5,6],2,"A",3))]).unwrap();
        let page=&snapshots.channels[&1].fax.as_ref().unwrap().gray;
        assert_eq!(page,&[1,2,3,4,5,6]);
        assert_eq!(snapshots.channels[&1].height,2);
        snapshots.observe(&[fax_frame(3,fax_image(0,&[9,8,7,6,5,4],3,"A",3))]).unwrap();
        assert_eq!(snapshots.channels[&1].fax.as_ref().unwrap().gray,&[9,8,7,6,5,4]);
        assert_eq!(snapshots.channels[&1].lines,3);
        let state=json!({"type":"radiofax-state","running":true,"lines":2,"complete":false});
        snapshots.observe(&[fax_frame(4,state.clone())]).unwrap();
        let registry=[sdr_pipeline::ChannelRegistryEntry {channel:1,channel_id:11,epoch:7,
            frequency_hz:7.8e6,output_rate:12_000.0,input_rate:48_000.0,group_delay_samples:0.0,state:"active"}];
        let mut subscriber=subscriber_stream::SubscriberStream::default();
        let frames=snapshots.initial(Some(1),&registry,&mut subscriber).unwrap();
        let events:Vec<Value>=frames.iter().map(|message| {
            let Message::Binary(bytes)=message else {panic!("binary snapshot")};
            serde_json::from_slice(&bytes[8..]).unwrap()
        }).collect();
        assert_eq!(events.iter().map(|event|event["seq"].as_u64().unwrap()).collect::<Vec<_>>(),vec![0,1,2]);
        assert_eq!(events[0]["event"]["type"],"start");
        assert_eq!(events[1]["event"]["row"],0);
        assert_eq!(events[1]["event"]["rows"],3);
        assert_eq!(events[1]["event"]["gray"],fax_base64(&[9,8,7,6,5,4]));
        assert_eq!(events[1]["event"]["revision"],4);
        assert_eq!(events[2]["event"],state);
        assert!(subscriber.frames(Message::Binary(fax_frame(4,state).into())).unwrap().is_empty());
        assert_eq!(subscriber.frames(Message::Binary(fax_frame(5,json!({"type":"radiofax-state"})).into())).unwrap().len(),1);
    }

    #[test]
    fn radiofax_snapshot_rejects_invalid_gray_geometry_and_ordering() {
        let valid=|event| {
            let mut snapshots=DecodeSnapshots::default();
            snapshots.observe(&[fax_frame(0,json!({"type":"start"})),fax_frame(1,event)])
        };
        assert!(valid(json!({"type":"radiofax-image","width":2,"row":0,"rows":1,
            "gray":"A","revision":1,"meta":fax_meta("A",1,false)})).unwrap_err().contains("base64"));
        assert!(valid(json!({"type":"radiofax-image","width":2,"row":0,"rows":65,
            "gray":fax_base64(&[0;130]),"revision":1,"meta":fax_meta("A",65,false)})).unwrap_err().contains("64"));
        assert!(valid(json!({"type":"radiofax-image","width":2,"row":0,"rows":1,
            "gray":fax_base64(&[1,2,3]),"revision":1,"meta":fax_meta("A",1,false)})).unwrap_err().contains("length"));
        assert!(valid(json!({"type":"radiofax-image","width":2,"row":1,"rows":1,
            "gray":fax_base64(&[1,2]),"revision":1,"meta":fax_meta("A",2,false)})).unwrap_err().contains("row zero"));
        let mut snapshots=DecodeSnapshots::default();
        snapshots.observe(&[fax_frame(0,json!({"type":"start"})),fax_frame(1,fax_image(0,&[1,2],1,"A",1))]).unwrap();
        let error=snapshots.observe(&[fax_frame(2,fax_image(0,&[3,4],1,"A",1))]).unwrap_err();
        assert!(error.contains("revision"),"{error}");
        let error=snapshots.observe(&[fax_frame(2,fax_image(4,&[3,4],2,"A",5))]).unwrap_err();
        assert!(error.contains("discontinuity"),"{error}");
        let mut entry=DecodeSnapshot {
            envelope:Value::Null,serial:"B".into(),width:1,height:1,lines:1,
            rgb:Vec::new(),meta:Value::Null,fax:Some(FaxPage {gray:vec![7],state:None}) };
        let event=json!({"type":"radiofax-image","width":FAX_PIXEL_LIMIT+1,"row":0,"rows":1,
            "gray":"A".repeat((FAX_PIXEL_LIMIT+1).div_ceil(3)*4),"revision":2,
            "meta":fax_meta("B",1,false)}).to_string();
        assert!(observe_fax_image(&mut entry,&event).unwrap_err().contains("size"));
    }
}
