//! P3 logical receiver API. Slots are a private bridge to the existing three-lane service.
use super::*;
use receiver_workers::{BasebandConfig, ChannelConfig};
use std::collections::VecDeque;

const SOURCE: &str = "primary";
const MAX_RECEIVERS: usize = 3;
const HISTORY_LIMIT: usize = 256;

/// Equal receiver audio settings can share one local speaker route even when their
/// independent IQ plans, logical identities and recording tasks differ.
fn audio_signature(channel: &ChannelConfig) -> String {
    let mut value = json!(channel);
    let fields = value.as_object_mut().unwrap();
    for key in ["channel", "enabled", "baseband", "am_sync_generation"] {
        fields.remove(key);
    }
    value.to_string()
}

struct LogicalReceiver {
    uid: String,
    revision: u64,
    config: Value,
    channel: ChannelConfig,
}

#[derive(Clone)]
pub(super) struct IqIdentity {
    pub uid: String,
    pub slot: u32,
    pub entry: sdr_pipeline::ChannelRegistryEntry,
    pub origin: u64,
    pub baseband: Value,
    pub clock: Option<Value>,
    pub revision: u64,
}

pub(super) struct Directory {
    boot: String,
    next: u64,
    snapshot_revision: u64,
    receivers: BTreeMap<u32, LogicalReceiver>,
    history: VecDeque<IqIdentity>,
    replies: VecDeque<(String, Value, Value)>,
    recording_requests: BTreeMap<String,(Value,recording::StartTicket)>,
}

impl Default for Directory {
    fn default() -> Self {
        Self { boot: format!("{}-{}",std::process::id(),std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()), next:0,snapshot_revision:0,
            receivers:BTreeMap::new(),history:VecDeque::new(),replies:VecDeque::new(),recording_requests:BTreeMap::new() }
    }
}

#[derive(Default)]
pub(super) struct ProcessingBranches {
    current:BTreeMap<u32,ProcessingBranch>,
    history:VecDeque<(sdr_pipeline::ChannelRegistryEntry,sdr_pipeline::ChannelRegistryEntry)>,
}
struct ProcessingBranch {
    config:ChannelConfig,
    source:sdr_pipeline::ChannelRegistryEntry,
    output:sdr_pipeline::ChannelRegistryEntry,
    first_sequence:u32,
}
impl ProcessingBranches {
    pub(super) fn invalidate(&mut self) {self.current.clear();}
    pub(super) fn retire_channel(&mut self,channel:u32) {self.current.remove(&channel);}
    pub(super) fn uses_channel(&self,channel:u32)->bool {self.history.iter().any(|(entry,_)|entry.channel==channel)}
    pub(super) fn input(&mut self,frame:&[u8],source:&sdr_pipeline::ChannelRegistryEntry,config:&ChannelConfig)
        ->Result<(sdr_pipeline::ChannelRegistryEntry,u32,bool),String> {
        let sequence=u32::from_le_bytes(frame[8..12].try_into().unwrap());
        let fresh=self.current.get(&source.channel).is_none_or(|branch|
            branch.source.channel_id!=source.channel_id || branch.source.epoch!=source.epoch || branch.config.resets_receiver(config));
        if fresh {
            let epoch=allocate_dsp_epoch().map_err(str::to_owned)?;
            let mut output=source.clone();output.channel_id=epoch;output.epoch=epoch;
            self.history.push_back((output.clone(),source.clone()));
            self.current.insert(source.channel,ProcessingBranch {config:config.clone(),source:source.clone(),output,first_sequence:sequence});
            while self.history.len()>HISTORY_LIMIT {
                let old=self.history.iter().position(|(output,_)|!self.current.values().any(|b|b.output.channel_id==output.channel_id&&b.output.epoch==output.epoch));
                if let Some(index)=old {self.history.remove(index);} else {break;}
            }
        }
        let branch=&self.current[&source.channel];
        let translated=if branch.first_sequence==0 {sequence} else {sequence.checked_sub(branch.first_sequence)
            .and_then(|value|value.checked_add(1)).ok_or("processing source sequence overflow")?};
        Ok((branch.output.clone(),translated,fresh && sequence!=0))
    }
}
impl DevState {
    pub(super) fn processing_source_identity(&self,identity:Option<u64>)->Option<u64> {
        identity.map(|key|self.processing_branches.history.iter().find(|(output,_)|
            service_stream::identity_key(output.channel_id,output.epoch)==key).map_or(key,|(_,source)|
                service_stream::identity_key(source.channel_id,source.epoch)))
    }
    pub(super) fn processing_registry(&self)->Arc<[sdr_pipeline::ChannelRegistryEntry]> {
        self.stream_registry().iter().map(|source|self.processing_branches.current.get(&source.channel)
            .filter(|b|b.source.channel_id==source.channel_id&&b.source.epoch==source.epoch)
            .map_or_else(||source.clone(),|b|b.output.clone())).collect()
    }
}

pub(super) fn config_value(channel: &ChannelConfig, previous: Option<&Value>) -> Value {
    let plan = service_ddc::channel_plan(COARSE_RATE, channel).ok();
    let mut config = previous.cloned().unwrap_or_else(||json!({
        "name":format!("C{}",channel.channel),"color":"#4cc99b","tags":[],
    }));
    config["enabled"]=json!(channel.enabled);
    config["purpose"]=json!(if !channel.processing_enabled {"iq"}
        else if matches!(channel.mode.as_str(),"ft8"|"sstv"|"radiofax"|"drm"|"drmplus")
            || websa_channelizer::catalog::is_external(&channel.mode) {"digital"} else {"listen"});
    config["baseband"]=json!({"frequency_hz":channel.freq,
        "passband_hz":channel.baseband.as_ref().map(|b|b.passband_hz).or_else(||plan.as_ref().map(|p|p.occupied_bandwidth_hz)).unwrap_or(channel.if_bw),
        "transition_hz":channel.baseband.as_ref().map(|b|b.transition_hz).or_else(||plan.as_ref().map(|p|p.transition_hz)).unwrap_or(100.0),
        "minimum_rate_hz":channel.baseband.as_ref().map(|b|b.minimum_rate_hz).unwrap_or(1.0)});
    config["processors"]=if channel.processing_enabled {json!([{
        "id":"primary","kind":if config["purpose"]=="digital" {"decoder"} else {"demodulator"},
        "mode":channel.mode,"minimum_rate_hz":websa_channelizer::catalog::mode(&channel.mode).map_or(0,|d|d.minimum_rate_hz),
        "bandwidth_hz":channel.if_bw,"pcm_gain":channel.volume,"agc":channel.agc,"squelch_dbm":channel.squelch
    }])} else {json!([])};
    config
}

impl DevState {
    pub(super) fn remember_receiver_streams(&mut self) {
        let configs=self.channel_configs();
        let registry=self.stream_registry();
        for channel in configs {
            if !channel.enabled && !self.receivers.receivers.contains_key(&channel.channel) {continue;}
            if !self.receivers.receivers.contains_key(&channel.channel) {
                self.receivers.next+=1;
                self.receivers.receivers.insert(channel.channel,LogicalReceiver {
                    uid:format!("receiver-{}-{}",self.receivers.boot,self.receivers.next),
                    revision:self.config_version,config:config_value(&channel,None),channel:channel.clone(),
                });
            }
            let receiver=self.receivers.receivers.get_mut(&channel.channel).unwrap();
            if receiver.channel!=channel {
                receiver.revision=self.config_version;
                receiver.config=config_value(&channel,Some(&receiver.config));
                receiver.channel=channel;
            }
            if let Some(entry)=registry.iter().find(|entry|entry.channel==receiver.channel.channel) {
                if !self.receivers.history.iter().any(|known|known.entry.channel_id==entry.channel_id && known.entry.epoch==entry.epoch) {
                    self.receivers.history.push_back(IqIdentity {uid:receiver.uid.clone(),slot:entry.channel,
                        entry:entry.clone(),origin:0,baseband:receiver.config["baseband"].clone(),clock:None,revision:receiver.revision});
                }
            }
        }
        while self.receivers.history.len()>HISTORY_LIMIT {
            // A quiet, unchanged receiver can outlive hundreds of retunes on a
            // different lane. Its active identity must never age out of history.
            let retired=self.receivers.history.iter().position(|known|!registry.iter().any(|entry|
                entry.channel_id==known.entry.channel_id && entry.epoch==known.entry.epoch));
            if let Some(index)=retired {self.receivers.history.remove(index);} else {break;}
        }
    }

    pub(super) fn receiver_snapshot(&mut self) -> Value {
        self.remember_receiver_streams();
        let registry=self.stream_registry();
        let capture_fault=self.ddc_error.as_deref().or_else(||(!self.link_up).then_some("capture disconnected"));
        let receivers:Vec<_>=self.receivers.receivers.values().map(|receiver| {
            let entry=registry.iter().find(|entry|entry.channel==receiver.channel.channel);
            json!({"receiver_uid":receiver.uid,"source_id":SOURCE,"display_number":receiver.channel.channel,
                "revision":receiver.revision.to_string(),"confirmed":receiver.config,
                "audio_signature":audio_signature(&receiver.channel),
                "state":if !receiver.channel.enabled {"stopped"} else if capture_fault.is_some() {"failed"}
                    else if entry.is_some() {"running"} else {"interrupted"},
                "stream":entry.map(|e|json!({"source_id":SOURCE,"epoch":e.epoch.to_string(),"channel_id":e.channel_id.to_string()})),
                "clock":entry.and_then(|e|self.receivers.history.iter().find(|known|
                    known.entry.channel_id==e.channel_id&&known.entry.epoch==e.epoch)).and_then(|known|known.clock.clone()),
                "fault":capture_fault.map(|error|fault("capture","CAPTURE_FAILED",error)).or_else(||
                    receiver.channel.processing_enabled.then_some(self.receiver_failure.as_deref()).flatten()
                        .map(|error|fault("receiver","PROCESSING_FAILED",error)))})
        }).collect();
        let mut snapshot=json!({"contract":"multichannel-draft-v1","transport":"native","service_id":self.receivers.boot,
            "revision":self.config_version.to_string(),
            "capabilities":{"max_channels":MAX_RECEIVERS,"capacity_target":128,
                "receiver_presets":self.receiver_presets.is_some(),
                "modes":websa_channelizer::catalog::manifest().iter().filter_map(|m|m["id"].as_str()).collect::<Vec<_>>(),
                "mode_descriptors":websa_channelizer::catalog::manifest().into_iter().filter(|m|matches!(m["kind"].as_str(),Some("analog"|"digital"))).collect::<Vec<_>>(),
                "commands":if self.recordings.is_some() {vec!["create","configure","retire","output","record_start","record_stop"]} else {vec!["create","configure","retire","output"]},
                "recording":self.recordings.is_some(),"recording_unavailable_reason":self.recording_error,"recording_kinds":if self.recordings.is_some() {vec!["iq","results","audio"]}else{vec![]},
                "results_archive_export":self.recordings.is_some(),"audio_archive_export":self.recordings.is_some(),
                "audio_flac_export":self.recordings.is_some(),
                "audio_archive_read":self.recordings.is_some(),"audio_range_max_samples":"1048576","audio_range_max_seconds":60,
                "iq_file_demodulate":self.recordings.is_some(),"iq_file_demodulate_modes":super::recording_demod::MODES,
                "iq_file_demodulate_max_seconds":60,
                "iq_file_demodulate_offset":self.recordings.is_some(),
                "iq_archive_read":self.recordings.is_some(),"iq_range_max_samples":"1048576",
                "iq_archive_collection":self.recordings.is_some(),"iq_collection_max_samples":super::recording_export::MAX_SAMPLES.to_string(),"iq_collection_max_segments":super::recording_collection::MAX_SEGMENTS,
                "iq_archive_export":self.recordings.is_some(),"iq_export_max_samples":super::recording_export::MAX_SAMPLES.to_string(),"monitor_mixing":true},
            "capture":self.capture_snapshot(),
            "receivers":receivers,"outputs":[],"recordings":self.recordings.as_ref().map(|r|r.snapshots()).unwrap_or_default()});
        if self.recordings.is_some() {
            self.receivers.snapshot_revision+=1;
            snapshot["state_revision"]=json!(self.receivers.snapshot_revision.to_string());
        }
        snapshot
    }

    pub(super) fn receiver_slot(&mut self,uid:&str)->Option<u32> {
        self.remember_receiver_streams();
        self.receivers.receivers.iter().find(|(_,receiver)|receiver.uid==uid).map(|(&slot,_)|slot)
    }

    pub(super) fn iq_identity(&mut self,id:u32,epoch:u32)->Option<IqIdentity> {
        self.remember_receiver_streams();
        self.receivers.history.iter().find(|known|known.entry.channel_id==id && known.entry.epoch==epoch).cloned()
    }
}

/// Bind processed output to its original logical source, including retiring tails.
pub(super) fn processing_frame(state:&mut DevState,uid:&str,frame:Message)->Result<Option<Message>,String> {
    if matches!(&frame,Message::Close(_)) {return Ok(Some(frame));}
    let Some(identity)=service_stream::message_identity(&frame) else {return Err("processed stream has no identity".into());};
    let Some(known)=state.iq_identity(state.processing_source_identity(Some(identity)).unwrap() as u32,
        (state.processing_source_identity(Some(identity)).unwrap()>>32) as u32) else {return Ok(None);};
    if known.uid!=uid {return Ok(None);}
    let Message::Binary(bytes)=frame else {return Ok(None);};
    if bytes.starts_with(b"SDE2") {
        let mut envelope:Value=serde_json::from_slice(&bytes[8..]).map_err(|error|error.to_string())?;
        envelope["baseband_stream"]=json!({"source_id":SOURCE,"channel_id":known.entry.channel_id.to_string(),"epoch":known.entry.epoch.to_string()});
        envelope["receiver_uid"]=json!(uid);envelope["service_id"]=json!(state.receivers.boot);
        return websa_channelizer::encode_decode_event(&serde_json::to_vec(&envelope).map_err(|e|e.to_string())?)
            .map(|bytes|Some(Message::Binary(bytes.into()))).map_err(str::to_owned);
    }
    Ok(Some(Message::Binary(bytes)))
}

fn fault(scope:&str,code:&str,message:&str)->Value {
    json!({"scope":scope,"code":code,"message":message,"retryable":false})
}

struct Change {slot:u32,config:Value,data:Value,retire:bool,revision:u64,output_bytes:f64}

fn prepare_change(state:&mut DevState,request:&Value)->Result<Change,String> {
    if request.get("service_id").is_some_and(|id|id!=&state.receivers.boot) {
        return Err("source service restarted; refresh before retrying".into());
    }
    if state.swp.mode!="sdr" || !state.link_up {return Err("SDR capture must be connected before editing receivers".into());}
    state.remember_receiver_streams();
    let kind=request["type"].as_str().ok_or("receiver command type missing")?;
    let slot=if kind=="create" {
        (1..=MAX_RECEIVERS as u32).find(|slot|!state.receivers.receivers.contains_key(slot))
            .ok_or("receiver capacity reached")?
    } else {
        let uid=request["receiver_uid"].as_str().ok_or("receiver_uid missing")?;
        state.receiver_slot(uid).ok_or("receiver not found")?
    };
    let previous=state.receivers.receivers.get(&slot);
    if kind=="configure" {
        let expected=request["expected_revision"].as_str().and_then(|v|v.parse::<u64>().ok()).ok_or("expected_revision must be a decimal string")?;
        if previous.is_none_or(|r|r.revision!=expected) {return Err("receiver revision changed; refresh before retrying".into());}
    }
    let mut config=if kind=="retire" {previous.ok_or("receiver absent")?.config.clone()}
        else if matches!(kind,"create"|"configure") {request["config"].clone()}
        else {return Err("unsupported receiver command".into());};
    if kind=="retire" {config["enabled"]=json!(false);}
    if config["name"].as_str().is_none_or(|v|v.is_empty()||v.len()>128)
        || config["color"].as_str().is_none_or(|v|v.len()!=7||!v.starts_with('#')||!v[1..].bytes().all(|c|c.is_ascii_hexdigit()))
        || config["tags"].as_array().is_none_or(|tags|tags.len()>32||tags.iter().any(|t|t.as_str().is_none_or(|v|v.len()>64))) {
        return Err("invalid receiver name, color or tags".into());
    }
    let enabled=config["enabled"].as_bool().ok_or("enabled must be boolean")?;
    if enabled && state.hardware_shim.is_some()
        && (state.ddc_error.is_some() || state.live_capture_parameters().is_none()) {
        return Err("Start or recover the shared capture before enabling receivers".into());
    }
    let purpose=config["purpose"].as_str().ok_or("purpose missing")?;
    if !matches!(purpose,"iq"|"listen"|"digital") {return Err("unsupported receiver purpose".into());}
    let bb=&config["baseband"];
    let frequency=bb["frequency_hz"].as_f64().filter(|v|v.is_finite()&&*v>0.0).ok_or("frequency_hz must be finite and positive")?;
    let baseband=BasebandConfig {passband_hz:bb["passband_hz"].as_f64().ok_or("passband_hz missing")?,
        transition_hz:bb["transition_hz"].as_f64().ok_or("transition_hz missing")?,
        minimum_rate_hz:bb["minimum_rate_hz"].as_f64().ok_or("minimum_rate_hz missing")?};
    let processors=config["processors"].as_array().ok_or("processors must be an array")?;
    if processors.len()>1 || (purpose=="iq" && !processors.is_empty()) || (purpose!="iq" && processors.is_empty()) {
        return Err("Each receiver supports zero or one primary processor; IQ-only receivers must use an empty set".into());
    }
    let mut channel=state.channel_configs()[(slot-1) as usize].clone();
    channel.freq=frequency;channel.enabled=enabled;channel.processing_enabled=!processors.is_empty();
    channel.baseband=Some(baseband);
    if let Some(processor)=processors.first() {
        if !matches!(processor["kind"].as_str(),Some("demodulator"|"decoder")) {return Err("unsupported processor kind".into());}
        channel.mode=processor["mode"].as_str().ok_or("processor mode missing")?.into();
        let descriptor=websa_channelizer::catalog::mode(&channel.mode).ok_or("unsupported processor mode")?;
        if !descriptor.available {return Err(descriptor.unavailable_reason.unwrap_or_else(||"processor unavailable".into()));}
        if (purpose=="listen") != (descriptor.kind=="analog")
            || processor["kind"]!=if purpose=="listen" {"demodulator"} else {"decoder"} {
            return Err("processor kind and purpose must match the mode descriptor".into());
        }
        channel.if_bw=processor["bandwidth_hz"].as_f64().unwrap_or(descriptor.default_bandwidth_hz);
        if let Some(gain)=processor.get("pcm_gain") {channel.volume=gain.as_f64().ok_or("invalid PCM gain")?;}
        if let Some(agc)=processor.get("agc") {channel.agc=agc.as_bool().ok_or("invalid AGC flag")?;}
        if let Some(squelch)=processor.get("squelch_dbm") {channel.squelch=squelch.as_f64().ok_or("invalid squelch")?;}
        let floor=processor["minimum_rate_hz"].as_f64().filter(|v|v.is_finite()&&*v>=0.0).ok_or("invalid processor minimum rate")?;
        channel.baseband.as_mut().unwrap().minimum_rate_hz=channel.baseband.as_ref().unwrap().minimum_rate_hz.max(floor);
    }
    channel.validate()?;
    let fs=state.capture_bandwidth()/0.8;
    let plan=service_ddc::source_plan(fs,&channel)?;
    if enabled {
        let (lo,hi)=state.freq_limits();
        if !(lo..=hi).contains(&frequency) {return Err("frequency outside device RF limits".into());}
        if (frequency-state.capture_center()).abs()+plan.plan().stopband_hz>state.capture_bandwidth()/2.0 {
            return Err("full passband and transition exceed shared capture".into());
        }
        service_ddc::validate_source_frequency(state.capture_center(),state.capture_bandwidth(),fs,
            state.sdr_pfb_taps_per_phase,frequency,&plan)?;
    }
    let mut data=json!({"cmd":"SET_SDR_CHANNEL","channel":slot,"enabled":enabled,"freq":frequency,
        "mode":channel.mode,"if_bw":channel.if_bw,"baseband":channel.baseband,
        "processing_enabled":channel.processing_enabled,"volume":channel.volume,"agc":channel.agc,"squelch":channel.squelch,"request_id":request["request_id"]});
    // A retained compatibility mode is irrelevant to IQ-only. Sending it would
    // also look like an explicit legacy decoder-recovery request after a fault.
    if !channel.processing_enabled {data.as_object_mut().unwrap().remove("mode");}
    Ok(Change {slot,config,data,retire:kind=="retire",revision:state.config_version,
        output_bytes:if enabled {plan.plan().output_rate*8.0} else {0.0}})
}

pub(super) async fn snapshot(State(app):State<Arc<App>>)->Response {
    match tokio::task::spawn_blocking(move|| app.state.lock().unwrap().receiver_snapshot()).await {
        Ok(value)=>Json(value).into_response(),Err(error)=>state_worker_failed(error).into_response(),
    }
}

pub(super) async fn command(State(app):State<Arc<App>>,headers:HeaderMap,body:String)->Response {
    execute(app,headers,body,false).await
}
pub(super) async fn preflight(State(app):State<Arc<App>>,headers:HeaderMap,body:String)->Response {
    execute(app,headers,body,true).await
}

async fn execute(app:Arc<App>,headers:HeaderMap,body:String,preview:bool)->Response {
    if !browser_access(&headers) {return StatusCode::FORBIDDEN.into_response();}
    let request:Value=match serde_json::from_str(&body) {Ok(value)=>value,Err(_)=>return StatusCode::BAD_REQUEST.into_response()};
    let Some(id)=request["request_id"].as_str().filter(|id|!id.is_empty()&&id.len()<=128).map(str::to_owned)
        else {return (StatusCode::BAD_REQUEST,Json(error_body("request_id string required","request_id_required"))).into_response();};
    if matches!(request["type"].as_str(),Some("record_start"|"record_stop")) {
        return recording_command(app,request,id,preview).await;
    }
    let candidate={
        let mut state=app.state.lock().unwrap();
        if !preview {
            if let Some((_,old,reply))=state.receivers.replies.iter().find(|(known,_,_)|known==&id) {
                if old==&request {return Json(reply.clone()).into_response();}
                return (StatusCode::CONFLICT,Json(error_body("request_id reused with a different command","identity_conflict"))).into_response();
            }
        }
        prepare_change(&mut state,&request)
    };
    let result=match candidate {
        Ok(change)=>match service_preflight::preflight(&app,"SET_SDR_CHANNEL",&change.data).await {
            Ok(admission)=>Ok((change,admission)),Err(error)=>Err(error),
        },Err(error)=>Err(error),
    };
    let worker_app=app.clone();
    match tokio::task::spawn_blocking(move|| {
        let mut state=worker_app.state.lock().unwrap();
        // Admission runs without state held. A concurrent retry may already have
        // committed this request while we prepared; never replace its receipt.
        if !preview {
            if let Some((_,old,reply))=state.receivers.replies.iter().find(|(known,_,_)|known==&id) {
                let cached=if old==&request {reply.clone()} else {
                    json!({"request_id":id,"snapshot":state.receiver_snapshot(),
                        "fault":fault("request","IDENTITY_CONFLICT","request_id reused with a different command")})
                };
                drop(state);drop(result);return cached;
            }
        }
        let mut failure=None;
        let mut estimated_bytes=0.0;
        match result {
            Ok((change,mut admission))=>{
                estimated_bytes=change.output_bytes;
                if state.config_version!=change.revision {failure=Some("configuration changed during admission; refresh before retrying".to_owned());}
                else if !preview {
                    if let Err(error)=admission.dispatch(&mut state,"SET_SDR_CHANNEL",&change.data) {
                        let capture_failed=state.ddc.as_ref().is_some_and(|d|d.0.capture_failed)
                            || state.pipeline.as_ref().is_some_and(|p|p.failure_diagnostics().get("error").is_some());
                        if capture_failed {state.hardware_presentation_failed(error.message.clone());}
                        failure=Some(error.message);
                    }
                    else {
                        if state.config_version==change.revision {state.config_version+=1;}
                        state.remember_receiver_streams();
                        if !state.receivers.receivers.contains_key(&change.slot) {
                            state.receivers.next+=1;
                            let uid=format!("receiver-{}-{}",state.receivers.boot,state.receivers.next);
                            let channel=state.channel_configs()[(change.slot-1) as usize].clone();
                            let revision=state.config_version;
                            state.receivers.receivers.insert(change.slot,LogicalReceiver {uid,revision,
                                channel,config:change.config.clone()});
                        }
                        let revision=state.config_version;
                        if let Some(receiver)=state.receivers.receivers.get_mut(&change.slot) {receiver.config=change.config;receiver.revision=revision;}
                        if change.retire {state.receivers.receivers.remove(&change.slot);}
                    }
                    service_stream::deliver_retiring(&mut state,&worker_app);
                    state.publish_identities(&worker_app.identities);
                    worker_app.display_version.store(state.freq_version,Ordering::Release);
                }
                drop(state);drop(admission);state=worker_app.state.lock().unwrap();
            },Err(error)=>failure=Some(error),
        }
        if preview {
            return json!({"accepted":failure.is_none(),"faults":failure.map(|e|vec![fault("request","REJECTED",&e)]).unwrap_or_default(),
                "capture_change":false,"affected_receivers":request["receiver_uid"].as_str().map(|v|vec![v]).unwrap_or_default(),
                "affected_tracks":[],"candidate_center_hz":state.capture_center(),"estimated_cf32_bytes_per_second":estimated_bytes});
        }
        let reply=json!({"request_id":id,"snapshot":state.receiver_snapshot(),
            "fault":failure.map(|message|fault(if state.ddc_error.is_some() {"capture"} else {"request"},"REJECTED",&message))});
        state.receivers.replies.push_back((id,request,reply.clone()));
        if state.receivers.replies.len()>128 {state.receivers.replies.pop_front();}
        let status=state.status_snapshot();drop(state);
        worker_app.state.publish_status(status.revision,status.build());
        reply
    }).await {
        Ok(reply)=>Json(reply).into_response(),Err(error)=>state_worker_failed(error).into_response(),
    }
}

async fn recording_command(app:Arc<App>,request:Value,id:String,preview:bool)->Response {
    match tokio::task::spawn_blocking(move|| {
        let mut state=app.state.lock().unwrap();
        if !preview {
            if let Some((_,old,reply))=state.receivers.replies.iter().find(|(key,_,_)|key==&id) {
                if old==&request {return reply.clone();}
                return json!({"request_id":id,"snapshot":state.receiver_snapshot(),"fault":fault("request","IDENTITY_CONFLICT","request_id reused with different command")});
            }
        }
        let mut ticket=None;
        let mut result=if let Some((old,pending))=state.receivers.recording_requests.get(&id) {
            if old!=&request {Err("request_id reused with different command".to_owned())}
            else {ticket=Some(pending.clone());Ok(())}
        } else {(||->Result<(),String> {
            if request["service_id"].as_str().is_some_and(|s|s!=state.receivers.boot) {return Err("source service restarted; refresh before retrying".into());}
            if state.recordings.is_none() {return Err("backend recording storage unavailable".into());}
            if request["type"]=="record_start" {
                let kind=if request["kinds"]==json!(["iq"]) {"iq"} else if request["kinds"]==json!(["results"]) {"results"} else if request["kinds"]==json!(["audio"]) {"audio"}
                    else {return Err("select one backend recording kind: iq, results or audio".into());};
                let uid=request["receiver_uid"].as_str().ok_or("receiver_uid required")?;
                let slot=state.receiver_slot(uid).ok_or("receiver no longer exists")?;
                let channel=state.channel_configs()[(slot-1) as usize].clone();
                let has_results=channel.processing_enabled && websa_channelizer::catalog::mode(&channel.mode)
                    .is_some_and(|mode|mode.outputs.iter().any(|kind|kind!="audio"));
                if kind=="audio" && (!channel.processing_enabled || state.receiver_failure.is_some()
                    || !websa_channelizer::catalog::mode(&channel.mode).is_some_and(|m|m.outputs.iter().any(|k|k=="audio"))) {
                    return Err("enable a healthy processor with published audio before recording".into());
                }
                if kind=="results" && (!has_results || state.receiver_failure.is_some()) {
                    return Err("enable a healthy processor before recording its published results".into());
                }
                if state.swp.mode!="sdr" || !state.link_up || state.ddc_error.is_some()
                    || !state.channel_configs()[(slot-1) as usize].enabled {return Err("enable a healthy IQ receiver before recording".into());}
                if !preview {
                    let service=state.receivers.boot.clone();
                    let pending=if kind=="iq" {state.recordings.as_mut().unwrap().start(uid,&service,&id)?}
                        else {state.recordings.as_mut().unwrap().start_kind(uid,&service,&id,kind)?};
                    state.receivers.recording_requests.insert(id.clone(),(request.clone(),pending.clone()));ticket=Some(pending);
                }
            } else {
                let task=request["task_id"].as_str().ok_or("task_id required")?;
                if !preview {state.recordings.as_mut().unwrap().stop(task)?;}
            }
            Ok(())
        })()};
        if preview {return json!({"accepted":result.is_ok(),"faults":result.err().map(|e|vec![fault("recording","RECORDING_REJECTED",&e)]).unwrap_or_default(),
            "capture_change":false,"affected_receivers":[],"affected_tracks":[],"candidate_center_hz":state.capture_center(),"estimated_cf32_bytes_per_second":0});}
        if let Some(ticket)=ticket {
            drop(state);result=ticket.wait();state=app.state.lock().unwrap();
            // Concurrent retries share the preparation ticket and the first committed receipt.
            if let Some((_,old,reply))=state.receivers.replies.iter().find(|(key,_,_)|key==&id) {
                if old==&request {return reply.clone();}
            }
            state.receivers.recording_requests.remove(&id);
        } else if state.receivers.recording_requests.contains_key(&id) {
            return json!({"request_id":id,"snapshot":state.receiver_snapshot(),"fault":fault("request","IDENTITY_CONFLICT","request_id reused with different command")});
        }
        let reply=json!({"request_id":id,"snapshot":state.receiver_snapshot(),"fault":result.err().map(|e|fault("recording","RECORDING_REJECTED",&e))});
        state.receivers.replies.push_back((id,request,reply.clone()));
        if state.receivers.replies.len()>128 {state.receivers.replies.pop_front();}
        reply
    }).await {Ok(value)=>Json(value).into_response(),Err(error)=>state_worker_failed(error).into_response()}
}

pub(super) fn auxiliary(frame:&[u8])->bool {
    frame.starts_with(b"IQAX") || (frame.starts_with(b"IQEN") && frame.len()==16
        && u32::from_le_bytes(frame[4..8].try_into().unwrap())>1)
}

/// Observe actual published events once, before subscriber snapshots or sequence translation.
/// Original processing/source identities survive asynchronous tails and slot reuse.
pub(super) fn record_result(state:&DevState,bytes:Bytes) {
    let Some(manager)=state.recordings.as_ref().filter(|r|r.wants_results()) else {return;};
    let message=Message::Binary(bytes.clone());
    let Some(key)=state.processing_source_identity(service_stream::message_identity(&message)) else {return;};
    let Some(known)=state.receivers.history.iter().find(|k|service_stream::identity_key(k.entry.channel_id,k.entry.epoch)==key) else {return;};
    let result=(||->Result<Bytes,String> {
        let mut envelope:Value=serde_json::from_slice(&bytes[8..]).map_err(|e|e.to_string())?;
        envelope["receiver_uid"]=json!(known.uid);envelope["service_id"]=json!(state.receivers.boot);
        envelope["baseband_stream"]=json!({"source_id":SOURCE,"channel_id":known.entry.channel_id.to_string(),"epoch":known.entry.epoch.to_string()});
        let row=json!({"version":1,"received_at_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis().to_string(),
            "source":{"baseband":known.baseband,"config_revision":known.revision.to_string(),"frequency_hz":known.entry.frequency_hz,
                "actual_rate_hz":known.entry.output_rate,"input_rate_hz":known.entry.input_rate},"envelope":envelope});
        serde_json::to_vec(&row).map(Bytes::from).map_err(|e|e.to_string())
    })();
    match result {Ok(bytes)=>manager.admit_result(&known.uid,bytes),Err(error)=>manager.processing_sources(&[],Some(&error))}
}

/// Actual packet configuration travels with PCM, so queued old frames never acquire
/// a newer shared gain/NR/mode configuration from the mutable control snapshot.
pub(super) fn record_audio(state:&DevState,bytes:Bytes,config:&ChannelConfig) {
    let Some(manager)=state.recordings.as_ref().filter(|r|r.wants_audio()) else {return;};
    let Some(key)=state.processing_source_identity(service_stream::message_identity(&Message::Binary(bytes.clone()))) else {return;};
    let Some(known)=state.receivers.history.iter().find(|k|service_stream::identity_key(k.entry.channel_id,k.entry.epoch)==key) else {return;};
    let source=json!({"receiver_uid":known.uid,"service_id":state.receivers.boot,
        "baseband_stream":{"source_id":SOURCE,"channel_id":known.entry.channel_id.to_string(),"epoch":known.entry.epoch.to_string()},
        "baseband":known.baseband,"baseband_revision":known.revision.to_string(),"config":config});
    if let Ok(source)=serde_json::to_vec(&source) {manager.admit_audio(&known.uid,recording::audio::Input {bytes,source:source.into()});}
}

pub(super) fn recording_processing_sources(state:&DevState) {
    if let Some(manager)=&state.recordings {
        let active=state.receivers.receivers.values().filter(|r|r.channel.enabled&&r.channel.processing_enabled
            && websa_channelizer::catalog::mode(&r.channel.mode).is_some_and(|m|m.outputs.iter().any(|kind|kind!="audio"))).map(|r|r.uid.clone()).collect::<Vec<_>>();
        manager.processing_sources(&active,state.receiver_failure.as_deref());
        let audio=state.receivers.receivers.values().filter(|r|r.channel.enabled&&r.channel.processing_enabled
            && websa_channelizer::catalog::mode(&r.channel.mode).is_some_and(|m|m.outputs.iter().any(|kind|kind=="audio"))).map(|r|r.uid.clone()).collect::<Vec<_>>();
        manager.processing_kind_sources("audio",&audio,state.receiver_failure.as_deref());
    }
}

/// Encode once at the transport boundary; consumers share Bytes and only inspect headers.
pub(super) fn tag_iq(state:&mut DevState,frames:Vec<Vec<u8>>)->Vec<(Bytes,Option<String>)> {
    state.remember_receiver_streams();
    frames.into_iter().map(|frame| {
        let key=if frame.starts_with(b"IQEN") && frame.len()==16 {
            Some((u32::from_le_bytes(frame[8..12].try_into().unwrap()),u32::from_le_bytes(frame[12..16].try_into().unwrap())))
        } else if frame.len()>=56 && matches!(frame.get(..4),Some(b"IQBF"|b"IQAX")) {
            Some((u32::from_le_bytes(frame[32..36].try_into().unwrap()),u32::from_le_bytes(frame[36..40].try_into().unwrap())))
        } else {None};
        let uid=key.and_then(|(id,epoch)|state.receivers.history.iter_mut()
            .find(|known|known.entry.channel_id==id && known.entry.epoch==epoch).map(|known| {
                if frame.len()>=56 && u32::from_le_bytes(frame[8..12].try_into().unwrap())==0 {
                    known.origin=u64::from_le_bytes(frame[40..48].try_into().unwrap());
                }
                if frame.len()>=56 {
                    let input=u64::from_le_bytes(frame[40..48].try_into().unwrap());
                    let rate=f64::from_le_bytes(frame[16..24].try_into().unwrap());
                    let delay=f64::from_le_bytes(frame[48..56].try_into().unwrap());
                    let step=(known.entry.input_rate/rate).round() as u64;
                    known.clock=Some(json!({"input_index":input.to_string(),
                        "output_index":(input.saturating_sub(known.origin)/step.max(1)).to_string(),
                        "fraction_num":"0","fraction_den":"1","input_rate_hz":known.entry.input_rate,
                        "actual_rate_hz":rate,"delay_seconds":delay/rate,"utc_reliable":false}));
                }
                known.uid.clone()
            }));
        let bytes=Bytes::from(frame);
        if let (Some(uid),Some(recordings))=(&uid,&state.recordings) {
            if bytes.starts_with(b"IQEN") {recordings.source_end(uid);}
            else if bytes.len()>=56 && u32::from_le_bytes(bytes[12..16].try_into().unwrap())>0 {
                if let Some((id,epoch))=key {
                    if let Some(known)=state.receivers.history.iter().find(|known|known.entry.channel_id==id&&known.entry.epoch==epoch) {
                        let read=|at|u32::from_le_bytes(bytes[at..at+4].try_into().unwrap());
                        let input=u64::from_le_bytes(bytes[40..48].try_into().unwrap());
                        let rate=f64::from_le_bytes(bytes[16..24].try_into().unwrap());
                        let delay=f64::from_le_bytes(bytes[48..56].try_into().unwrap());
                        let step=(known.entry.input_rate/rate).round() as u64;
                        recordings.admit(uid,recording::Input {bytes:bytes.clone(),meta:websa_channelizer::iq_archive::v2::Meta {
                            service_id:state.receivers.boot.clone(),segment_id:format!("{epoch}-{id}"),sequence:read(8).to_string(),
                            frequency_hz:f64::from_le_bytes(bytes[24..32].try_into().unwrap()),baseband:known.baseband.clone(),
                            clock:websa_channelizer::iq_archive::IqBlockMeta {receiver_uid:uid.clone(),source_id:SOURCE.into(),track_id:String::new(),
                                epoch:epoch.to_string(),channel_id:id.to_string(),config_revision:known.revision.to_string(),
                                first_input_index:input.to_string(),first_output_index:(input.saturating_sub(known.origin)/step.max(1)).to_string(),
                                fraction_num:"0".into(),fraction_den:"1".into(),samples:read(12),sample_format:"cf32_le".into(),
                                amplitude_scale:1.0,input_rate_hz:known.entry.input_rate,actual_rate_hz:rate,delay_seconds:Some(delay/rate)}
                        }});
                    }
                }
            }
        }
        (bytes,uid)
    }).collect()
}

pub(super) async fn iq_ws(ws:WebSocketUpgrade,State(app):State<Arc<App>>,
    Query(params):Query<HashMap<String,String>>,headers:HeaderMap)->Response {
    if !browser_access(&headers) {return StatusCode::FORBIDDEN.into_response();}
    let Some(uid)=params.get("receiver_uid").cloned() else {return StatusCode::BAD_REQUEST.into_response();};
    let mut state=app.state.lock().unwrap();
    let slot=state.receiver_slot(&uid);
    let Some(slot)=slot else {return StatusCode::NOT_FOUND.into_response();};
    if !state.channel_configs()[(slot-1) as usize].enabled {return StatusCode::CONFLICT.into_response();}
    drop(state);
    ws.on_upgrade(move|socket|client_loop(app,socket,slot==1,slot!=1,false,false,Some(slot),false,Some(uid)))
}

#[derive(Default)]
pub(super) struct IqSubscriber {active:Option<(u32,u32,u32)>,pub ended:bool}
impl IqSubscriber {
    pub(super) fn frames(&mut self,state:&mut DevState,uid:&str,frame:Message)->Result<Vec<Message>,String> {
        let Message::Binary(bytes)=frame else {return Ok(Vec::new());};
        if bytes.starts_with(b"IQEN") && bytes.len()==16 {
            let slot=u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            let id=u32::from_le_bytes(bytes[8..12].try_into().unwrap());
            let epoch=u32::from_le_bytes(bytes[12..16].try_into().unwrap());
            let known=state.iq_identity(id,epoch).ok_or("unknown retiring IQ identity")?;
            if known.uid!=uid || known.slot!=slot {return Err("retiring IQ identity mismatch".into());}
            self.ended=true;
            return Ok(vec![Message::Text(json!({"type":"stream_boundary","receiver_uid":uid,
                "previous":{"source_id":SOURCE,"channel_id":id.to_string(),"epoch":epoch.to_string()},
                "next":null,"clock":known.clock,"reason":"stopped","drained":true}).to_string().into())]);
        }
        if bytes.len()<56 {return Err("truncated IQ frame".into());}
        let read=|at|u32::from_le_bytes(bytes[at..at+4].try_into().unwrap());
        let (id,epoch,sequence)=(read(32),read(36),read(8));
        let known=state.iq_identity(id,epoch).ok_or("IQ stream identity no longer available")?;
        if known.uid!=uid {return Err("IQ logical receiver identity mismatch".into());}
        let mut output=Vec::new();
        if let Some((old_id,old_epoch,previous))=self.active {
            if (old_id,old_epoch)==(id,epoch) {
                if previous.checked_add(1)!=Some(sequence) {return Err("IQ source sequence discontinuity".into());}
            } else {
                output.push(Message::Text(json!({"type":"stream_boundary","receiver_uid":uid,
                    "previous":{"source_id":SOURCE,"channel_id":old_id.to_string(),"epoch":old_epoch.to_string()},
                    "next":{"source_id":SOURCE,"channel_id":id.to_string(),"epoch":epoch.to_string()},
                    "clock":known.clock,"reason":"reconfigured","drained":true}).to_string().into()));
            }
        }
        if self.active.is_none_or(|(old_id,old_epoch,_)|(old_id,old_epoch)!=(id,epoch)) {
            let input=u64::from_le_bytes(bytes[40..48].try_into().unwrap());
            let rate=f64::from_le_bytes(bytes[16..24].try_into().unwrap());
            let delay=f64::from_le_bytes(bytes[48..56].try_into().unwrap());
            let step=(known.entry.input_rate/rate).round() as u64;
            output.push(Message::Text(json!({"type":"stream_start","receiver_uid":uid,"source_id":SOURCE,
                "stream":{"source_id":SOURCE,"channel_id":id.to_string(),"epoch":epoch.to_string()},
                "format":"cf32_le","baseband":known.baseband,"first_sequence":sequence.to_string(),
                "clock":{"input_index":input.to_string(),"output_index":((input.saturating_sub(known.origin))/step.max(1)).to_string(),
                    "fraction_num":"0","fraction_den":"1","input_rate_hz":known.entry.input_rate,
                    "actual_rate_hz":rate,"delay_seconds":delay/rate,"utc_reliable":false}}).to_string().into()));
        }
        self.active=Some((id,epoch,sequence));
        output.push(Message::Binary(bytes));
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_equivalence_ignores_identity_and_iq_plans_but_keeps_processing_settings() {
        let first = ChannelConfig { freq: 100e6, ..ChannelConfig::default() };
        let mut copy = first.clone();
        copy.channel = 2; copy.enabled = false; copy.am_sync_generation = 8;
        copy.baseband = Some(BasebandConfig { passband_hz: 8000.0, transition_hz: 1000.0, minimum_rate_hz: 12000.0 });
        assert_eq!(audio_signature(&first), audio_signature(&copy));
        for field in ["freq", "if_bw", "pitch", "volume", "squelch", "deemph_us"] {
            let mut changed = json!(copy);
            changed[field] = json!(changed[field].as_f64().unwrap() + 1.0);
            let changed: ChannelConfig = serde_json::from_value(changed).unwrap();
            assert_ne!(audio_signature(&first), audio_signature(&changed), "{field}");
        }
        copy.nr = true;
        assert_ne!(audio_signature(&first), audio_signature(&copy));
    }

    fn app() -> Arc<App> {
        let (golden,schema)=load_golden();
        let mut state=DevState::new(golden,schema);
        state.swp.mode="sdr".into();state.sdr_center=100e6;state.sdr_listen=100e6;
        state.sdr_channels[0].enabled=false;
        Arc::new(App {state:StateMutex::new(state),clients:PLMutex::new(Vec::new()),next_client:AtomicU64::new(0),
            epoch:Arc::new(AtomicU64::new(0)),identities:Arc::new(std::array::from_fn(|_|AtomicU64::new(0))),
            display_version:Arc::new(AtomicU64::new(0))})
    }

    fn iq_config(frequency:f64)->Value {
        json!({"name":"Independent IQ","color":"#4cc99b","tags":[],"enabled":true,"purpose":"iq",
            "baseband":{"frequency_hz":frequency,"passband_hz":3000,"transition_hz":1000,"minimum_rate_hz":6000},"processors":[]})
    }

    async fn run(app:&Arc<App>,request:Value)->Value {
        let response=command(State(app.clone()),HeaderMap::new(),request.to_string()).await;
        assert_eq!(response.status(),StatusCode::OK);
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(),2*1024*1024).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn iq_configuration_during_decoder_failure_does_not_recover_or_restart_other_lanes() {
        let app=app();
        run(&app,json!({"type":"create","request_id":"first","config":iq_config(100e6)})).await;
        let created=run(&app,json!({"type":"create","request_id":"second","config":iq_config(100_010_000.0)})).await;
        let before=created["snapshot"]["receivers"].as_array().unwrap();
        app.state.lock().unwrap().receiver_failure=Some("decoder worker stopped".into());
        let mut config=before[1]["confirmed"].clone();config["baseband"]["frequency_hz"]=json!(100_011_000.0);
        let changed=run(&app,json!({"type":"configure","request_id":"iq-retune",
            "receiver_uid":before[1]["receiver_uid"],"expected_revision":before[1]["revision"],"config":config})).await;
        assert!(changed["fault"].is_null(),"{changed}");
        assert_eq!(changed["snapshot"]["receivers"][0],before[0]);
        let state=app.state.lock().unwrap();
        assert!(state.receiver_workers.is_none());
        assert_eq!(state.receiver_failure.as_deref(),Some("decoder worker stopped"));
    }

    #[tokio::test]
    async fn concurrent_retries_return_the_committed_receipt_once() {
        let app=app();
        let request=json!({"type":"create","request_id":"same","config":iq_config(100e6)});
        let (a,b)=tokio::join!(run(&app,request.clone()),run(&app,request));
        assert_eq!(a,b);assert!(a["fault"].is_null(),"{a}");
        assert_eq!(app.state.lock().unwrap().receivers.receivers.len(),1);
        let reply=run(&app,json!({"type":"create","request_id":"old-service","service_id":"previous-boot",
            "config":iq_config(100e6)})).await;
        assert!(!reply["fault"].is_null());
        assert_eq!(reply["snapshot"]["receivers"].as_array().unwrap().len(),1);
    }

    #[test]
    fn native_iq_byte_budget_and_fifo_failure_are_local_and_release_shared_storage() {
        let client=|id| {
            let (tx,rx)=mpsc::channel(64);
            (Client {id,tx,decode_pending_bytes:Arc::new(AtomicU64::new(0)),dropped:Arc::new(AtomicU64::new(0)),
                overflow:Arc::new(AtomicBool::new(false)),receiver_failed:Arc::new(AtomicBool::new(false)),
                wake:Arc::new(Notify::new()),epoch:Arc::new(AtomicU64::new(0)),status_revision:Arc::new(AtomicU64::new(0)),
                display_version:Arc::new(AtomicU64::new(0)),iq:true,aux:false,audio:false,decode:false,channel:Some(1),
                receiver_uid:Some("logical".into()),identities:Arc::new(std::array::from_fn(|_|AtomicU64::new(0))),
                latest:Arc::new(PLMutex::new((None,None)))},rx)
        };
        let (slow,mut pending)=client(1);let (healthy,mut live)=client(2);
        let bytes=Bytes::from(vec![0u8;2*1024*1024+1]);
        send_or_count(&slow,Message::Binary(bytes.clone()),0);
        send_or_count(&slow,Message::Binary(bytes.clone()),0);
        assert!(slow.overflow.load(Ordering::Acquire));
        send_or_count(&healthy,Message::Binary(bytes.clone()),0);
        assert!(!healthy.overflow.load(Ordering::Acquire));
        let packet=live.try_recv().unwrap();
        assert!(packet.epoch_current(1),"queued native IQ tails survive a capture revision");
        let Message::Binary(shared)=packet.frames().next().unwrap() else {panic!("IQ binary")};
        assert_eq!(shared.as_ptr(),bytes.as_ptr());
        drop(packet);assert_eq!(healthy.decode_pending_bytes.load(Ordering::Acquire),0);
        drop(pending.try_recv().unwrap());assert_eq!(slow.decode_pending_bytes.load(Ordering::Acquire),0);
    }

    #[tokio::test]
    async fn dynamic_uids_revision_admission_and_stop_preserve_other_receivers() {
        let app=app();
        let mut uids=Vec::new();
        for (index,frequency) in [100e6,100_010_000.0,100_020_000.0].into_iter().enumerate() {
            let request=json!({"type":"create","request_id":format!("create-{index}"),"config":iq_config(frequency)});
            let reply=run(&app,request.clone()).await;
            assert!(reply["fault"].is_null(),"{reply}");
            assert_eq!(run(&app,request).await,reply,"idempotent create changed identity");
            uids=reply["snapshot"]["receivers"].as_array().unwrap().iter()
                .map(|receiver|receiver["receiver_uid"].as_str().unwrap().to_owned()).collect();
        }
        assert_eq!(uids.len(),3);
        let full=run(&app,json!({"type":"create","request_id":"full","config":iq_config(100e6)})).await;
        assert!(!full["fault"].is_null());
        let before=app.state.lock().unwrap().receiver_snapshot();
        let mut invalid=before["receivers"][1]["confirmed"].clone();invalid["baseband"]["frequency_hz"]=json!(200e6);
        let rejected=run(&app,json!({"type":"configure","request_id":"invalid","receiver_uid":uids[1],
            "expected_revision":before["receivers"][1]["revision"],"config":invalid})).await;
        assert!(!rejected["fault"].is_null());
        assert_eq!(rejected["snapshot"],before);
        let mut renamed=before["receivers"][1]["confirmed"].clone();renamed["name"]=json!("Renamed");
        let changed=run(&app,json!({"type":"configure","request_id":"rename","receiver_uid":uids[1],
            "expected_revision":before["receivers"][1]["revision"],"config":renamed.clone()})).await;
        assert!(changed["fault"].is_null(),"{changed}");
        let late=run(&app,json!({"type":"configure","request_id":"late","receiver_uid":uids[1],
            "expected_revision":before["receivers"][1]["revision"],"config":renamed})).await;
        assert!(!late["fault"].is_null());
        let stopped=run(&app,json!({"type":"retire","request_id":"retire","receiver_uid":uids[1]})).await;
        assert!(stopped["fault"].is_null(),"{stopped}");
        assert_eq!(stopped["snapshot"]["receivers"].as_array().unwrap().len(),2);
        assert_eq!(stopped["snapshot"]["receivers"][0],changed["snapshot"]["receivers"][0]);
        let mut state=app.state.lock().unwrap();
        let frames=state.fixture_sources.as_mut().unwrap().step().unwrap();
        state.remember_receiver_streams();
        let active=state.stream_registry()[0].clone();
        let original=state.iq_identity(active.channel_id,active.epoch).unwrap();
        for index in 0..HISTORY_LIMIT+8 {
            let mut retired=original.clone();retired.entry.channel_id=u32::MAX-index as u32;
            retired.entry.epoch=u32::MAX-index as u32;state.receivers.history.push_back(retired);
        }
        state.remember_receiver_streams();
        assert!(state.receivers.history.len()<=HISTORY_LIMIT);
        assert_eq!(state.iq_identity(active.channel_id,active.epoch).unwrap().uid,original.uid);
        let tune=state.parse_channel_config(&json!({"channel":1,"freq":100_001_000.0})).unwrap();
        assert!(!tune.processing_enabled);assert!(tune.baseband.is_some());
        let legacy=state.parse_channel_config(&json!({"channel":1,"mode":"am","ifbw":6000})).unwrap();
        assert!(legacy.processing_enabled);assert!(legacy.baseband.is_none());
        let mut no_processor=tune;no_processor.mode="adsb".into();
        assert!(no_processor.validate().is_ok(),"IQ-only must not require a retained decoder/provider or its RF band");
        let registry=state.stream_registry();
        let processed=state.process_receiver_frames_with_registry(&frames,&registry,false).unwrap();
        assert!(processed.audio.is_empty()&&processed.decode.is_empty());
        assert!(state.receiver_workers.is_none(),"IQ-only created processing workers");
        assert!(state.channel_configs().iter().all(|c|!c.processing_enabled));
        drop(state);
        let reused=run(&app,json!({"type":"create","request_id":"reuse-slot","config":iq_config(100_030_000.0)})).await;
        assert!(reused["fault"].is_null(),"{reused}");
        assert_eq!(reused["snapshot"]["receivers"][0]["receiver_uid"],uids[0]);
        assert_eq!(reused["snapshot"]["receivers"][1]["display_number"],2);
        assert_ne!(reused["snapshot"]["receivers"][1]["receiver_uid"],uids[1],"reusing a slot must allocate a new logical UID");
    }

    #[tokio::test]
    async fn recording_start_receipt_waits_for_durable_files_and_concurrent_retries_share_one_task() {
        let app=app();
        let root=std::env::temp_dir().join(format!("websa-recording-api-{}",app.state.lock().unwrap().receivers.boot));
        {let mut state=app.state.lock().unwrap();state.link_up=true;state.recordings=Some(recording::Manager::open(root.clone()).unwrap());}
        let created=run(&app,json!({"type":"create","request_id":"record-create","config":iq_config(100e6)})).await;
        assert!(created["fault"].is_null(),"{created}");
        let uid=created["snapshot"]["receivers"][0]["receiver_uid"].as_str().unwrap();
        let command=json!({"type":"record_start","request_id":"durable-start","receiver_uid":uid,"kinds":["iq"]});
        let (first,second)=tokio::join!(run(&app,command.clone()),run(&app,command));
        assert!(first["fault"].is_null(),"{first}");assert_eq!(first,second);
        let tasks=first["snapshot"]["recordings"].as_array().unwrap();assert_eq!(tasks.len(),1);
        let id=tasks[0]["task_id"].as_str().unwrap();
        let manifest:Value=serde_json::from_reader(std::fs::File::open(root.join(id).join("task.json")).unwrap()).unwrap();
        assert_eq!(manifest["receiver_uid"],uid);assert_eq!(manifest["tracks"][0]["volumes"].as_array().unwrap().len(),1);
        assert_eq!(std::fs::read(root.join(id).join("iq-000000.wbiq")).unwrap(),b"WBIQ0002");
        assert!(root.join(id).join("iq-000000.jsonl").is_file());
        let mut manager=app.state.lock().unwrap().recordings.take().unwrap();manager.stop(id).unwrap();manager.shutdown();drop(manager);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_pfb_publication_records_the_same_cf32_blocks_and_drains_on_stop() {
        let app=app();let mut state=app.state.lock().unwrap();
        let configs:Vec<_>=(1..=3).map(|slot|ChannelConfig {channel:slot,enabled:slot==1,freq:100e6,
            processing_enabled:false,baseband:Some(BasebandConfig {passband_hz:3000.0,transition_hz:1000.0,minimum_rate_hz:6000.0}),
            ..ChannelConfig::default()}).collect();
        state.sdr_channels=configs.clone();
        state.ddc=Some(SendDdc(Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&configs).unwrap()));
        let snapshot=state.receiver_snapshot();let uid=snapshot["receivers"][0]["receiver_uid"].as_str().unwrap().to_owned();
        let root=std::env::temp_dir().join(format!("websa-native-record-{}",snapshot["service_id"].as_str().unwrap()));
        let mut manager=recording::Manager::open(root.clone()).unwrap();
        manager.start(&uid,snapshot["service_id"].as_str().unwrap(),"native-record").unwrap().wait().unwrap();state.recordings=Some(manager);
        let first=state.receiver_snapshot();let second=state.receiver_snapshot();
        assert_eq!(first["revision"],second["revision"]);
        assert!(second["state_revision"].as_str().unwrap().parse::<u64>().unwrap()>first["state_revision"].as_str().unwrap().parse::<u64>().unwrap());
        let mut expected=Vec::new();
        for _ in 0..256 {
            let frames=state.ddc.as_mut().unwrap().0.packet().unwrap();
            for (frame,_) in tag_iq(&mut state,frames) {if frame.len()>56 {expected.push(frame);}}
        }
        let mut tail=Vec::new();
        let ddc=&mut state.ddc.as_mut().unwrap().0;ddc.stream.flush_pending().unwrap();tail.extend(ddc.drain().unwrap());ddc.finish_framers(&mut tail).unwrap();
        for (frame,_) in tag_iq(&mut state,tail) {if frame.len()>56 {expected.push(frame);}}
        let mut manager=state.recordings.take().unwrap();let id=manager.snapshots()[0]["task_id"].as_str().unwrap().to_owned();
        manager.stop(&id).unwrap();drop(state);manager.shutdown();
        let saved=&manager.snapshots()[0];assert_eq!(saved["state"],"completed","{saved}");
        let mut count=0;
        let result=websa_channelizer::iq_archive::v2::scan(&mut std::fs::File::open(root.join(id).join("iq-000000.wbiq")).unwrap(),|index,raw| {
            let frame=&expected[count];assert_eq!(raw,&frame[56..]);
            assert_eq!(index.meta.clock.first_input_index,u64::from_le_bytes(frame[40..48].try_into().unwrap()).to_string());
            assert_eq!(index.meta.clock.input_rate_hz,IQ_FS);assert_eq!(index.meta.clock.actual_rate_hz,COARSE_RATE/162.0);
            assert_eq!(index.meta.clock.receiver_uid,uid);count+=1;Ok(())
        }).unwrap();assert!(result.complete);assert_eq!(count,expected.len());assert!(count>0);
        assert_eq!(saved["accepted_bytes"],saved["tracks"][0]["raw_bytes"]);
        drop(manager);std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_iq_plans_run_pfb_and_keep_unaffected_samples_across_cross_bin_retune() {
        let mut configs:Vec<_>=(1..=3).map(|slot|ChannelConfig {channel:slot,enabled:true,freq:100e6,
            processing_enabled:false,baseband:Some(BasebandConfig {passband_hz:3000.0,transition_hz:1000.0,minimum_rate_hz:6000.0}),
            ..ChannelConfig::default()}).collect();
        let mut live=Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&configs).unwrap();
        let mut reference=Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&configs).unwrap();
        let primary=live.channel_registry().iter().find(|r|r.channel==1).unwrap().clone();
        assert_eq!(primary.output_rate,COARSE_RATE/162.0);
        assert_eq!(primary.input_rate,IQ_FS);
        let mut frames=Vec::new();let mut expected=Vec::new();
        for _ in 0..128 {frames.extend(live.packet().unwrap());expected.extend(reference.packet().unwrap());}
        let old=live.channel_registry().iter().find(|r|r.channel==3).unwrap().channel_id;
        configs[2].freq=100e6+BIN_SPACING;
        service_cost::admit_service(&configs,IQ_FS,16,live.stream.bins().len()+1).unwrap();
        frames.extend(live.configure_service_channel(configs[2].clone()).unwrap());
        let registry=live.channel_registry();
        assert_eq!(registry.iter().find(|r|r.channel==1).unwrap().channel_id,primary.channel_id);
        let next=registry.iter().find(|r|r.channel==3).unwrap().channel_id;
        assert_ne!(old,next);
        for _ in 0..128 {frames.extend(live.packet().unwrap());expected.extend(reference.packet().unwrap());}
        live.stream.flush_pending().unwrap();frames.extend(live.drain().unwrap());live.finish_framers(&mut frames).unwrap();
        reference.stream.flush_pending().unwrap();expected.extend(reference.drain().unwrap());reference.finish_framers(&mut expected).unwrap();
        let payload=|frames:&[Vec<u8>],id:u32|frames.iter().filter(|f|f.len()>=56&&u32::from_le_bytes(f[32..36].try_into().unwrap())==id)
            .flat_map(|f|f[56..].iter().copied()).collect::<Vec<_>>();
        assert_eq!(payload(&frames,primary.channel_id),payload(&expected,reference.channel_registry()[0].channel_id));
        let actual=payload(&frames,primary.channel_id);
        assert!(!actual.is_empty());assert!(actual.chunks_exact(4).map(|v|f32::from_le_bytes(v.try_into().unwrap())).any(|v|v.abs()>0.0001));
        let last_old=frames.iter().rposition(|f|f.len()>=56&&u32::from_le_bytes(f[32..36].try_into().unwrap())==old).unwrap();
        let first_next=frames.iter().position(|f|f.len()>=56&&u32::from_le_bytes(f[32..36].try_into().unwrap())==next).unwrap();
        assert!(last_old<first_next,"new identity overtook old tail");
        let original=live.channel_registry();let mut invalid=configs[2].clone();invalid.baseband.as_mut().unwrap().transition_hz=1e-300;
        assert!(live.configure_service_channel(invalid).is_err());
        assert_eq!(live.channel_registry()[0].channel_id,original[0].channel_id);
    }

    #[test]
    fn processed_output_keeps_uid_and_original_iq_mapping() {
        let app=app();let mut state=app.state.lock().unwrap();
        state.sdr_channels[0].enabled=true;
        state.sdr_channels[0].baseband=Some(BasebandConfig {passband_hz:16000.0,transition_hz:1000.0,minimum_rate_hz:48000.0});
        state.fixture_sources=Some(service_fixture::FixtureSources::new());
        let configs=state.channel_configs();state.fixture_sources.as_mut().unwrap().configure(&configs).unwrap();
        let registry=state.stream_registry();let snapshot=state.receiver_snapshot();
        let uid=snapshot["receivers"][0]["receiver_uid"].as_str().unwrap().to_owned();
        let iq_stream=snapshot["receivers"][0]["stream"].clone();
        let frames=state.fixture_sources.as_mut().unwrap().step().unwrap();
        let output=state.process_receiver_frames_with_registry(&frames,&registry,true).unwrap();
        assert!(!output.audio.is_empty());assert!(!output.decode.is_empty());
        for frame in output.decode {
            assert!(processing_frame(&mut state,"retired-or-other-uid",Message::Binary(frame.clone().into())).unwrap().is_none());
            let Some(Message::Binary(frame))=processing_frame(&mut state,&uid,Message::Binary(frame.into())).unwrap() else {panic!("UID output lost")};
            let envelope:Value=serde_json::from_slice(&frame[8..]).unwrap();
            assert_eq!(envelope["receiver_uid"],uid);assert_eq!(envelope["service_id"],snapshot["service_id"]);
            assert_eq!(envelope["baseband_stream"],iq_stream);
            assert_ne!(envelope["epoch"].as_u64().unwrap().to_string(),iq_stream["epoch"].as_str().unwrap());
        }
    }

    #[test]
    fn processing_changes_keep_real_iq_and_publish_distinct_processing_boundaries() {
        let mut configs:Vec<_>=(1..=3).map(|slot|ChannelConfig {channel:slot,enabled:true,freq:100e6,
            processing_enabled:false,baseband:Some(BasebandConfig {passband_hz:16000.0,transition_hz:1000.0,minimum_rate_hz:48000.0}),
            ..ChannelConfig::default()}).collect();
        let mut live=Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&configs).unwrap();
        let mut reference=Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&configs).unwrap();
        let source=live.channel_registry()[0].clone();
        let mut workers=receiver_workers::ReceiverWorkers::new().unwrap();
        let mut branches=ProcessingBranches::default();
        let mut ids=Vec::new();let mut actual=Vec::new();let mut expected=Vec::new();
        for mode in [None,Some("am"),Some("usb"),None,Some("usb")] {
            configs[0].processing_enabled=mode.is_some();
            if let Some(mode)=mode {configs[0].mode=mode.into();configs[0].if_bw=if mode=="usb" {2400.0} else {6000.0};}
            let old=workers.retire_channels(&[1]).unwrap();
            assert!(old.decode.iter().all(|frame|serde_json::from_slice::<Value>(&frame[8..]).unwrap()["event"]["type"]!="start"));
            workers.configure(&configs).unwrap();
            if mode.is_none() {branches.retire_channel(1);}
            assert!(live.configure_service_channel(configs[0].clone()).unwrap().is_empty());
            assert_eq!(live.channel_registry()[0].channel_id,source.channel_id);
            assert_eq!(live.channel_registry()[0].epoch,source.epoch);
            for _ in 0..512 {
                let frames=live.packet().unwrap();expected.extend(reference.packet().unwrap());
                for frame in &frames {
                    if mode.is_none() || u32::from_le_bytes(frame[32..36].try_into().unwrap())!=source.channel_id {continue;}
                    let (output,seq,start)=branches.input(frame,&source,&configs[0]).unwrap();
                    if ids.last()!=Some(&output.epoch) {ids.push(output.epoch);}
                    if start {let mut marker=frame[..56].to_vec();marker[8..16].fill(0);
                        workers.submit_processing(&marker,std::slice::from_ref(&source),output.channel_id,output.epoch,0).unwrap();}
                    workers.submit_processing(frame,std::slice::from_ref(&source),output.channel_id,output.epoch,seq).unwrap();
                    let decoded=workers.finish().unwrap();
                    for audio in decoded.audio {assert_eq!(u32::from_le_bytes(audio[36..40].try_into().unwrap()),output.epoch);}
                    for event in decoded.decode {let event:Value=serde_json::from_slice(&event[8..]).unwrap();assert_eq!(event["epoch"],output.epoch);}
                }
                actual.extend(frames);
            }
        }
        assert_eq!(ids.len(),3);assert!(ids.windows(2).all(|pair|pair[0]<pair[1]));
        assert!(live.restart_service_receivers().unwrap().is_empty(),"decoder recovery restarted independent IQ");
        assert_eq!(live.channel_registry()[0].channel_id,source.channel_id);
        live.stream.flush_pending().unwrap();actual.extend(live.drain().unwrap());live.finish_framers(&mut actual).unwrap();
        reference.stream.flush_pending().unwrap();expected.extend(reference.drain().unwrap());reference.finish_framers(&mut expected).unwrap();
        for (entry,reference_entry) in live.channel_registry().iter().zip(reference.channel_registry().iter()) {
            let payload=|frames:&[Vec<u8>],id:u32|frames.iter().filter(|f|u32::from_le_bytes(f[32..36].try_into().unwrap())==id)
                .flat_map(|frame|frame[..32].iter().chain(frame[40..].iter()).copied()).collect::<Vec<_>>();
            assert_eq!(payload(&actual,entry.channel_id),payload(&expected,reference_entry.channel_id),"processor change altered IQ bytes, clock or sequence");
        }
    }

    #[test]
    fn iq_metadata_keeps_u64_and_fraction_delay_without_decoding_samples() {
        let app=app();let mut state=app.state.lock().unwrap();
        state.sdr_channels[0].enabled=true;state.sdr_channels[0].processing_enabled=false;
        state.fixture_sources=Some(service_fixture::FixtureSources::new());
        let configs=state.channel_configs();state.fixture_sources.as_mut().unwrap().configure(&configs).unwrap();
        let entry=state.stream_registry()[0].clone();let start=(1u64<<53)+321;
        let frame=websa_channelizer::encode_baseband(websa_channelizer::BasebandMagic::Primary,0,
            entry.output_rate,entry.frequency_hz,entry.channel_id as u64,entry.epoch as u64,start,1.25,&[]).unwrap();
        let tagged=tag_iq(&mut state,vec![frame]);let uid=tagged[0].1.clone().unwrap();
        let mut subscriber=IqSubscriber::default();
        let output=subscriber.frames(&mut state,&uid,Message::Binary(tagged[0].0.clone())).unwrap();
        let Message::Text(text)=&output[0] else {panic!("metadata absent")};
        let meta:Value=serde_json::from_str(text).unwrap();
        assert_eq!(meta["clock"]["input_index"],start.to_string());
        assert_eq!(meta["clock"]["fraction_num"],"0");
        assert_eq!(meta["clock"]["delay_seconds"],1.25/entry.output_rate);
        assert!(matches!(&output[1],Message::Binary(bytes) if bytes.as_ptr()==tagged[0].0.as_ptr()));
    }
}
