//! P3 logical receiver API. Slots are a private bridge to the existing three-lane service.
use super::*;
use receiver_workers::{BasebandConfig, ChannelConfig};
use std::collections::VecDeque;

const SOURCE: &str = "primary";
const MAX_RECEIVERS: usize = 3;
const HISTORY_LIMIT: usize = 256;

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
}

pub(super) struct Directory {
    boot: String,
    next: u64,
    receivers: BTreeMap<u32, LogicalReceiver>,
    history: VecDeque<IqIdentity>,
    replies: VecDeque<(String, Value, Value)>,
}

impl Default for Directory {
    fn default() -> Self {
        Self { boot: format!("{}-{}",std::process::id(),std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()), next:0,
            receivers:BTreeMap::new(),history:VecDeque::new(),replies:VecDeque::new() }
    }
}

fn config_value(channel: &ChannelConfig, previous: Option<&Value>) -> Value {
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
        "mode":channel.mode,"minimum_rate_hz":plan.map(|p|p.output_rate).unwrap_or(1.0)
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
                        entry:entry.clone(),origin:0,baseband:receiver.config["baseband"].clone(),clock:None});
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
                "state":if !receiver.channel.enabled {"stopped"} else if capture_fault.is_some() {"failed"}
                    else if entry.is_some() {"running"} else {"interrupted"},
                "stream":entry.map(|e|json!({"source_id":SOURCE,"epoch":e.epoch.to_string(),"channel_id":e.channel_id.to_string()})),
                "clock":entry.and_then(|e|self.receivers.history.iter().find(|known|
                    known.entry.channel_id==e.channel_id&&known.entry.epoch==e.epoch)).and_then(|known|known.clock.clone()),
                "fault":capture_fault.map(|error|fault("capture","CAPTURE_FAILED",error))})
        }).collect();
        json!({"contract":"multichannel-draft-v1","transport":"native","service_id":self.receivers.boot,
            "revision":self.config_version.to_string(),
            "capabilities":{"max_channels":MAX_RECEIVERS,"capacity_target":128,
                "modes":websa_channelizer::catalog::manifest().iter().filter_map(|m|m["id"].as_str()).collect::<Vec<_>>(),
                "commands":["create","configure","retire","output"],"recording":false,"monitor_mixing":false},
            "capture":{"source_id":SOURCE,"center_hz":self.capture_center(),
                "usable_bandwidth_hz":self.capture_bandwidth(),"input_rate_hz":self.pipeline.as_ref().map(|p|p.snapshot.rate)
                    .or_else(||self.ddc.as_ref().map(|d|d.0.stream.input_sample_rate())).unwrap_or(self.capture_bandwidth()/0.8),
                "epoch":self.pipeline.as_ref().map(|p|p.snapshot.generation)
                    .or_else(||self.ddc.as_ref().map(|d|d.0.generation)).unwrap_or(0).to_string(),
                "source_kind":if self.hardware_shim.is_some() {"hardware"} else {"fixture"},
                "state":if capture_fault.is_some() || self.swp.mode!="sdr" {"failed"} else {"running"}},
            "receivers":receivers,"outputs":[],"recordings":[]})
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
        else {return Err("command is not available in P3".into());};
    if kind=="retire" {config["enabled"]=json!(false);}
    if config["name"].as_str().is_none_or(|v|v.is_empty()||v.len()>128)
        || config["color"].as_str().is_none_or(|v|v.len()!=7||!v.starts_with('#')||!v[1..].bytes().all(|c|c.is_ascii_hexdigit()))
        || config["tags"].as_array().is_none_or(|tags|tags.len()>32||tags.iter().any(|t|t.as_str().is_none_or(|v|v.len()>64))) {
        return Err("invalid receiver name, color or tags".into());
    }
    let enabled=config["enabled"].as_bool().ok_or("enabled must be boolean")?;
    let purpose=config["purpose"].as_str().ok_or("purpose missing")?;
    if !matches!(purpose,"iq"|"listen"|"digital") {return Err("unsupported receiver purpose".into());}
    let bb=&config["baseband"];
    let frequency=bb["frequency_hz"].as_f64().filter(|v|v.is_finite()&&*v>0.0).ok_or("frequency_hz must be finite and positive")?;
    let baseband=BasebandConfig {passband_hz:bb["passband_hz"].as_f64().ok_or("passband_hz missing")?,
        transition_hz:bb["transition_hz"].as_f64().ok_or("transition_hz missing")?,
        minimum_rate_hz:bb["minimum_rate_hz"].as_f64().ok_or("minimum_rate_hz missing")?};
    let processors=config["processors"].as_array().ok_or("processors must be an array")?;
    if processors.len()>1 || (purpose=="iq" && !processors.is_empty()) || (purpose!="iq" && processors.is_empty()) {
        return Err("P3 supports zero or one processor; IQ-only receivers must use an empty set".into());
    }
    let mut channel=state.channel_configs()[(slot-1) as usize].clone();
    channel.freq=frequency;channel.enabled=enabled;channel.processing_enabled=!processors.is_empty();
    channel.baseband=Some(baseband);
    if let Some(processor)=processors.first() {
        if !matches!(processor["kind"].as_str(),Some("demodulator"|"decoder")) {return Err("unsupported processor kind".into());}
        channel.mode=processor["mode"].as_str().ok_or("processor mode missing")?.into();
        let descriptor=websa_channelizer::catalog::mode(&channel.mode).ok_or("unsupported processor mode")?;
        channel.if_bw=descriptor.default_bandwidth_hz;
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
        "processing_enabled":channel.processing_enabled,"request_id":request["request_id"]});
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

pub(super) fn auxiliary(frame:&[u8])->bool {
    frame.starts_with(b"IQAX") || (frame.starts_with(b"IQEN") && frame.len()==16
        && u32::from_le_bytes(frame[4..8].try_into().unwrap())>1)
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
        (Bytes::from(frame),uid)
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
