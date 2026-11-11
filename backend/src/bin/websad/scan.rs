//! Bounded live observations. The acquisition tap never waits for analysis.
//! Logical receivers and the one temporary lease share the existing DDC owner.
use super::*;
use std::{collections::VecDeque, sync::mpsc, thread, time::Instant};
use serde::Serialize;
use rustfft::{num_complex::Complex32, FftPlanner};
use receiver_workers::{BasebandConfig, ChannelConfig};
const FFT:usize=65_536;
const BLOCK:usize=FFT*5/2;
const MAX_SIGNALS:usize=256;
const MAX_IQ:usize=1_048_576; // complex f32: 8 MiB hard ceiling

// Scale is calibration metadata, not source identity. The SDK can refresh it
// without a tune, sample gap or new capture. Assemble each PSD at one scale,
// but do not cancel an independent IQ identification when calibration changes.
fn same_capture(a:display_fft::Meta,b:display_fft::Meta)->bool {
    a.epoch==b.epoch && a.generation==b.generation && a.center==b.center
        && a.rate==b.rate && a.bandwidth==b.bandwidth
}

#[derive(Clone,Serialize)]
struct Signal {
    id:String, generation:String, center_hz:f64, bandwidth_hz:f64, analysis_bandwidth_hz:f64, low_hz:f64, high_hz:f64,
    excess_db:f64, power_v2_db:f64, activity:String, observed_hit_ratio:f64,
    label:String, verdict:String, identify_state:String, suggested_mode:Option<String>,
    protocol:Option<String>, receiver_available:bool,decoder_available:bool, receiver_uid:Option<String>,iq_receiver_uid:Option<String>, flags:Vec<String>,
    source:String, data_source:&'static str, sampled_at_ms:Option<String>, evidence:Value,
    #[serde(skip)] stable:bool, #[serde(skip)] hits:u64, #[serde(skip)] observations:u64, #[serde(skip)] history:u8,
    #[serde(skip)] last_seen:Option<Instant>, #[serde(skip)] created:Option<Instant>,
}
impl Signal {
    fn advance_generation(&mut self){self.generation=self.generation.parse::<u64>().unwrap_or(0).saturating_add(1).to_string();}
    fn accepts_protocol(&self,running:bool)->bool {self.activity!="stale"&&self.activity!="ignored"&&(running||matches!(self.identify_state.as_str(),"queued"|"waiting"|"sampling"|"analyzing"))}
    fn new(id:u64,lo:f64,hi:f64,source:&str)->Self {
        Self{id:format!("s{id}"),generation:"1".into(),center_hz:(lo+hi)/2.0,bandwidth_hz:hi-lo,analysis_bandwidth_hz:hi-lo,low_hz:lo,high_hz:hi,
            excess_db:0.0,power_v2_db:-300.0,activity:"short".into(),observed_hit_ratio:0.0,
            label:"unknown".into(),verdict:"unknown".into(),identify_state:"idle".into(),suggested_mode:None,
            protocol:None,receiver_available:false,decoder_available:false,receiver_uid:None,iq_receiver_uid:None,flags:vec![],source:source.into(),data_source:"live_iq",
            sampled_at_ms:None,evidence:json!({}),stable:false,hits:0,observations:0,history:0,last_seen:None,created:Some(Instant::now())}
    }
}
#[derive(Clone)]
struct Region {lo:f64,hi:f64,origin:String,token:u64}
struct Collector {id:String,token:u64,entry:sdr_pipeline::ChannelRegistryEntry,iq:Vec<f32>,
    next_seq:Option<u32>,next_index:Option<u64>,first_index:Option<u64>,started:Instant,at:String}
struct FeatureJob {id:String,token:u64,rate:f64,offset:f64,bandwidth:f64,iq:Vec<f32>,at:String,source:Value}
struct RawJob {job:display_fft::Job,token:u64}
#[derive(Default)]
struct Metrics {observations:u64,skipped:u64,observed_seconds:f64,cpu_seconds:f64,feature_cpu_seconds:f64,last_ms:f64,last_at:Option<String>,started:Option<Instant>,rate_hz:f64,overloads:u64,scale_updates:u64,iq_restarts:u64}
struct Data {
    capture:Value, signature:Value, meta:Option<display_fft::Meta>,token:u64,revision:u64,next:u64,
    tap_attached:bool,running:bool,state:String,range:Option<(f64,f64)>,channels:Vec<(f64,f64)>,auto:bool,threshold:f64,
    signals:Vec<Signal>,region:Option<Region>,queue:VecDeque<String>,collector:Option<Collector>,
    metrics:Metrics,background_reliable:bool,truncated:usize,reason:Option<String>,
    replies:VecDeque<(String,Value,Value)>,
    next_schedule:Instant,next_owner_tick:Instant,
    pressure_until:Option<Instant>,pressure_ticks:u32,last_queue_full:u64,
}
impl Default for Data {
    fn default()->Self {Self{capture:json!({}),signature:Value::Null,meta:None,token:1,revision:0,next:0,tap_attached:false,running:false,state:"stopped".into(),range:None,channels:vec![],auto:true,threshold:8.0,signals:vec![],region:None,queue:VecDeque::new(),collector:None,metrics:Metrics{rate_hz:5.0,..Metrics::default()},background_reliable:false,truncated:0,reason:None,replies:VecDeque::new(),next_schedule:Instant::now(),next_owner_tick:Instant::now(),pressure_until:None,pressure_ticks:0,last_queue_full:0}}
}
impl Data {
    fn cancel(&mut self){self.token+=1;self.queue.clear();self.collector=None;self.region=None;
        for signal in &mut self.signals {if matches!(signal.identify_state.as_str(),"detecting"|"sampling"|"queued"|"waiting"|"analyzing") {signal.identify_state="cancelled".into();}}}
    fn enqueue(&mut self,id:&str,front:bool)->Result<(),String> {
        if front {self.next_schedule=Instant::now();}
        if self.collector.as_ref().is_some_and(|c|c.id==id) {return Ok(());}
        self.queue.retain(|known|known!=id);
        if self.queue.len()>=32 {if front {if let Some(id)=self.queue.pop_back(){if let Some(s)=self.signals.iter_mut().find(|s|s.id==id){s.identify_state="budget_skipped".into();}}} else {return Err("identify queue full".into());}}
        if front {self.queue.push_front(id.into());} else {self.queue.push_back(id.into());}
        if let Some(signal)=self.signals.iter_mut().find(|s|s.id==id) {signal.identify_state="queued".into();} Ok(())
    }
    fn snapshot(&self,service:&str,probe:bool)->Value {
        let rows:Vec<Value>=self.signals.iter().filter(|s|s.source=="region").chain(self.signals.iter().filter(|s|s.source!="region"&&(s.stable||s.history.count_ones()>=2||s.source=="channel"))).take(128).map(|s|{let mut row=json!(s);row.as_object_mut().unwrap().remove("evidence");row}).collect();
        json!({"service_id":service,"scan_id":format!("scan-{service}"),"revision":self.revision.to_string(),
            "capture":self.capture,"running":self.running,"state":self.state,"range":self.range.map(|(lo,hi)|json!({"low_hz":lo,"high_hz":hi})),
            "fft_size":FFT,"bin_spacing_hz":self.meta.map(|m|m.rate/FFT as f64),"background_reliable":self.background_reliable,
            "signals":rows,"truncated":self.truncated+self.signals.len().saturating_sub(128),"reason":self.reason,
            "probe_slots_used":usize::from(probe),"identify_queued":self.queue.len(),"auto_identify":self.auto,
            "statistics":{"observations":self.metrics.observations,"skipped":self.metrics.skipped,
                "observed_seconds":self.metrics.observed_seconds,"analysis_cpu_seconds":self.metrics.cpu_seconds,
                "feature_cpu_seconds":self.metrics.feature_cpu_seconds,"last_processing_ms":self.metrics.last_ms,
                "last_observed_at_ms":self.metrics.last_at,"rate_hz":self.metrics.rate_hz,"overloads":self.metrics.overloads,
                "elapsed_seconds":self.metrics.started.map(|t|t.elapsed().as_secs_f64()),"scale_updates":self.metrics.scale_updates,"iq_restarts":self.metrics.iq_restarts,"coverage":"intermittent"}})
    }
}

pub(super) struct Engine {data:Arc<PLMutex<Data>>,raw:mpsc::SyncSender<RawJob>,features:mpsc::SyncSender<FeatureJob>,skipped:AtomicU64}
impl Engine {
    fn new()->Arc<Self> {
        let data=Arc::new(PLMutex::new(Data::default()));
        let (raw,rx)=mpsc::sync_channel::<RawJob>(1); let (features,fx)=mpsc::sync_channel::<FeatureJob>(1);
        let state=data.clone();
        thread::Builder::new().name("sdr-scan-psd".into()).spawn(move||{
            websa_channelizer::capture_scheduling::normal();
            let mut psd=Psd::new(FFT);
            while let Ok(work)=rx.recv() {
                let cpu=receiver_workers::cpu_now();let start=Instant::now();
                let config={let d=state.lock();if d.token!=work.token {continue;}(d.running,d.range,d.channels.clone(),d.region.clone().filter(|r|r.token==d.token),d.threshold)};
                let spectrum=psd.compute(&work.job.iq,work.job.meta.rate,work.job.meta.scale);
                let floor=psd.floor(work.job.meta,&spectrum);
                let observation=prepare_observation(work.job.meta,&spectrum,config.0,config.1,&config.2,config.3,config.4,Some(&floor));
                let cost=cpu.zip(receiver_workers::cpu_now()).map_or(start.elapsed().as_secs_f64(),|(a,b)|b.saturating_sub(a) as f64/1e9);
                let mut d=state.lock();if d.token!=work.token {continue;}
                d.metrics.cpu_seconds+=cost;d.metrics.last_ms=start.elapsed().as_secs_f64()*1000.0;
                // A duty-cycle CPU budget and bounded queue protect existing consumers.
                d.metrics.rate_hz=if cost>0.035 || d.pressure_until.is_some() {2.0} else {5.0};
                if cost>0.1 {d.reason=Some("analysis_budget".into());d.metrics.rate_hz=0.5;}
                d.apply_observation(work.job.meta,observation);
            }
        }).expect("scan worker");
        let state=data.clone();
        thread::Builder::new().name("sdr-scan-identify".into()).spawn(move||{
            websa_channelizer::capture_scheduling::normal();
            while let Ok(job)=fx.recv() {
                if state.lock().token!=job.token {continue;}
                let cpu=receiver_workers::cpu_now();let start=Instant::now();
                let mut result=match isolate_region(&job.iq,job.rate,job.offset,job.bandwidth) {
                    Ok((iq,rate,delay))=>{let mut result=features_iq(&iq,rate,0.0);result.evidence["additional_filter_delay_seconds"]=json!(delay);result},
                    Err(reason)=>Features{label:"unknown",verdict:"unknown",mode:None,evidence:json!({"reason":reason})}
                };result.evidence["source"]=job.source;
                let cost=cpu.zip(receiver_workers::cpu_now()).map_or(start.elapsed().as_secs_f64(),|(a,b)|b.saturating_sub(a) as f64/1e9);
                let mut d=state.lock();if d.token!=job.token {continue;}
                d.metrics.feature_cpu_seconds+=cost;
                d.next_schedule=Instant::now()+Duration::from_secs_f64(cost*10.0);
                if let Some(signal)=d.signals.iter_mut().find(|s|s.id==job.id && s.identify_state=="analyzing") {
                    if let Some(hz)=refined_center(signal,&result) {
                        // Preserve a subset of the original analysis window on
                        // any later retry; never expand a user's selected band.
                        signal.analysis_bandwidth_hz=(signal.analysis_bandwidth_hz-2.0*(hz-signal.center_hz).abs()).max(50.0);
                        signal.center_hz=hz;signal.advance_generation();
                    }
                    signal.label=result.label.into();if signal.protocol.is_none(){signal.verdict=result.verdict.into();}
                    if signal.protocol.is_none(){let next=result.mode.map(str::to_owned);if signal.suggested_mode!=next {signal.advance_generation();}signal.suggested_mode=next;}
                    if signal.protocol.is_none(){signal.receiver_available=result.mode.and_then(websa_channelizer::catalog::mode).is_some_and(|m|m.available);signal.decoder_available=false;}
                    let protocol_evidence=signal.evidence.get("protocol_evidence").cloned();signal.evidence=result.evidence;if signal.protocol.is_some(){if let Some(evidence)=protocol_evidence {signal.evidence["protocol_evidence"]=evidence;}}signal.identify_state="complete".into();signal.sampled_at_ms=Some(job.at);
                }
            }
        }).expect("identify worker");
        Arc::new(Self{data,raw,features,skipped:AtomicU64::new(0)})
    }
    pub fn iq(&self,frames:&[IqFrame]) {
        let Some(mut d)=self.data.try_lock() else {return;};
        let Some(mut c)=d.collector.take() else {return;};
        for frame in frames {
            let h=frame.header();if h.len()<56 {continue;}
            let id=u32::from_le_bytes(h[32..36].try_into().unwrap());let epoch=u32::from_le_bytes(h[36..40].try_into().unwrap());
            if id!=c.entry.channel_id || epoch!=c.entry.epoch {continue;}
            let seq=u32::from_le_bytes(h[8..12].try_into().unwrap());if seq==0 {continue;}
            let index=u64::from_le_bytes(h[40..48].try_into().unwrap());
            if c.next_seq.is_some_and(|n|n!=seq)||c.next_index.is_some_and(|n|n.abs_diff(index)>1) {
                // Only the copied analysis stream may have missed a frame.
                // Retry from this frame within the original two-second lease;
                // never join samples across the gap or reset the deadline.
                c.iq.clear();c.first_index=None;c.at=recording::millis();d.metrics.iq_restarts+=1;
            }
            c.first_index.get_or_insert(index);
            let Some(native)=frame.native_iq() else {continue;};
            let take=(2*MAX_IQ-c.iq.len()).min(native.samples.len());c.iq.extend_from_slice(&native.samples[..take]);
            c.next_seq=seq.checked_add(1);
            // Source index counts input samples, not output samples.
            c.next_index=Some(index+(native.samples.len()/2) as u64*(c.entry.input_rate/c.entry.output_rate).round() as u64);
            let target=(c.entry.output_rate*0.8) as usize;
            if c.iq.len()/2>=target.min(MAX_IQ) {
                let source=json!({"data_source":"live_iq","capture_epoch":d.capture["epoch"],"channel_id":c.entry.channel_id.to_string(),"epoch":c.entry.epoch.to_string(),"center_hz":c.entry.frequency_hz,"sample_rate_hz":c.entry.output_rate,"first_input_index":c.first_index.map(|n|n.to_string()),"sample_time_kind":"host_receive_time"});
                if let Some(s)=d.signals.iter_mut().find(|s|s.id==c.id) {
                    let job=FeatureJob{id:c.id.clone(),token:c.token,rate:c.entry.output_rate,offset:s.center_hz-c.entry.frequency_hz,bandwidth:s.analysis_bandwidth_hz,iq:c.iq,at:c.at,source};
                    s.identify_state=if self.features.try_send(job).is_ok() {"analyzing"} else {"budget_skipped"}.into();
                }return;
            }
        }
        d.collector=Some(c);
    }
}

pub(super) struct Tap {engine:Arc<Engine>,assembler:display_fft::Assembler,token:u64,next_start:u64,collecting:bool}
impl Tap {
    fn new(engine:Arc<Engine>)->Self {Self{engine,assembler:Default::default(),token:0,next_start:0,collecting:false}}
    pub fn push(&mut self,meta:display_fft::Meta,seq:u64,stamp:u64,iq:&[i16],warning:i32){
        let Some(mut d)=self.engine.data.try_lock() else {if self.collecting {self.engine.skipped.fetch_add(1,Ordering::Relaxed);}self.assembler=Default::default();self.collecting=false;return;};
        if !d.running&&d.region.is_none() || d.pressure_ticks>=3 {return;}
        if d.capture["epoch"].as_str()!=Some(meta.epoch.to_string().as_str()) {return;}
        if d.meta.is_some_and(|old|!same_capture(old,meta)) {d.cancel();for s in &mut d.signals {s.activity="stale".into();}d.meta=None;}
        if d.meta.is_some_and(|old|old.scale!=meta.scale){d.metrics.scale_updates+=1;}
        d.meta=Some(meta);
        let token=d.token;let hz=d.metrics.rate_hz;
        if self.token!=token {self.assembler=Default::default();self.token=token;self.next_start=0;self.collecting=false;}
        drop(d);
        // Cadence belongs to the tap, not to partial assembly. A busy analysis
        // lock may discard a partial block but must never reset the duty cycle.
        if !self.collecting {
            if stamp<self.next_start {return;}
            self.next_start=stamp.saturating_add((125_000_000.0/hz) as u64);self.collecting=true;self.assembler=Default::default();
        }
        if let Some(job)=self.assembler.push_at(BLOCK,meta,seq,stamp,iq,warning,hz){
            self.collecting=false;
            if warning!=0 && warning!=-301 {if let Some(mut d)=self.engine.data.try_lock(){d.metrics.skipped+=1;d.metrics.overloads+=1;d.metrics.rate_hz=0.5;d.reason=Some("capture_pressure".into());}return;}
            if self.engine.raw.try_send(RawJob{job,token}).is_err(){self.engine.skipped.fetch_add(1,Ordering::Relaxed);}
        }
    }
}

struct Psd {n:usize,fft:Arc<dyn rustfft::Fft<f32>>,window:Vec<f32>,energy:f64,buffer:Vec<Complex32>,scratch:Vec<Complex32>,background:Vec<f64>,background_meta:Option<display_fft::Meta>}
impl Psd {
    fn new(n:usize)->Self {let fft=FftPlanner::new().plan_fft_forward(n);let window:Vec<f32>=(0..n).map(|i|(0.5-0.5*(std::f64::consts::TAU*i as f64/n as f64).cos()) as f32).collect();
        let energy=window.iter().map(|w|(*w as f64).powi(2)).sum();let scratch=vec![Complex32::default();fft.get_inplace_scratch_len()];
        Self{n,fft,window,energy,buffer:vec![Complex32::default();n],scratch,background:vec![],background_meta:None}}
    fn floor(&mut self,meta:display_fft::Meta,power:&[f64])->Vec<f64>{
        let (_,estimate,_)=detect(meta,power,(meta.center-meta.bandwidth/2.0,meta.center+meta.bandwidth/2.0),8.0);
        if self.background_meta.is_none_or(|old|!same_capture(old,meta))||self.background.len()!=estimate.len(){self.background=estimate;}
        else {for ((old,new),p) in self.background.iter_mut().zip(estimate).zip(power){
            if let Some(previous)=self.background_meta { *old*=(meta.scale as f64/previous.scale as f64).powi(2); }
            // Occupied bins cannot raise their own background. Decreases track
            // faster than increases; source/ref/scale changes reset the model.
            if new<*old {*old=0.9* *old+0.1*new;}else if *p<*old*3.0 {*old=0.98* *old+0.02*new;}
        }}self.background_meta=Some(meta);self.background.clone()
    }
    fn compute(&mut self,iq:&[i16],rate:f64,scale:f32)->Vec<f64>{
        let n=self.n;let mut power=vec![0.0;n];let mut segments=0;
        for start in (0..=iq.len()/2-n).step_by(n/2).take(4) {
            for (i,value) in self.buffer.iter_mut().enumerate(){*value=Complex32::new(iq[2*(start+i)] as f32,iq[2*(start+i)+1] as f32)*(scale*self.window[i]);}
            self.fft.process_with_scratch(&mut self.buffer,&mut self.scratch);
            for (i,z) in self.buffer.iter().enumerate(){power[(i+n/2)%n]+=z.norm_sqr() as f64/(rate*self.energy);}
            segments+=1;
        }
        for p in &mut power {*p/=segments as f64;}power
    }
}
#[derive(Debug)]
struct Detection {lo:f64,hi:f64,excess:f64,power:f64,edge:bool}
/// Local lower quantiles resist narrow peaks; a global lower quantile prevents
/// a broad occupied plateau from being incorporated into its own background.
fn detect(meta:display_fft::Meta,power:&[f64],range:(f64,f64),threshold:f64)->(Vec<Detection>,Vec<f64>,bool){detect_floor(meta,power,range,threshold,None)}
fn detect_floor(meta:display_fft::Meta,power:&[f64],range:(f64,f64),threshold:f64,floor:Option<&[f64]>)->(Vec<Detection>,Vec<f64>,bool){
    let n=power.len();let bin=meta.rate/n as f64;let start=meta.center-meta.rate/2.0;
    let first=((((meta.center-meta.bandwidth/2.0).max(0.0)-start)/bin).ceil() as usize).min(n);
    let end=(((meta.center+meta.bandwidth/2.0-start)/bin).floor() as usize).min(n);
    let capture_edges=(first,end);
    if end<=first {return(vec![],vec![1e-30;n],false);}
    let quantile=|values:&[f64]|{let mut a=values.to_vec();let k=a.len()/5;a.select_nth_unstable_by(k,|a,b|a.total_cmp(b));a[k].max(1e-30)};
    let global=quantile(&power[first..end]);let mut background=vec![global;n];
    for base in (first..end).step_by(512) {let stop=(base+512).min(end);let local=quantile(&power[base..stop]).min(global*4.0);
        background[base..stop].fill(local);}
    if let Some(floor)=floor.filter(|f|f.len()==n){background.copy_from_slice(floor);}
    // Like OnAir's detectRegions: seed on smoothed linear power, extend the
    // skirts towards the floor, then merge touching components. Raw-bin runs
    // split speech sidebands and FM modulation into unusably small probes.
    let first=first.max(((range.0-start)/bin).ceil().max(0.0) as usize);
    let end=end.min(((range.1-start)/bin).floor().max(0.0) as usize);
    if end<=first {return(vec![],background,true);}
    let mut sums=vec![0.0;n+1];let mut floors=vec![0.0;n+1];
    for i in 0..n {sums[i+1]=sums[i]+power[i];floors[i+1]=floors[i]+background[i];}
    let ratio:Vec<f64>=(0..n).map(|i|{let a=i.saturating_sub(2);let b=(i+3).min(n);
        10.0*((sums[b]-sums[a]).max(1e-30)/(floors[b]-floors[a]).max(1e-30)).log10()}).collect();
    let mut runs:Vec<(usize,usize)>=vec![];let mut i=first;
    while i<end {
        if ratio[i]<threshold {i+=1;continue;}
        let mut hi=i+1;let mut last=hi;
        while hi<end {if ratio[hi]>threshold {last=hi+1;}else if hi+1>last+2 {break;}hi+=1;}
        let limit=(2*(last-i)).max(8);let mut lo=i;hi=last;
        while lo>first && i-lo<limit && ratio[lo-1]>2.5 {lo-=1;}
        while hi<end && hi-last<limit && ratio[hi]>2.5 {hi+=1;}
        let gap=(1500.0/bin).ceil().max(4.0) as usize;
        if let Some(previous)=runs.last_mut().filter(|r|lo<=r.1+gap.max((r.1-r.0)/8)) {previous.1=previous.1.max(hi);}
        else {runs.push((lo,hi));}
        i=hi.max(i+1);
    }
    let mut rows=Vec::new();let mut occupied=0;
    for (lo,hi) in runs {
        let signal:f64=power[lo..hi].iter().sum();let noise:f64=background[lo..hi].iter().sum();
        occupied+=hi-lo;
        let excess=10.0*(signal/noise).max(1e-30).log10();
        if hi-lo>=2 && excess>=threshold-3.0 {rows.push(Detection{lo:(start+lo as f64*bin).max(range.0),hi:(start+hi as f64*bin).min(range.1),excess,power:signal*bin,edge:lo==capture_edges.0||hi==capture_edges.1});}
    }
    (rows,background,occupied as f64/((end-first) as f64)<0.7)
}
struct Observation {incoming:Vec<(Detection,&'static str)>,reliable:bool,truncated:usize,origin:Option<String>}
fn prepare_observation(meta:display_fft::Meta,power:&[f64],running:bool,range:Option<(f64,f64)>,channels:&[(f64,f64)],region:Option<Region>,threshold:f64,floor:Option<&[f64]>)->Observation {
    let full=((meta.center-meta.bandwidth/2.0).max(0.0),meta.center+meta.bandwidth/2.0);
    let (regions,background,reliable)=detect_floor(meta,power,range.unwrap_or(full),threshold,floor);
    let mut incoming=Vec::new();let mut truncated=0;let mut origin=None;
    if running {
        if channels.is_empty(){incoming.extend(regions.into_iter().map(|r|(r,"scan")));}
        else {
            let bin=meta.rate/power.len() as f64;let start=meta.center-meta.rate/2.0;
            let mut sums=vec![0.0;power.len()+1];let mut floors=vec![0.0;power.len()+1];
            for i in 0..power.len(){sums[i+1]=sums[i]+power[i];floors[i+1]=floors[i]+background[i];}
            for &(lo,hi) in channels {if lo<full.0||hi>full.1 {continue;}let a=((lo-start)/bin).ceil() as usize;let b=(((hi-start)/bin).floor() as usize).min(power.len());
                if b<=a {continue;}incoming.push((Detection{lo,hi,excess:10.0*((sums[b]-sums[a])/(floors[b]-floors[a]).max(1e-30)).max(1e-30).log10(),power:(sums[b]-sums[a])*bin,edge:false},"channel"));}
        }
    }
    if let Some(region)=region {
        let (mut rows,_,_)=detect_floor(meta,power,(region.lo,region.hi),threshold,floor);
        rows.sort_by(|a,b|b.excess.total_cmp(&a.excess));
        truncated=rows.len().saturating_sub(8);rows.truncate(8);
        if rows.is_empty(){rows.push(Detection{lo:region.lo,hi:region.hi,excess:0.0,power:0.0,edge:false});}
        incoming.extend(rows.into_iter().map(|r|(r,"region")));origin=Some(region.origin);
    }
    Observation{incoming,reliable,truncated,origin}
}
impl Data {
    #[cfg(test)]
    fn observe(&mut self,meta:display_fft::Meta,power:&[f64]) {
        let ready=prepare_observation(meta,power,self.running,self.range,&self.channels,self.region.clone(),self.threshold,None);self.apply_observation(meta,ready);
    }
    fn apply_observation(&mut self,meta:display_fft::Meta,ready:Observation) {
        let selected_bounds=self.region.as_ref().map(|r|(r.lo,r.hi));
        self.metrics.observations+=1;self.metrics.observed_seconds+=BLOCK as f64/meta.rate;
        self.metrics.last_at=Some(recording::millis());if self.meta.is_none(){self.meta=Some(meta);}self.background_reliable=ready.reliable;self.truncated=ready.truncated;
        self.signals.retain(|s|s.source=="region"||s.activity!="stale");
        if self.running {
            self.state="running".into();
            for s in self.signals.iter_mut().filter(|s|s.source!="region"&&s.activity!="ignored"&&s.activity!="stale") {s.observations+=1;s.history=(s.history<<1)&7;if s.last_seen.is_some_and(|t|t.elapsed()>Duration::from_secs(1)){s.activity="unseen".into();}}
        }
        if let Some(origin)=ready.origin {self.region=None;self.signals.retain(|s|s.identify_state!="detecting");self.truncated=ready.truncated;self.reason=Some(format!("live_iq_{origin}"));}
        let incoming=ready.incoming;
        let now=Instant::now();let mut auto_queue=Vec::new();
        for (r,source) in incoming {
            if r.hi<=r.lo {continue;}
            let matched=self.signals.iter().position(|s|s.source==source&&s.activity!="stale"&&s.activity!="ignored"&&(source!="channel"||(s.low_hz-r.lo).abs()<0.5&&(s.high_hz-r.hi).abs()<0.5)&&r.lo<s.high_hz&&r.hi>s.low_hz
                && ((r.lo+r.hi)/2.0-s.center_hz).abs()<((r.hi-r.lo).max(s.bandwidth_hz)*0.6));
            let index=if let Some(index)=matched {index} else {
                if self.signals.len()>=MAX_SIGNALS {if source=="region" {if let Some(index)=self.signals.iter().position(|s|s.source!="region"){self.signals.remove(index);}}else{self.truncated+=1;continue;}}
                self.next+=1;self.signals.push(Signal::new(self.next,r.lo,r.hi,source));self.signals.len()-1
            };
            let s=&mut self.signals[index];
            if s.identify_state=="idle" {
                // A narrow carrier peak is not the whole AM channel. Keep room
                // for its audio sidebands, bounded by the user's selected band.
                let scope=if source=="region"{selected_bounds}else{None}.unwrap_or(((meta.center-meta.bandwidth/2.0).max(0.0),meta.center+meta.bandwidth/2.0));
                s.analysis_bandwidth_hz=if source=="channel"{s.bandwidth_hz}else{s.bandwidth_hz.max(12_000.0).min(2.0*(s.center_hz-scope.0).min(scope.1-s.center_hz)).max(s.bandwidth_hz)};
            }
            // Keep reception advice stable under ordinary bin jitter.
            s.excess_db=r.excess;s.power_v2_db=10.0*r.power.max(1e-30).log10();
            let active=r.excess>=if s.hits>0 {5.0}else if source=="channel"{self.threshold}else{self.threshold-3.0};
            if active {s.hits+=1;s.history|=1;s.stable|=s.history.count_ones()>=2;s.last_seen=Some(now);s.activity=if source=="region"||s.stable{"active"}else{"short"}.into();}else if source=="channel"&&s.hits==0{s.activity="unseen".into();}
            s.observations=s.observations.max(1);s.observed_hit_ratio=s.hits as f64/s.observations as f64;s.observed_hit_ratio=s.observed_hit_ratio.min(1.0);
            if source=="region"{s.activity=if active{"active"}else{"not_detected"}.into();s.flags.push("current_samples".into());}
            if r.edge&&!s.flags.iter().any(|f|f=="capture_edge"){s.flags.push("capture_edge".into());}
            if (s.center_hz-meta.center).abs()<meta.rate/FFT as f64*2.0&&!s.flags.iter().any(|f|f=="possible_dc"){s.flags.push("possible_dc".into());}
            if source=="region" || (self.auto&&s.hits>=2&&s.identify_state=="idle") {auto_queue.push(s.id.clone());}
        }
        self.signals.retain(|s|s.source=="region"||s.activity=="ignored"||s.last_seen.or(s.created).is_some_and(|t|t.elapsed()<Duration::from_secs(30)));
        auto_queue.sort_by(|a,b|{let level=|id:&str|self.signals.iter().find(|s|s.id==id).map_or(0.0,|s|s.excess_db);level(b).total_cmp(&level(a))});
        let (manual,background):(Vec<_>,Vec<_>)=auto_queue.into_iter().partition(|id|self.signals.iter().any(|s|s.id==*id&&s.source=="region"));
        for id in background {let _=self.enqueue(&id,false);}
        for id in manual.into_iter().rev() {let _=self.enqueue(&id,true);}
    }
}

/// Reuse the native planned DDC even for a bypass: a wider existing receiver
/// must not lend out-of-region neighbours to the classifier. This owns only
/// bounded copied IQ, never another live capture/receiver execution slot.
fn isolate_region(iq:&[f32],rate:f64,offset:f64,bandwidth:f64)->Result<(Vec<f32>,f64,f64),String> {
    let bandwidth=bandwidth.max(50.0);let transition=(bandwidth*0.1).max(50.0).min((rate-bandwidth)/2.0);
    let minimum=(bandwidth*1.2).max(if bandwidth>=40000.0 {96000.0}else{8000.0}).min(rate);
    let plan=websa_channelizer::rate_plan::RatePlan::baseband(rate,bandwidth,transition,minimum).map_err(str::to_owned)?;
    struct Filter(*mut websa_channelizer::Channel);
    impl Drop for Filter {fn drop(&mut self){unsafe{websa_channelizer::channel_close(self.0);}}}
    let filter=Filter(websa_channelizer::channel_open_planned(rate,-offset,&plan));
    if filter.0.is_null(){return Err("local region filter unavailable".into());}
    let delay=unsafe{websa_channelizer::channel_delay(filter.0)};
    let mut scratch=vec![0.0f32;2*(65536/plan.decimate as usize+2)];let mut output=Vec::new();
    for block in iq.chunks(2*65536) {
        let count=unsafe{websa_channelizer::channel_process_float(filter.0,block.as_ptr(),(block.len()/2) as i32,scratch.as_mut_ptr(),(scratch.len()/2) as i32)};
        if count<0||count as usize>scratch.len()/2{return Err("local region filter failed".into());}
        output.extend_from_slice(&scratch[..2*count as usize]);
    }
    let discard=(2.0*delay).ceil() as usize*2;
    if discard>=output.len(){return Err("insufficient settled region IQ".into());}output.drain(..discard);
    Ok((output,plan.output_rate,delay/plan.output_rate))
}

struct Features {label:&'static str,verdict:&'static str,mode:Option<&'static str>,evidence:Value}
fn refined_center(signal:&Signal,result:&Features)->Option<f64> {
    let key=match result.mode{Some("am")=>"carrier_peak_offset_hz",Some("wfm")=>"residual_frequency_hz",_=>return None};
    let shift=result.evidence[key].as_f64()?;let hz=signal.center_hz+shift;
    (shift.is_finite()&&shift.abs()>0.5&&shift.abs()<signal.analysis_bandwidth_hz*0.4&&hz>=signal.low_hz&&hz<=signal.high_hz).then_some(hz)
}
fn carrier_line(iq:&[f32],rate:f64)->(f64,f64) {
    let size=(iq.len()/2).next_power_of_two().min(16384)/2;
    if size<1024{return(0.0,0.0);}
    let fft=FftPlanner::new().plan_fft_forward(size);let mut buffer=vec![Complex32::default();size];let mut power=vec![0.0;size];
    for block in iq.chunks_exact(2*size).take(4) {
        for (i,v) in block.chunks_exact(2).enumerate(){let w=(0.5-0.5*(std::f64::consts::TAU*i as f64/size as f64).cos()) as f32;buffer[i]=Complex32::new(v[0],v[1])*w;}
        fft.process(&mut buffer);for (p,z) in power.iter_mut().zip(&buffer){*p+=z.norm_sqr() as f64;}
    }
    let peak=(0..size).max_by(|&a,&b|power[a].total_cmp(&power[b])).unwrap_or(0);
    let fraction=[(peak+size-1)%size,peak,(peak+1)%size].iter().map(|&i|power[i]).sum::<f64>()/power.iter().sum::<f64>().max(1e-30);
    let frequency=if peak>size/2{peak as f64-size as f64}else{peak as f64}*rate/size as f64;
    (fraction,frequency)
}
fn stereo_pilot(fm:&[f64],rate:f64)->bool {
    if rate<96_000.0||fm.len()<3*4096{return false;}
    let size=(fm.len()/3).next_power_of_two().min(32768)/2;
    let fft=FftPlanner::new().plan_fft_forward(size);let mut buffer=vec![Complex32::default();size];
    for j in 0..3 {
        let begin=j*(fm.len()-size)/2;
        for (i,z) in buffer.iter_mut().enumerate(){let w=0.5-0.5*(std::f64::consts::TAU*i as f64/size as f64).cos();*z=Complex32::new((fm[begin+i]*w) as f32,0.0);}
        fft.process(&mut buffer);
        let a=(18850.0*size as f64/rate).ceil() as usize;let b=(19150.0*size as f64/rate).floor() as usize;
        let peak=(a..=b).max_by(|&a,&b|buffer[a].norm_sqr().total_cmp(&buffer[b].norm_sqr())).unwrap_or(a);
        let line=(peak-1..=peak+1).map(|i|buffer[i].norm_sqr() as f64).sum::<f64>();
        let mut neighbours:Vec<f64>=((18000.0*size as f64/rate) as usize..=(20000.0*size as f64/rate) as usize).filter(|i|i.abs_diff(peak)>4).map(|i|buffer[i].norm_sqr() as f64).collect();
        let k=neighbours.len()/2;neighbours.select_nth_unstable_by(k,|a,b|a.total_cmp(b));
        let total=buffer[1..size/2].iter().map(|z|z.norm_sqr() as f64).sum::<f64>();
        if line<neighbours[k]*3.0*16.0||line<total*0.0003{return false;}
    }true
}
fn features_iq(iq:&[f32],rate:f64,offset:f64)->Features {
    let n=iq.len()/2;let unknown=|evidence|Features{label:"unknown",verdict:"unknown",mode:None,evidence};
    if n<4096 || !rate.is_finite() || rate<=0.0 {return unknown(json!({"reason":"insufficient_iq"}));}
    let mut amp=Vec::with_capacity(n);let mut fm=Vec::with_capacity(n-1);let mut sum=Complex32::default();let mut prev=Complex32::default();
    for (i,v) in iq.chunks_exact(2).enumerate(){let phase=-std::f64::consts::TAU*offset*i as f64/rate;
        let z=Complex32::new(v[0],v[1])*Complex32::from_polar(1.0,phase as f32);amp.push(z.norm() as f64);sum+=z;
        if i>0 {fm.push((z*prev.conj()).arg() as f64*rate/std::f64::consts::TAU);}prev=z;}
    let mean=amp.iter().sum::<f64>()/n as f64;let power=amp.iter().map(|a|a*a).sum::<f64>()/n as f64;
    let cv=(power-mean*mean).max(0.0).sqrt()/mean.max(1e-15);
    let freq_mean=fm.iter().sum::<f64>()/fm.len() as f64;for f in &mut fm {*f-=freq_mean;}
    let deviation=(fm.iter().map(|f|f*f).sum::<f64>()/fm.len() as f64).sqrt();
    // A coherent mean at exactly DC misses every carrier with a small offset.
    // Use a Hann spectral line, as in OnAir's powerLines, rather than requiring
    // an AM carrier to land on the requested window's exact centre.
    let (carrier,carrier_hz)=carrier_line(iq,rate);
    let pilot=stereo_pilot(&fm,rate);
    // Discrete discriminator plateaus are only a family hint. Require most
    // samples near 2/4 narrow clusters so sinusoidal FM is not called FSK.
    let mut histogram=[0usize;128];let spread=(deviation*4.0).max(1.0);
    for f in &fm {let i=((*f/spread+0.5)*128.0).floor() as isize;if (0..128).contains(&i){histogram[i as usize]+=1;}}
    let mut peaks:Vec<usize>=(2..126).filter(|&i|histogram[i]>histogram[i-1]&&histogram[i]>=histogram[i+1]&&histogram[i]>fm.len()/25).collect();
    peaks.sort_by_key(|&i|std::cmp::Reverse(histogram[i]));
    let mut clusters=Vec::new();for i in peaks {if clusters.iter().all(|j:&usize|i.abs_diff(*j)>8){clusters.push(i);}if clusters.len()==4{break;}}
    let covered:usize=clusters.iter().map(|&i|histogram[i.saturating_sub(2)..=(i+2).min(127)].iter().sum::<usize>()).sum();
    let fsk=if covered as f64/fm.len() as f64>0.72&&deviation>150.0&&cv<0.18 {match clusters.len(){2=>Some("2FSK-like"),4=>Some("4FSK-like"),_=>None}}else{None};
    let mut moment2=Complex32::default();let mut moment4=Complex32::default();let mut moment1=Complex32::default();
    for (i,v) in iq.chunks_exact(2).enumerate(){let z=Complex32::new(v[0],v[1]);let phase=-std::f64::consts::TAU*(offset+freq_mean)*i as f64/rate;let u=z/z.norm().max(1e-12)*Complex32::from_polar(1.0,phase as f32);moment1+=u;moment2+=u*u;moment4+=u*u*u*u;}
    let mut phase_line=0.0f64;
    if cv<0.18 && moment1.norm() as f64/(n as f64)<0.3 {
        let size=4096;let fft=FftPlanner::new().plan_fft_forward(size);let mut buffer=vec![Complex32::default();size];let mut scratch=vec![Complex32::default();fft.get_inplace_scratch_len()];let mut windows=0;
        for values in iq.chunks_exact(size*2).take(3) {
            for (i,v) in values.chunks_exact(2).enumerate(){let z=Complex32::new(v[0],v[1]);let u=z/z.norm().max(1e-12);buffer[i]=u*u*u*u;}
            fft.process_with_scratch(&mut buffer,&mut scratch);let total=buffer.iter().map(|z|z.norm_sqr() as f64).sum::<f64>();
            let peak=buffer.iter().enumerate().max_by(|a,b|a.1.norm_sqr().total_cmp(&b.1.norm_sqr())).map(|(i,_)|i).unwrap_or(0);
            phase_line+=[(peak+size-1)%size,peak,(peak+1)%size].iter().map(|&i|buffer[i].norm_sqr() as f64).sum::<f64>()/total.max(1e-30);windows+=1;
        }
        phase_line/=windows.max(1) as f64;
    }
    let psk=phase_line>0.7;
    let (label,verdict,mode)=if power<1e-30 {("unknown","unknown",None)}
        else if pilot&&cv<0.6&&deviation>500.0 {("FM stereo pilot","credible",Some("wfm"))}
        else if let Some(label)=fsk {(label,"tentative",None)}
        else if psk {("PSK-like","tentative",None)}
        else if carrier>0.18&&cv>0.025&&cv<0.7 {("AM-like","tentative",Some("am"))}
        else if carrier>0.7&&cv<0.08 {("carrier","tentative",Some("cw"))}
        else if cv<0.25&&rate>=128_000.0&&deviation>5000.0 {("FM-like","tentative",Some("wfm"))}
        else if cv<0.12&&deviation>100.0 {("FM-like","tentative",Some("nfm"))}
        else {("unknown","unknown",None)};
    Features{label,verdict,mode,evidence:json!({"sample_rate_hz":rate,"samples":n,"duration_seconds":n as f64/rate,
        "envelope_cv":cv,"frequency_deviation_rms_hz":deviation,"residual_frequency_hz":freq_mean,"carrier_fraction":carrier,"carrier_peak_offset_hz":carrier_hz-offset,
        "fm_19khz_pilot":pilot,"discriminator_clusters":clusters.len(),"cluster_sample_fraction":covered as f64/fm.len() as f64,"phase_fourth_power_line_fraction":phase_line,"phase_second_moment":moment2.norm() as f64/n as f64,"phase_fourth_moment":moment4.norm() as f64/n as f64,"protocol_evidence":[],"limitations":"Shape evidence does not confirm a protocol; short live observation."})}
}

pub(super) struct Probe {slot:u32,original:ChannelConfig,entry:sdr_pipeline::ChannelRegistryEntry}
impl Probe {pub fn matches(&self,frame:&IqFrame)->bool {let h=frame.header();h.len()>=40&&u32::from_le_bytes(h[32..36].try_into().unwrap())==self.entry.channel_id&&u32::from_le_bytes(h[36..40].try_into().unwrap())==self.entry.epoch}}
impl DevState {
    pub(super) fn flush_scan_control(&mut self,app:&App) {
        if !self.pending_receiver.audio.is_empty() || !self.pending_receiver.decode.is_empty() {service_stream::deliver_retiring(self,app);}
    }
    pub(super) fn scan_summary(&self)->Value {self.scan.as_ref().map_or_else(||json!({"service_id":self.receivers.boot,"capture":self.capture_snapshot(),"running":false,"state":"stopped","probe_slots_used":0}),|engine|{
        let d=engine.data.lock();json!({"service_id":self.receivers.boot,"capture":self.capture_snapshot(),"running":d.running,"state":d.state,"probe_slots_used":usize::from(self.scan_probe.is_some()),"probe_slot":self.scan_probe.as_ref().map(|p|p.slot),"reason":d.reason})})}
    pub(super) fn retire_scan_probe(&mut self)->Result<(),String> {
        let Some(probe)=self.scan_probe.as_ref() else {return Ok(());};
        let original=probe.original.clone();
        // Local commands drain the fine lane and acknowledge retirement before
        // returning. Keep the allocation reserved on any failure.
        self.ddc_channel_command(move|ddc|ddc.configure_service_channel(original))?;
        self.scan_probe=None;
        if let Some(engine)=&self.scan {let mut d=engine.data.lock();if let Some(c)=d.collector.take(){let id=c.id;let _=d.enqueue(&id,true);}}
        Ok(())
    }
    pub(super) fn scan_tick_due(&mut self) {
        // The publisher runs at about 1 kHz. Queue diagnostics and resource
        // scheduling need only 20 Hz; HTTP commands still force a fresh check.
        if self.scan.as_ref().is_some_and(|e|Instant::now()>=e.data.lock().next_owner_tick){self.scan_tick();}
    }
    pub(super) fn scan_tick(&mut self) {
        let Some(engine)=self.scan.clone() else {return;};
        let capture=self.capture_snapshot();let signature=json!([capture["epoch"],capture["center_hz"],capture["input_rate_hz"],capture["usable_bandwidth_hz"],capture["state"],self.sdr_ref]);
        let mut d=engine.data.lock();
        d.next_owner_tick=Instant::now()+Duration::from_millis(50);
        d.metrics.skipped+=engine.skipped.swap(0,Ordering::Relaxed);
        let changed=d.signature!=signature;
        if changed {d.cancel();d.tap_attached=false;d.signature=signature;d.meta=None;for signal in &mut d.signals {signal.activity="stale".into();}
            d.background_reliable=false;d.reason=Some("source_changed".into());d.revision+=1;}
        d.capture=capture;
        let healthy=d.capture["state"]=="running"&&d.capture["source_kind"]=="hardware";
        if !healthy {d.state="unavailable".into();d.cancel();drop(d);let _=self.retire_scan_probe();return;}
        if !d.running&&d.region.is_none()&&d.queue.is_empty()&&d.collector.is_none()&&self.scan_probe.is_none(){return;}
        if !d.tap_attached && (d.running||d.region.is_some()) {
            drop(d);let observer=engine.clone();
            match self.ddc_channel_command(move|ddc|{ddc.scan_tap=Some(Tap::new(observer));Ok(vec![])}) {
                Ok(())=>engine.data.lock().tap_attached=true,
                Err(error)=>{engine.data.lock().reason=Some(error);return;}
            }
            d=engine.data.lock();
        }
        let (pressure,queue_full)=self.pipeline.as_ref().map_or((false,0),|p|p.scan_pressure());
        if pressure || queue_full>d.last_queue_full {d.pressure_until=Some(Instant::now()+Duration::from_secs(2));d.pressure_ticks=d.pressure_ticks.saturating_add(1);d.metrics.overloads+=1;}
        d.last_queue_full=queue_full;
        if d.pressure_until.is_some_and(|t|t>Instant::now()) {
            d.metrics.rate_hz=2.0;d.reason=Some("capture_pressure".into());
            if let Some(c)=d.collector.take(){let _=d.enqueue(&c.id,true);}
            if d.pressure_ticks>=3 {d.state="throttled".into();}
            drop(d);let _=self.retire_scan_probe();return;
        } else if d.pressure_until.take().is_some(){d.pressure_ticks=0;d.reason=None;d.metrics.rate_hz=5.0;}
        let timed_out=d.collector.as_ref().is_some_and(|c|c.started.elapsed()>Duration::from_secs(2));
        if timed_out {if let Some(c)=d.collector.take(){if let Some(s)=d.signals.iter_mut().find(|s|s.id==c.id){s.identify_state="insufficient_iq".into();}}}
        let retire=self.scan_probe.is_some()&&d.collector.is_none();
        drop(d);
        if retire && self.retire_scan_probe().is_err(){return;}
        let mut d=engine.data.lock();
        if d.collector.is_some()||self.scan_probe.is_some(){return;}
        if Instant::now()<d.next_schedule {return;}d.next_schedule=Instant::now()+Duration::from_millis(200);
        let Some(id)=d.queue.front().cloned() else {return;};
        let Some(signal)=d.signals.iter().find(|s|s.id==id).cloned() else {d.queue.pop_front();return;};
        if signal.activity=="stale" {d.queue.pop_front();return;}
        let token=d.token;drop(d);
        let configs=self.channel_configs();let registry=self.stream_registry();
        let reusable=configs.iter().filter(|c|c.enabled).find_map(|c|{
            let plan=service_ddc::source_plan(self.capture_input_rate(),c).ok()?;
            let entry=registry.iter().find(|e|e.channel==c.channel&&e.input_rate==self.capture_input_rate())?;
            let covered=(signal.center_hz-c.freq).abs()+signal.analysis_bandwidth_hz/2.0<=plan.plan().occupied_bandwidth_hz/2.0;
            (covered&&entry.output_rate<=1e6&&entry.output_rate>=6000.0&&entry.output_rate>=signal.analysis_bandwidth_hz*1.2).then_some(entry.clone())
        });
        let mut receiver=None;
        let result=if let Some(entry)=reusable {receiver=self.receiver_uid_for_slot(entry.channel).map(str::to_owned);Ok(entry)} else {
            // Definitions reserve slots even when stopped or out of band.
            self.remember_receiver_streams();
            let free=(2..=MAX_CHANNELS as u32).find(|&slot|self.receiver_uid_for_slot(slot).is_none());
            if let Some(slot)=free {
                let original=configs[(slot-1) as usize].clone();let mut config=ChannelConfig::for_slot(slot);
                config.enabled=true;config.processing_enabled=false;config.freq=signal.center_hz;
                let bw=signal.analysis_bandwidth_hz.max(2000.0);let transition=(bw*0.15).max(1000.0);
                config.baseband=Some(BasebandConfig{passband_hz:bw,transition_hz:transition,minimum_rate_hz:(bw*1.4).max(48_000.0)});
                service_ddc::source_plan(self.capture_input_rate(),&config).and_then(|source|{
                    service_ddc::validate_source_frequency(self.capture_center(),self.capture_bandwidth(),self.capture_input_rate(),self.sdr_pfb_taps_per_phase,config.freq,&source)?;
                    if source.plan().output_rate>1e6 {return Err("probe_bandwidth_budget".into());}
                    self.ddc_channel_command(move|ddc|ddc.configure_service_channel(config))?;
                    let entry=self.stream_registry().iter().find(|e|e.channel==slot).cloned().ok_or("probe admission produced no source")?;
                    self.scan_probe=Some(Probe{slot,original,entry:entry.clone()});Ok(entry)
                })
            } else {Err("capacity".into())}
        };
        let mut d=engine.data.lock();if d.token!=token{return;}
        match result {
            Ok(entry)=>{d.queue.pop_front();d.reason=None;
                if let Some(s)=d.signals.iter_mut().find(|s|s.id==id){s.identify_state="sampling".into();s.iq_receiver_uid=receiver;}
                d.collector=Some(Collector{id,token,entry,iq:Vec::new(),next_seq:None,next_index:None,first_index:None,started:Instant::now(),at:recording::millis()});}
            Err(error)=>{d.reason=Some(error.clone());if let Some(s)=d.signals.iter_mut().find(|s|s.id==id){s.identify_state=if error=="capacity"{"waiting"}else{"unavailable"}.into();s.evidence=json!({"reason":error});}
                if error=="capacity"{d.queue.rotate_left(1);}else{d.queue.pop_front();}}
        }
    }
    fn scan_command(&mut self,request:Value)->Result<Value,String> {
        if request["service_id"]!=self.receivers.boot {return Err("source service changed; refresh".into());}
        let id=request["request_id"].as_str().filter(|s|!s.is_empty()&&s.len()<=128).ok_or("request_id required")?.to_owned();
        let engine=self.scan.get_or_insert_with(Engine::new).clone();
        {let d=engine.data.lock();if let Some((_,old,reply))=d.replies.iter().find(|(known,_,_)|known==&id){return if old==&request{Ok(reply.clone())}else{Err("request_id identity conflict".into())};}}
        self.scan_tick();
        let kind=request["type"].as_str().ok_or("type required")?;
        let mut d=engine.data.lock();
        if (d.capture["state"]!="running" || d.capture["source_kind"]!="hardware")&&!matches!(kind,"clear"|"pause"|"stop"|"cancel_region") {return Err("healthy live SDR RF capture required; fixture lanes have no shared RF IQ".into());}
        if request["capture_epoch"]!=d.capture["epoch"] {return Err("capture epoch changed; refresh".into());}
        if kind!="identify_region"&&kind!="cancel_region" {
            if request["scan_id"]!=format!("scan-{}",self.receivers.boot)||request["expected_revision"]!=d.revision.to_string(){return Err("scan revision changed; refresh".into());}
        }
        let full=((d.capture["center_hz"].as_f64().unwrap_or(0.0)-d.capture["usable_bandwidth_hz"].as_f64().unwrap_or(0.0)/2.0).max(0.0),d.capture["center_hz"].as_f64().unwrap_or(0.0)+d.capture["usable_bandwidth_hz"].as_f64().unwrap_or(0.0)/2.0);
        let read_range=|r:&Value|->Result<(f64,f64),String>{let lo=r["low_hz"].as_f64().filter(|v|v.is_finite()).ok_or("finite low_hz required")?;
            let hi=r["high_hz"].as_f64().filter(|v|v.is_finite()).ok_or("finite high_hz required")?;
            if hi<=lo||lo<full.0||hi>full.1{return Err("region exceeds current usable capture; reselect without retuning".into());}Ok((lo,hi))};
        match kind {
            "start"=>{d.cancel();d.running=true;d.state="warming".into();d.reason=None;d.signals.retain(|s|s.source=="region");d.metrics.started.get_or_insert_with(Instant::now);}
            "pause"|"stop"=>{d.cancel();d.running=false;d.state=if kind=="pause"{"paused"}else{"stopped"}.into();for s in &mut d.signals {if s.activity=="active"{s.activity="snapshot".into();}}}
            "clear"=>{
                match request["scope"].as_str().unwrap_or("all") {
                    "all"=>{d.cancel();d.signals.clear();d.truncated=0;d.reason=None;}
                    "inactive"=>{
                        let old:Vec<String>=d.signals.iter().filter(|s|matches!(s.activity.as_str(),"stale"|"unseen"|"snapshot"|"not_detected"|"ignored")).map(|s|s.id.clone()).collect();
                        d.signals.retain(|s|!old.contains(&s.id));d.queue.retain(|id|!old.contains(id));
                        if d.collector.as_ref().is_some_and(|c|old.contains(&c.id)){d.collector=None;}
                    }
                    _=>return Err("clear scope must be all or inactive".into())
                }
            }
            "configure"=>{
                let range=if request.get("range").is_none(){d.range}else if request["range"].is_null(){None}else{Some(read_range(&request["range"])?)};
                let threshold=request["threshold_db"].as_f64().unwrap_or(d.threshold);if !(5.0..=20.0).contains(&threshold){return Err("threshold must be 5..20 dB".into());}
                let mut channels=if request.get("channels").is_none(){d.channels.clone()}else{vec![]};
                if let Some(list)=request["channels"].as_array(){if list.len()>256{return Err("channel table exceeds 256 entries".into());}for c in list{let r=read_range(c)?;
                    if r.1-r.0<8.0*d.capture["input_rate_hz"].as_f64().unwrap()/FFT as f64{return Err("channel requires at least 8 FFT bins; reduce actual capture bandwidth".into());}channels.push(r);}}
                d.cancel();d.range=range;d.channels=channels;d.threshold=threshold;d.auto=request["auto_identify"].as_bool().unwrap_or(d.auto);d.signals.retain(|s|s.source=="region");}
            "identify_region"=>{
                let (lo,hi)=read_range(&request)?;
                let origin=request["origin"].as_str().filter(|s|["spectrum","waterfall","manual"].contains(s)).ok_or("origin required")?.to_owned();
                d.cancel();
                // A new selection replaces only the previous one-shot group.
                let old:Vec<String>=d.signals.iter().filter(|s|s.source=="region").map(|s|s.id.clone()).collect();
                if d.collector.as_ref().is_some_and(|c|old.contains(&c.id)){d.collector=None;}
                d.queue.retain(|id|!old.contains(id));d.signals.retain(|s|s.source!="region");d.signals.truncate(MAX_SIGNALS-8);
                let token=d.token;d.region=Some(Region{lo,hi,origin,token});d.next+=1;let next=d.next;
                let mut pending=Signal::new(next,lo,hi,"region");pending.identify_state="detecting".into();pending.flags.push("requested_window".into());d.signals.push(pending);
            }
            "cancel_region"=>{d.cancel();let old:Vec<String>=d.signals.iter().filter(|s|s.source=="region").map(|s|s.id.clone()).collect();d.region=None;d.queue.retain(|id|!old.contains(id));
                if d.collector.as_ref().is_some_and(|c|old.contains(&c.id)){d.collector=None;}d.signals.retain(|s|s.source!="region");}
            "identify"|"ignore"=>{let signal=d.signals.iter().find(|s|s.id==request["signal_id"]&&s.generation==request["signal_generation"]&&s.activity!="stale").ok_or("signal generation changed")?.id.clone();
                if kind=="identify"{d.enqueue(&signal,true)?;}else{d.queue.retain(|id|id!=&signal);if d.collector.as_ref().is_some_and(|c|c.id==signal){d.collector=None;}if let Some(s)=d.signals.iter_mut().find(|s|s.id==signal){s.activity="ignored".into();}}}
            _=>return Err("unsupported scan command".into())
        }
        d.revision+=1;drop(d);
        self.retire_scan_probe()?;
        self.scan_tick();
        let mut d=engine.data.lock();let mut reply=d.snapshot(&self.receivers.boot,self.scan_probe.is_some());reply["request_id"]=json!(id);d.replies.push_back((id,request,reply.clone()));if d.replies.len()>128{d.replies.pop_front();}Ok(reply)
    }
}

pub(super) async fn snapshot(State(app):State<Arc<App>>)->Response {
    match tokio::task::spawn_blocking(move||{let mut state=app.state.lock().unwrap();state.scan.get_or_insert_with(Engine::new);state.scan_tick();state.flush_scan_control(&app);state.scan.as_ref().map_or_else(||{
        let mut d=Data::default();d.capture=state.capture_snapshot();d.snapshot(&state.receivers.boot,false)
    },|engine|engine.data.lock().snapshot(&state.receivers.boot,state.scan_probe.is_some()))}).await {Ok(value)=>Json(value).into_response(),Err(error)=>state_worker_failed(error).into_response()}
}
pub(super) async fn command(State(app):State<Arc<App>>,headers:HeaderMap,body:String)->Response {
    if !browser_access(&headers){return StatusCode::FORBIDDEN.into_response();}
    let request=match serde_json::from_str::<Value>(&body){Ok(r)=>r,Err(_)=>return StatusCode::BAD_REQUEST.into_response()};
    let request_id=request["request_id"].clone();
    match tokio::task::spawn_blocking(move||{let mut state=app.state.lock().unwrap();let result=state.scan_command(request);state.flush_scan_control(&app);result}).await {
        Ok(Ok(reply))=>Json(reply).into_response(),Ok(Err(error))=>(StatusCode::CONFLICT,Json(json!({"error":error,"code":"scan_rejected","request_id":request_id}))).into_response(),Err(error)=>state_worker_failed(error).into_response()
    }
}
pub(super) async fn detail(State(app):State<Arc<App>>,Path(id):Path<String>)->Response {
    let state=app.state.lock_async().await.unwrap();
    let value=state.scan.as_ref().and_then(|engine|engine.data.lock().signals.iter().find(|s|s.id==id).map(|s|json!(s)));
    match value {Some(mut value)=>{value["service_id"]=json!(state.receivers.boot);value["current_capture_epoch"]=state.capture_snapshot()["epoch"].clone();Json(value).into_response()},None=>StatusCode::NOT_FOUND.into_response()}
}

// The receiver owner rechecks this guard after asynchronous admission.
pub(super) fn validate_receiver_request(state:&DevState,request:&Value)->Result<(),String>{
    let Some(guard)=request.get("scan") else{return Ok(());};
    let engine=state.scan.as_ref().ok_or("scan source expired")?;let d=engine.data.lock();
    if guard["capture_epoch"]!=state.capture_snapshot()["epoch"] || d.capture["state"]!="running" {return Err("scan capture changed".into());}
    let capture=state.capture_snapshot();
    if d.signature!=json!([capture["epoch"],capture["center_hz"],capture["input_rate_hz"],capture["usable_bandwidth_hz"],capture["state"],state.sdr_ref]) {return Err("scan source changed".into());}
    let signal=d.signals.iter().find(|s|s.id==guard["signal_id"]&&s.generation==guard["signal_generation"]&&s.activity!="stale").ok_or("scan candidate expired")?;
    if (request["config"]["baseband"]["frequency_hz"].as_f64().ok_or("scan frequency missing")?-signal.center_hz).abs()>0.5 {return Err("scan recommendation changed".into());}Ok(())
}

impl Engine {
    /// Observe only already-running, identity-matched decoder output. A lock bit
    /// alone is never a protocol confirmation, and no provider is started here.
    pub(super) fn decoded(&self,frames:&[Vec<u8>],registry:&[sdr_pipeline::ChannelRegistryEntry],uids:&[(u32,String)]) {
        let Some(mut d)=self.data.try_lock() else{return;};
        let running=d.running;if !d.signals.iter().any(|s|s.accepts_protocol(running)){return;}
        for frame in frames {
            if frame.len()<8||frame.len()>128*1024 {continue;}
            let Ok(envelope)=serde_json::from_slice::<Value>(&frame[8..]) else{continue;};
            let Some(entry)=registry.iter().find(|e|envelope["channel_id"]==e.channel_id&&envelope["epoch"]==e.epoch) else{continue;};
            let event=&envelope["event"];
            let protocol=if event["type"]=="wfm-info"&&event["rdsGroups"].as_u64().unwrap_or(0)>=3&&event["rdsLocked"]==true {Some("RDS")}
                else if event["type"]=="drm"&&event["status"]["facOk"].as_u64().unwrap_or(0)>=3&&event["status"]["locked"]==true {Some("DRM")}
                else if event["type"]=="ft8"&&event["text"].as_str().is_some_and(|s|!s.trim().is_empty()){Some("FT8")}
                else{None};
            let Some(protocol)=protocol else{continue;};
            for signal in d.signals.iter_mut().filter(|s|s.accepts_protocol(running)) {
                let compatible=if protocol=="FT8" {let hz=entry.frequency_hz+event["frequencyHz"].as_f64().unwrap_or(f64::INFINITY);hz>=signal.low_hz&&hz<=signal.high_hz}
                    else {(signal.center_hz-entry.frequency_hz).abs()<signal.bandwidth_hz*0.25};
                if !compatible {continue;}
                let mode=envelope["mode"].as_str().map(str::to_owned);if signal.protocol.as_deref()!=Some(protocol)||signal.suggested_mode!=mode {signal.advance_generation();}
                signal.protocol=Some(protocol.into());signal.verdict="confirmed".into();
                signal.evidence["protocol_evidence"]=json!({"protocol":protocol,"kind":"running_decoder_valid_output","at_ms":recording::millis(),"source_epoch":entry.epoch.to_string(),
                    "rds_groups":event["rdsGroups"],"drm_fac_ok":event["status"]["facOk"],"ft8_text":event["text"]});
                signal.iq_receiver_uid=uids.iter().find(|(slot,_)|*slot==entry.channel).map(|(_,uid)|uid.clone());
                // Keep modulation and protocol distinct; suggest the actual existing receiver mode.
                signal.suggested_mode=mode;
                signal.receiver_available=true;signal.decoder_available=true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn meta()->display_fft::Meta{display_fft::Meta{epoch:1,generation:1,center:100e6,rate:1_000_000.0,bandwidth:800_000.0,scale:1e-4}}
    fn noise(n:usize)->Vec<i16>{let mut seed=17u64;(0..n*2).map(|_|{seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;((seed%2001) as i32-1000) as i16}).collect()}
    #[test]
    fn welch_noise_tones_adjacent_broad_and_edge(){
        let m=meta();let n=4096;let mut psd=Psd::new(n);let raw=noise(n*5/2);
        let p=psd.compute(&raw,m.rate,m.scale);let range=(99_600_000.0,100_400_000.0);
        assert!(detect(m,&p,range,8.0).0.is_empty(),"noise regions: {:?}",detect(m,&p,range,8.0).0);
        let mut raw=raw;
        for (i,v) in raw.chunks_exact_mut(2).enumerate(){for freq in [-100000.0,-90000.0,399000.0]{let phase=std::f64::consts::TAU*freq*i as f64/m.rate;v[0]+=(5000.0*phase.cos()) as i16;v[1]+=(5000.0*phase.sin()) as i16;}}
        let p=psd.compute(&raw,m.rate,m.scale);let found=detect(m,&p,range,8.0).0;
        assert_eq!(found.len(),3,"{found:?}");assert!((found[1].lo-found[0].hi)>5000.0);
        // Plateau crosses multiple local-background segments and stays one region.
        let mut p=vec![1.0;4096];p[1500..2700].fill(100.0);
        let (found,_,_)=detect(m,&p,range,8.0);assert_eq!(found.len(),1);assert!(found[0].hi-found[0].lo>290000.0);
        p.fill(1.0);p[405..430].fill(100.0);let found=detect(m,&p,range,8.0).0;assert_eq!(found.len(),1);assert!(found[0].edge);
    }
    #[test]
    fn psd_integral_preserves_complex_power(){let n=4096;let mut psd=Psd::new(n);let iq:Vec<i16>=(0..n*5/2).flat_map(|i|{let p=std::f64::consts::TAU*100.0*i as f64/n as f64;[(10000.0*p.cos()) as i16,(10000.0*p.sin()) as i16]}).collect();
        let p=psd.compute(&iq,1e6,1e-4);let integral=p.iter().sum::<f64>()*1e6/n as f64;assert!((integral-1.0).abs()<0.001,"{integral}");}
    #[test]
    fn capture_crossing_zero_never_exposes_negative_rf_candidates(){
        let m=display_fft::Meta{center:100000.0,..meta()};let mut p=vec![1.0;4096];p[810..830].fill(1000.0);p[2440..2460].fill(1000.0);
        let (rows,_,_)=detect(m,&p,(-300000.0,500000.0),8.0);assert_eq!(rows.len(),1,"{rows:?}");assert!(rows[0].lo>=0.0);
    }
    #[test]
    fn stability_and_one_shot_do_not_change_continuous_range(){let mut d=Data::default();d.running=true;d.range=Some((99_800_000.0,100_200_000.0));let mut p=vec![1.0;FFT];p[26000..26030].fill(100.0);
        let range=d.range;let mut m=meta();m.rate=1e6;
        d.observe(m,&p);assert!(d.snapshot("test",false)["signals"].as_array().unwrap().is_empty());
        d.observe(m,&p);assert_eq!(d.snapshot("test",false)["signals"].as_array().unwrap().len(),1);
        let id=d.signals[0].id.clone();d.region=Some(Region{lo:99_900_000.0,hi:100_100_000.0,origin:"manual".into(),token:d.token});
        d.observe(m,&p);assert_eq!(d.range,range);assert!(d.running);assert_eq!(d.signals.iter().find(|s|s.source=="scan").unwrap().id,id);
        assert!(d.signals.iter().any(|s|s.source=="region"));
        d.running=false;d.region=Some(Region{lo:99_900_000.0,hi:100_100_000.0,origin:"manual".into(),token:d.token});d.observe(m,&p);assert!(!d.running);
    }
    fn state()->DevState {let mut s=DevState::new(json!({"preset_defaults":{}}),json!({}));s.swp.mode="sdr".into();s.link_up=true;s.hardware_shim=Some("test".into());s.sdr_center=100e6;s.sdr_listen=100e6;
        s.sdr_channels=DevState::preset_service_channels();s.sdr_channels[0].freq=100e6;
        let configs=s.channel_configs();s.ddc=Some(SendDdc(Ddc::configured_service(100e6,50e6,&mut None,16,IQ_FS,&configs).unwrap()));s.scan=Some(Engine::new());s.scan_tick();s}
    fn request(s:&DevState,kind:&str,id:&str)->Value{let d=s.scan.as_ref().unwrap().data.lock();json!({"service_id":s.receivers.boot,"request_id":id,"type":kind,"capture_epoch":d.capture["epoch"],"scan_id":format!("scan-{}",s.receivers.boot),"expected_revision":d.revision.to_string()})}
    #[test]
    fn commands_deduplicate_reject_stale_and_keep_single_shot_independent(){let mut s=state();let mut r=request(&s,"identify_region","region");r["low_hz"]=json!(99_990_000);r["high_hz"]=json!(100_010_000);r["origin"]=json!("waterfall");
        let reply=s.scan_command(r.clone()).unwrap();assert_eq!(reply["running"],false);assert_eq!(s.scan_command(r.clone()).unwrap(),reply);
        r["low_hz"]=json!(99_999_000);assert!(s.scan_command(r).unwrap_err().contains("conflict"));
        let r=request(&s,"start","start");s.scan_command(r.clone()).unwrap();let mut stale=r;stale["request_id"]=json!("stale");assert!(s.scan_command(stale).is_err());
        let r=request(&s,"pause","pause");s.scan_command(r).unwrap();let d=s.scan.as_ref().unwrap().data.lock();assert!(!d.running);assert!(d.region.is_none());assert!(d.queue.is_empty());assert!(d.signals.iter().all(|s|s.identify_state!="detecting"));
    }
    #[test]
    fn clear_fences_old_work_preserves_scan_configuration_and_works_disconnected(){
        let mut s=state();let engine=s.scan.as_ref().unwrap().clone();
        {let mut d=engine.data.lock();d.running=true;d.range=Some((99.9e6,100.1e6));d.channels=vec![(99.99e6,100.01e6)];let mut row=Signal::new(1,99.99e6,100.01e6,"scan");row.activity="active".into();row.identify_state="analyzing".into();d.signals.push(row);d.queue.push_back("s1".into());}
        let token=engine.data.lock().token;let r=request(&s,"clear","clear");let reply=s.scan_command(r.clone()).unwrap();assert!(engine.data.lock().token>token);assert_eq!(reply["running"],true);assert!(reply["signals"].as_array().unwrap().is_empty());assert_eq!(engine.data.lock().channels.len(),1);assert_eq!(engine.data.lock().range,Some((99.9e6,100.1e6)));assert!(engine.data.lock().queue.is_empty());assert!(s.scan_probe.is_none());assert_eq!(s.scan_command(r).unwrap(),reply);
        {let mut d=engine.data.lock();for (id,activity)in [(2,"active"),(3,"stale")] {let mut row=Signal::new(id,99.99e6,100.01e6,"region");row.activity=activity.into();d.signals.push(row);}}
        let mut r=request(&s,"clear","inactive");r["scope"]=json!("inactive");let reply=s.scan_command(r).unwrap();assert_eq!(reply["signals"].as_array().unwrap().len(),1);assert_eq!(reply["signals"][0]["id"],"s2");
        s.link_up=false;s.scan_tick();let r=request(&s,"clear","offline");assert!(s.scan_command(r).unwrap()["signals"].as_array().unwrap().is_empty());
    }
    #[test]
    fn stopping_capture_retires_probe_before_finishing_formal_receivers(){
        let mut s=state();s.remember_receiver_streams();
        let engine=s.scan.as_ref().unwrap().clone();
        {let mut d=engine.data.lock();let mut signal=Signal::new(1,100_090_000.0,100_100_000.0,"region");signal.activity="active".into();d.signals.push(signal);d.enqueue("s1",true).unwrap();}
        s.scan_tick();assert!(s.scan_probe.is_some());
        let registry=s.stream_registry();
        let frames=s.ddc.as_mut().unwrap().0.step().unwrap();
        assert!(frames.iter().any(|f|s.scan_probe.as_ref().unwrap().matches(f)));
        s.pending_iq.extend(frames);
        s.stop_pipeline().unwrap();
        s.finish_radiofax_with_registry(registry).unwrap();
        assert!(s.scan_probe.is_none());
        assert!(s.receiver_failure.is_none());
    }
    #[test]
    fn full_definitions_wait_then_reuse_compatible_iq_and_retire_probe_before_create(){let mut s=state();s.remember_receiver_streams();
        let engine=s.scan.as_ref().unwrap().clone();{let mut d=engine.data.lock();let mut signal=Signal::new(1,100_090_000.0,100_100_000.0,"region");signal.activity="active".into();d.signals.push(signal);d.enqueue("s1",true).unwrap();}
        s.scan_tick();assert!(s.scan_probe.is_some());let slot=s.scan_probe.as_ref().unwrap().slot;assert!(s.receiver_uid_for_slot(slot).is_none());
        let primary=s.stream_registry().iter().find(|e|e.channel==1).unwrap().clone();
        let frames=s.ddc.as_mut().unwrap().0.step().unwrap();assert!(frames.iter().any(|f|s.scan_probe.as_ref().unwrap().matches(f)));s.pending_iq.extend(frames);
        s.retire_scan_probe().unwrap();assert!(s.receiver_failure.is_none(),"probe retirement failed formal processing: {:?}",s.receiver_failure);assert!(s.scan_probe.is_none());assert!(!s.stream_registry().iter().any(|e|e.channel==slot));assert_eq!(s.stream_registry().iter().find(|e|e.channel==1).unwrap(),&primary);
        // Disabled definitions are still reserved, even without DSP execution.
        for slot in 2..=MAX_CHANNELS as u32 {s.sdr_channels[(slot-1) as usize].enabled=true;s.sdr_channels[(slot-1) as usize].freq=100e6+slot as f64*10000.0;}
        s.remember_receiver_streams();for c in &mut s.sdr_channels[1..]{c.enabled=false;}
        s.remember_receiver_streams();s.scan_tick();assert!(s.scan_probe.is_none());assert_eq!(engine.data.lock().signals[0].identify_state,"waiting");
        {let mut d=engine.data.lock();d.signals[0].center_hz=100e6;d.signals[0].bandwidth_hz=2000.0;d.signals[0].analysis_bandwidth_hz=2000.0;d.next_schedule=Instant::now();}
        s.scan_tick();assert!(s.scan_probe.is_none());assert_eq!(engine.data.lock().signals[0].identify_state,"sampling");
    }
    #[test]
    fn source_change_cancels_and_receiver_guard_rejects_old_candidate(){let mut s=state();let engine=s.scan.as_ref().unwrap().clone();{let mut d=engine.data.lock();d.signals.push(Signal::new(1,99_999_000.0,100_001_000.0,"region"));d.enqueue("s1",true).unwrap();}
        let guard=json!({"scan":{"signal_id":"s1","signal_generation":"1","capture_epoch":s.capture_snapshot()["epoch"]},"config":{"baseband":{"frequency_hz":100e6}}});assert!(validate_receiver_request(&s,&guard).is_ok());
        let old=engine.data.lock().token;s.sdr_ref+=10.0;s.scan_tick();let d=engine.data.lock();assert!(d.token>old);assert!(d.queue.is_empty());assert_eq!(d.signals[0].activity,"stale");drop(d);assert!(validate_receiver_request(&s,&guard).is_err());}
    #[test]
    fn features_keep_noise_unknown_and_validate_fm_pilot(){let rate=192000.0;let count=(rate*0.6) as usize;let mut phase=0.0;let mut iq=vec![];
        for i in 0..count {let t=i as f64/rate;let freq=8000.0*(std::f64::consts::TAU*1000.0*t).sin()+1500.0*(std::f64::consts::TAU*19000.0*t).cos();phase+=std::f64::consts::TAU*freq/rate;iq.extend_from_slice(&[(0.4*phase.cos()) as f32,(0.4*phase.sin()) as f32]);}
        let result=features_iq(&iq,rate,0.0);assert_eq!(result.label,"FM stereo pilot");assert_eq!(result.verdict,"credible");
        let raw=noise(count);let iq:Vec<f32>=raw.iter().map(|v|*v as f32/1000.0).collect();let result=features_iq(&iq,rate,0.0);assert_eq!(result.label,"unknown");
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn copied_iq_gap_restarts_contiguous_collection_without_extending_lease() {
        let data=Arc::new(PLMutex::new(Data::default()));let started=Instant::now();
        let entry=sdr_pipeline::ChannelRegistryEntry{channel:2,channel_id:7,epoch:1,frequency_hz:10e6,output_rate:10000.0,input_rate:10000.0,group_delay_samples:0.0,state:"active"};
        {let mut d=data.lock();d.capture=json!({"epoch":"1"});let mut signal=Signal::new(1,10e6-1000.0,10e6+1000.0,"region");signal.identify_state="sampling".into();d.signals.push(signal);d.collector=Some(Collector{id:"s1".into(),token:d.token,entry,iq:vec![],next_seq:None,next_index:None,first_index:None,started,at:"0".into()});}
        let (raw,_)=mpsc::sync_channel(1);let (features,rx)=mpsc::sync_channel(1);let engine=Engine{data:data.clone(),raw,features,skipped:AtomicU64::new(0)};
        let frame=|seq,index,value|IqFrame::native(websa_channelizer::BasebandMagic::Aux,seq,10000.0,10e6,7,1,index,0.0,10000.0,vec![value;6000]).unwrap();
        engine.iq(&[frame(1,0,1.0)]);engine.iq(&[frame(3,6000,2.0)]);
        {let d=data.lock();let c=d.collector.as_ref().unwrap();assert_eq!(c.started,started);assert_eq!(c.iq.len(),6000);assert_eq!(c.first_index,Some(6000));assert_eq!(d.metrics.iq_restarts,1);}
        engine.iq(&[frame(4,9000,3.0),frame(5,12000,4.0)]);let job=rx.try_recv().unwrap();assert_eq!(job.iq.len(),18000);assert_eq!(job.iq[0],2.0);assert_eq!(job.source["first_input_index"],"6000");assert!(data.lock().collector.is_none());
    }
    #[test]
    fn calibration_refresh_keeps_region_and_rescales_background() {
        let m=display_fft::Meta{epoch:1,generation:7,center:10e6,rate:7_812_500.0,bandwidth:6_250_000.0,scale:1e-4};
        let data=Arc::new(PLMutex::new(Data::default()));
        {let mut d=data.lock();d.capture=json!({"epoch":"1"});d.meta=Some(m);d.region=Some(Region{lo:9.8e6,hi:9.85e6,origin:"manual".into(),token:d.token});let mut signal=Signal::new(1,9.8e6,9.85e6,"region");signal.identify_state="detecting".into();d.signals.push(signal);}
        let (raw,_rx)=mpsc::sync_channel(1);let (features,_)=mpsc::sync_channel(1);let engine=Arc::new(Engine{data:data.clone(),raw,features,skipped:AtomicU64::new(0)});let mut tap=Tap::new(engine);
        let newer=display_fft::Meta{scale:1.01e-4,..m};let token=data.lock().token;
        tap.push(newer,1,0,&vec![0;SDR_PACKET_SAMPLES*2],0);
        {let d=data.lock();assert_eq!(d.token,token);assert!(d.region.is_some());assert_eq!(d.signals[0].identify_state,"detecting");assert_eq!(d.metrics.scale_updates,1);}
        let mut psd=Psd::new(4096);let old=psd.floor(m,&vec![1.0;4096]);let ratio=(newer.scale as f64/m.scale as f64).powi(2);let next=psd.floor(newer,&vec![ratio;4096]);assert!((next[2048]/old[2048]-ratio).abs()<1e-6);
        tap.push(display_fft::Meta{generation:8,..newer},2,SDR_PACKET_SAMPLES as u64*16,&vec![0;SDR_PACKET_SAMPLES*2],0);
        assert!(data.lock().token>token);assert_eq!(data.lock().signals[0].activity,"stale");
    }
    #[test]
    fn noisy_offset_am_and_mono_stereo_fm_remain_identifiable() {
        let mut seed=19u64;let mut noise=||{seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;(seed%20001) as f64/10000.0-1.0};
        for offset in [-377.0,83.0,1210.0] {
            let rate=48000.0;let mut iq=Vec::new();
            for i in 0..38400 {let t=i as f64/rate;let a=1.0+0.4*(std::f64::consts::TAU*713.0*t).sin()+0.15*(std::f64::consts::TAU*2317.0*t).cos();let p=std::f64::consts::TAU*offset*t;iq.extend_from_slice(&[(a*p.cos()+0.10*noise()) as f32,(a*p.sin()+0.10*noise()) as f32]);}
            let result=features_iq(&iq,rate,0.0);assert_eq!(result.label,"AM-like","offset {offset}: {}",result.evidence);let signal=Signal::new(1,100e6-3000.0,100e6+3000.0,"region");assert!((refined_center(&signal,&result).unwrap()-100e6-offset).abs()<10.0);
        }
        for pilot in [false,true] {
            let rate=244140.625;let mut phase=0.0;let mut iq=Vec::new();
            for i in 0..(rate*0.8) as usize {let t=i as f64/rate;let f=23000.0*(std::f64::consts::TAU*847.0*t).sin()+9000.0*(std::f64::consts::TAU*3321.0*t).cos()+if pilot{3500.0*(std::f64::consts::TAU*19009.0*t).cos()}else{0.0};phase+=std::f64::consts::TAU*(f+210.0)/rate;let a=1.0+0.12*(std::f64::consts::TAU*137.0*t).sin();iq.extend_from_slice(&[(a*phase.cos()+0.12*noise()) as f32,(a*phase.sin()+0.12*noise()) as f32]);}
            let result=features_iq(&iq,rate,0.0);assert_eq!(result.mode,Some("wfm"),"pilot {pilot}: {}",result.evidence);assert_eq!(result.evidence["fm_19khz_pilot"],pilot);
        }
    }
    #[test]
    fn broadcast_discovery_merges_sidebands_and_preserves_adjacent_channels() {
        let m=display_fft::Meta{epoch:1,generation:1,center:10e6,rate:1e6,bandwidth:800000.0,scale:1e-4};let mut iq=Vec::new();let mut phase=0.0;let mut seed=91u64;
        for i in 0..BLOCK {let t=i as f64/m.rate;let mut z=Complex32::default();for hz in [100000.0,111000.0] {let a=0.5+0.2*(std::f64::consts::TAU*713.0*t).sin()+0.1*(std::f64::consts::TAU*2317.0*t).cos();let p=std::f64::consts::TAU*hz*t;z+=Complex32::new((a*p.cos()) as f32,(a*p.sin()) as f32);}
            phase+=std::f64::consts::TAU*(-150000.0+35000.0*(std::f64::consts::TAU*1000.0*t).sin())/m.rate;z+=Complex32::new((0.4*phase.cos()) as f32,(0.4*phase.sin()) as f32);seed^=seed<<13;seed^=seed>>7;seed^=seed<<17;let noise=(seed%101) as f32-50.0;iq.extend_from_slice(&[(12000.0*z.re+noise) as i16,(12000.0*z.im-noise) as i16]);}
        let mut psd=Psd::new(FFT);let power=psd.compute(&iq,m.rate,m.scale);let found=detect(m,&power,(9.6e6,10.4e6),8.0).0;
        for hz in [10.1e6,10.111e6] {assert!(found.iter().any(|r|r.lo<hz&&r.hi>hz),"missing {hz}: {found:?}");}
        assert!(found.iter().any(|r|r.lo<9.85e6&&r.hi>9.85e6&&r.hi-r.lo>60000.0),"split FM: {found:?}");assert!(found.len()<8,"fragmented broadcasts: {found:?}");
    }
    #[test]
    fn contention_never_restarts_the_observation_cadence(){
        let data=Arc::new(PLMutex::new(Data::default()));{let mut d=data.lock();d.capture=json!({"epoch":"1"});d.running=true;}
        let (raw,rx)=mpsc::sync_channel(1);let (features,_)=mpsc::sync_channel(1);let engine=Arc::new(Engine{data:data.clone(),raw,features,skipped:AtomicU64::new(0)});let mut tap=Tap::new(engine);
        let m=display_fft::Meta{epoch:1,generation:1,center:100e6,rate:62_500_000.0,bandwidth:50e6,scale:1e-4};let iq=vec![0i16;SDR_PACKET_SAMPLES*2];let mut jobs=0;
        for seq in 0..3900 {let stamp=seq*SDR_PACKET_SAMPLES as u64*2;
            if seq%40==25 {let _guard=data.lock();tap.push(m,seq,stamp,&iq,0);}else{tap.push(m,seq,stamp,&iq,0);}
            if rx.try_recv().is_ok(){jobs+=1;}
        }
        assert!((3..=6).contains(&jobs),"a 5 Hz tap produced {jobs} jobs in one second");
    }
    #[test]
    fn two_fsk_and_psk_are_only_modulation_family_hints(){
        let rate=48000.0;let mut phase=0.0;let mut iq=vec![];
        for i in 0..28800 {let f=if (i/240)%2==0 {-2400.0}else{2400.0};phase+=std::f64::consts::TAU*f/rate;iq.extend_from_slice(&[phase.cos() as f32,phase.sin() as f32]);}
        let result=features_iq(&iq,rate,0.0);assert_eq!(result.label,"2FSK-like");assert_eq!(result.verdict,"tentative");assert!(result.mode.is_none());
        let symbols=[0,3,1,0,2,2,1,3,0,2,3,1,2,0,1,3];
        let iq:Vec<f32>=(0..28800).flat_map(|i|{let p=symbols[i/240%symbols.len()] as f64*std::f64::consts::FRAC_PI_2;[p.cos() as f32,p.sin() as f32]}).collect();
        let result=features_iq(&iq,rate,0.0);assert_eq!(result.label,"PSK-like");assert!(result.mode.is_none());
    }
    #[test]
    fn channel_measurement_sums_linear_power_with_bandwidth_matched_background(){
        let m=display_fft::Meta{epoch:1,generation:1,center:100e6,rate:65536.0,bandwidth:52428.8,scale:1.0};
        let mut power=vec![1.0;FFT];power[33000..33100].fill(10.0);
        let mut d=Data::default();d.running=true;let start=m.center-m.rate/2.0;d.channels=vec![(start+33000.0,start+33100.0),(start+33000.0,start+33200.0)];
        d.observe(m,&power);assert_eq!(d.signals.len(),2);assert!((d.signals[0].excess_db-10.0).abs()<1e-6);assert!((d.signals[1].excess_db-10.0*5.5f64.log10()).abs()<1e-6);assert_eq!(d.signals[1].activity,"unseen","a below-threshold channel window has no observed activity");assert_eq!(d.signals[1].observed_hit_ratio,0.0);
    }
}

pub(super) fn receiver_created(state:&DevState,request:&Value,slot:u32) {
    let Some(uid)=state.receiver_uid_for_slot(slot) else{return;};let Some(engine)=&state.scan else{return;};
    let mut d=engine.data.lock();if let Some(signal)=d.signals.iter_mut().find(|s|s.id==request["scan"]["signal_id"]&&s.generation==request["scan"]["signal_generation"]){signal.receiver_uid=Some(uid.into());}
}

#[cfg(test)]
mod region_filter_tests {
    use super::*;
    #[test]
    fn bypass_rejects_a_strong_adjacent_signal_before_features(){
        let rate=48000.0;let iq:Vec<f32>=(0..38400).flat_map(|i|{let t=i as f64/rate;let p=std::f64::consts::TAU*6000.0*t;let adjacent=0.8*(1.0+0.4*(std::f64::consts::TAU*1000.0*t).sin());[(0.1+adjacent*p.cos()) as f32,(adjacent*p.sin()) as f32]}).collect();
        let (filtered,actual,delay)=isolate_region(&iq,rate,0.0,1000.0).unwrap();assert!(delay>0.0);assert!(actual<rate);
        let power=filtered.chunks_exact(2).map(|v|(v[0] as f64).powi(2)+(v[1] as f64).powi(2)).sum::<f64>()/(filtered.len()/2) as f64;
        assert!((power-0.01).abs()<0.001,"adjacent power leaked: {power}");assert_eq!(features_iq(&filtered,actual,0.0).label,"carrier");
    }
}

#[cfg(test)]
mod availability_tests {
    use super::*;
    #[test]
    fn audio_demodulation_availability_does_not_confirm_a_protocol_decoder(){
        let engine=Engine::new();let rate=48000.0;
        {let mut d=engine.data.lock();d.running=true;let mut signal=Signal::new(1,99_997_000.0,100_003_000.0,"region");signal.identify_state="analyzing".into();d.signals.push(signal);}
        let iq:Vec<f32>=(0..38400).flat_map(|i|{let p=std::f64::consts::TAU*1000.0*i as f64/rate;[(0.3*(1.0+0.5*p.cos())) as f32,0.0]}).collect();
        engine.features.send(FeatureJob{id:"s1".into(),token:1,rate,offset:0.0,bandwidth:6000.0,iq,at:recording::millis(),source:json!({})}).unwrap();
        let deadline=Instant::now()+Duration::from_secs(2);
        while engine.data.lock().signals[0].identify_state!="complete" {assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
        {let d=engine.data.lock();let s=&d.signals[0];assert_eq!(s.label,"AM-like");assert!(s.receiver_available);assert!(!s.decoder_available);assert!(s.protocol.is_none());}
        let registry=[sdr_pipeline::ChannelRegistryEntry{channel:1,channel_id:7,epoch:8,frequency_hz:100e6,output_rate:240000.0,input_rate:IQ_FS,group_delay_samples:0.0,state:"active"}];
        let frame=|groups:u32,epoch:u32|websa_channelizer::encode_decode_event(&serde_json::to_vec(&json!({"channel_id":7,"epoch":epoch,"mode":"wfm","event":{"type":"wfm-info","rdsLocked":true,"rdsGroups":groups}})).unwrap()).unwrap();
        engine.decoded(&[frame(0,8),frame(3,9)],&registry,&[]);assert!(engine.data.lock().signals[0].protocol.is_none());
        engine.decoded(&[frame(3,8)],&registry,&[]);let d=engine.data.lock();assert_eq!(d.signals[0].protocol.as_deref(),Some("RDS"));assert!(d.signals[0].decoder_available);drop(d);
        {let mut d=engine.data.lock();d.running=false;d.signals[0].protocol=None;d.signals[0].decoder_available=false;}
        engine.decoded(&[frame(4,8)],&registry,&[]);assert!(engine.data.lock().signals[0].protocol.is_none(),"paused snapshots must ignore late protocol output");
    }
}
