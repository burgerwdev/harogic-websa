//! Bounded, identity-scoped observations. Media time never uses the wall clock.
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const HISTORY: usize = 128;
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Identity {
    pub channel: u32,
    pub generation: u64,
    pub channel_id: u32,
    pub epoch: u32,
    pub receiver_uid: Option<String>,
    pub service_id: Option<String>,
}
#[derive(Default)]
pub(super) struct Directory(Mutex<VecDeque<Arc<Lane>>>);
pub(super) struct Lane {
    identity: Identity,
    state: Mutex<State>,
}
#[derive(Default)]
struct Totals {
    accepted: u64, started: u64, succeeded: u64, failed: u64, cancelled: u64,
    cancelled_after_start: u64, rejected: u64, successful_samples: u64,
    media_seconds: f64, wall_ns: u64, cpu_ns: u64, cpu_available: bool,
    wait_ns: u64, wait_max_ns: u64,
}
struct Held { stage: &'static str, since: Instant, samples: u64, bytes: u64 }
#[derive(Default)]
struct State {
    packet: Totals, search: Totals, next: u64, held: BTreeMap<u64, Held>,
    accepted: Option<Value>, started: Option<Value>, succeeded: Option<Value>,
    published: Option<Value>, consumed: Option<Value>, first_rejected: Option<Value>,
    receiver: Value,
    first_failed:Option<Value>,first_cancelled:Option<Value>,
    fault: Option<String>,
}
fn ns(at: Instant) -> u64 { at.elapsed().as_nanos().min(u64::MAX as u128) as u64 }
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

// CLOCK_THREAD_CPUTIME_ID measures CPU, excluding queueing and preemption.
#[cfg(target_os = "linux")]
fn cpu_now() -> Option<u64> {
    #[repr(C)] struct Timespec { seconds: std::ffi::c_long, nanos: std::ffi::c_long }
    unsafe extern "C" { fn clock_gettime(clock: std::ffi::c_int, time: *mut Timespec) -> std::ffi::c_int; }
    let mut time = Timespec { seconds: 0, nanos: 0 };
    (unsafe { clock_gettime(3, &mut time) } == 0).then(||
        (time.seconds as u64).saturating_mul(1_000_000_000).saturating_add(time.nanos as u64))
}
#[cfg(not(target_os = "linux"))]
fn cpu_now() -> Option<u64> { None }

impl Directory {
    pub fn lane(&self, identity: Identity) -> Arc<Lane> {
        let mut lanes = lock(&self.0);
        if let Some(lane) = lanes.iter().find(|lane| lane.identity == identity) { return lane.clone(); }
        // At most current identities plus genuinely in-flight old identities are
        // pinned. Never evict the accounting for a still executing reference.
        while lanes.len() >= HISTORY {
            let Some(index) = lanes.iter().position(|lane| Arc::strong_count(lane) == 1) else { break; };
            lanes.remove(index);
        }
        let lane = Arc::new(Lane { identity, state: Mutex::new(State::default()) });
        lanes.push_back(lane.clone()); lane
    }
    pub fn snapshot_for(&self,channel:u32,uid:Option<&str>)->Value {
        let lanes:Vec<_>=lock(&self.0).iter().filter(|lane|lane.identity.channel==channel
            && uid.is_none_or(|uid|lane.identity.receiver_uid.as_deref()==Some(uid))).cloned().collect();
        json!(lanes.iter().map(|lane|lane.snapshot()).collect::<Vec<_>>())
    }
    /// Lifecycle checks run on every publication. Inspect only identity and
    /// live holds; constructing all diagnostic JSON here can stall RF capture.
    pub fn retains_output(&self,channel:u32,uid:&str,retiring_generation:Option<u64>)->bool {
        lock(&self.0).iter().filter(|lane|lane.identity.channel==channel
            && lane.identity.receiver_uid.as_deref()==Some(uid)).any(|lane|
                retiring_generation==Some(lane.identity.generation)
                || lock(&lane.state).held.values().any(|held|matches!(held.stage,"output_pending"|"tail")))
    }
    pub fn snapshot(&self) -> Value {
        let lanes: Vec<_> = lock(&self.0).iter().cloned().collect();
        json!(lanes.iter().map(|lane| lane.snapshot()).collect::<Vec<_>>())
    }
}
impl Lane {
    pub fn hold_output(self:&Arc<Self>,clock:Value,bytes:u64,stage:&'static str)->OutputHold {
        let mut state=lock(&self.state);let id=state.next;state.next+=1;
        state.held.insert(id,Held {stage,since:Instant::now(),samples:0,bytes});
        OutputHold {lane:self.clone(),id,clock}
    }
    pub fn job(self: &Arc<Self>, search: bool, samples: u64, rate: f64, clock: Value, bytes: u64) -> Job {
        let mut state = lock(&self.state);
        let id = state.next; state.next += 1;
        let totals = if search { &mut state.search } else { &mut state.packet };
        totals.accepted += 1;
        state.held.insert(id, Held {stage: if search {"search_queued"} else {"input_queued"}, since:Instant::now(), samples, bytes});
        if !search { state.accepted = Some(clock.clone()); }
        Job { lane:self.clone(), id, search, samples, rate, clock, started:None, timing_stopped:false, done:false }
    }
    pub fn observe_receiver(&self, value:Value) {lock(&self.state).receiver=value;}
    fn snapshot(&self) -> Value {
        let state = lock(&self.state);
        let totals = |c: &Totals| json!({"accepted":c.accepted,"started":c.started,"succeeded":c.succeeded,
            "failed":c.failed,"cancelled":c.cancelled,"cancelled_after_start":c.cancelled_after_start,
            "rejected":c.rejected,"in_flight":c.accepted.saturating_sub(c.succeeded+c.failed+c.cancelled),
            "successful_samples":c.successful_samples,"successful_media_seconds":c.media_seconds,
            "wall_ns":c.wall_ns,"thread_cpu_ns":c.cpu_available.then_some(c.cpu_ns),
            "queue_wait_ns":c.wait_ns,"queue_wait_max_ns":c.wait_max_ns});
        let mut occupancy = serde_json::Map::new();
        for stage in ["input_queued","executing","output_pending","search_queued","search_executing","search_return_pending","tail"] {
            let rows: Vec<_> = state.held.values().filter(|h|h.stage==stage).collect();
            occupancy.insert(stage.into(),json!({"jobs":rows.len(),"samples":rows.iter().map(|h|h.samples).sum::<u64>(),
                "bytes":rows.iter().map(|h|h.bytes).sum::<u64>(),"oldest_age_ns":rows.iter().map(|h|ns(h.since)).max().unwrap_or(0)}));
        }
        json!({"channel":self.identity.channel,"processing_generation":self.identity.generation.to_string(),
            "channel_id":self.identity.channel_id.to_string(),"epoch":self.identity.epoch.to_string(),
            "receiver_uid":self.identity.receiver_uid,"service_id":self.identity.service_id,
            "packets":totals(&state.packet),"searches":totals(&state.search),"occupancy":occupancy,
            "held_reference_bytes":state.held.values().map(|h|h.bytes).sum::<u64>(),
            "watermarks":{"accepted":state.accepted,"started":state.started,"succeeded":state.succeeded,
                "published":state.published,"service_consumed":state.consumed},
            "first_rejected":state.first_rejected,"fault":state.fault,
            "gap":{"first_failed":state.first_failed,"first_cancelled":state.first_cancelled,"end":null,"end_known":false},
            "receiver":state.receiver,
            "clock_policy":"source_samples; monotonic_wait; UTC_is_not_continuity"})
    }
}
pub(super) struct Job {
    lane: Arc<Lane>, id: u64, search: bool, samples: u64, rate: f64, clock: Value,
    started: Option<(Instant, Option<u64>)>, timing_stopped:bool, done: bool,
}
impl Job {
    pub fn start(&mut self) {
        if self.started.is_some() { return; }
        let mut state = lock(&self.lane.state);
        let wait = state.held.get(&self.id).map(|held|ns(held.since)).unwrap_or(0);
        let totals = if self.search { &mut state.search } else { &mut state.packet };
        totals.started += 1; totals.wait_ns += wait; totals.wait_max_ns = totals.wait_max_ns.max(wait);
        if let Some(held) = state.held.get_mut(&self.id) {
            held.stage=if self.search {"search_executing"} else {"executing"}; held.since=Instant::now();
        }
        if !self.search { state.started=Some(self.clock.clone()); }
        self.started=Some((Instant::now(),cpu_now()));
    }
    pub fn returning(&mut self) {
        self.stop_timing();
        if let Some(held)=lock(&self.lane.state).held.get_mut(&self.id) { held.stage="search_return_pending";held.since=Instant::now(); }
    }
    fn stop_timing(&mut self) {
        if self.timing_stopped {return;}
        self.timing_stopped=true;
        let mut state=lock(&self.lane.state);
        let totals=if self.search {&mut state.search} else {&mut state.packet};
        if let Some((at,cpu))=self.started {
            totals.wall_ns+=ns(at);
            if let Some(elapsed)=cpu.zip(cpu_now()).map(|(old,new)|new.saturating_sub(old)) {
                totals.cpu_available=true;totals.cpu_ns+=elapsed;
            }
        }
    }
    pub fn reject(&mut self, error: &str) {
        let mut state=lock(&self.lane.state);
        let totals=if self.search {&mut state.search} else {&mut state.packet};
        totals.accepted-=1;totals.rejected+=1;
        if state.first_rejected.is_none() {state.first_rejected=Some(self.clock.clone());}
        state.fault=Some(error.into());state.held.remove(&self.id);self.done=true;
    }
    pub fn finish(&mut self, result: Result<bool, &str>) {
        if self.done {return;}
        self.stop_timing();
        let mut state=lock(&self.lane.state);
        let totals=if self.search {&mut state.search} else {&mut state.packet};
        match result {
            Ok(true)=>{totals.succeeded+=1;totals.successful_samples+=self.samples;
                totals.media_seconds+=self.samples as f64/self.rate;},
            Ok(false)=>{totals.cancelled+=1;totals.cancelled_after_start+=u64::from(self.started.is_some());},
            Err(_)=>totals.failed+=1,
        }
        if matches!(result,Ok(true)) && !self.search {state.succeeded=Some(self.clock.clone());}
        if let Err(error)=result {
            state.fault=Some(error.into());
            if state.first_failed.is_none() {state.first_failed=Some(self.clock.clone());}
        }
        if matches!(result,Ok(false)) && state.first_cancelled.is_none() {state.first_cancelled=Some(self.clock.clone());}
        state.held.remove(&self.id);self.done=true;
    }
    pub fn output(&self, bytes: u64) -> OutputHold {
        self.lane.hold_output(self.clock.clone(),bytes,"output_pending")
    }
    pub fn published(&self) {lock(&self.lane.state).published=Some(self.clock.clone());}
}
impl Drop for Job { fn drop(&mut self) { self.finish(Ok(false)); } }
pub(super) struct OutputHold {lane:Arc<Lane>,id:u64,clock:Value}
impl OutputHold {
    pub fn consumed(&self) {lock(&self.lane.state).consumed=Some(self.clock.clone());}
}
impl Drop for OutputHold {fn drop(&mut self) {lock(&self.lane.state).held.remove(&self.id);}}

#[cfg(test)]
mod tests {
    use super::*;
    fn lane() -> Arc<Lane> {Directory::default().lane(Identity {channel:3,generation:7,channel_id:9,epoch:11,
        receiver_uid:Some("C3".into()),service_id:Some("boot-1".into())})}
    #[test]
    fn cancellation_error_rejection_and_success_conserve_work_and_media() {
        let lane=lane();
        let mut good=lane.job(false,480,48000.0,json!({"input_index":"0"}),3840);good.start();
        assert_eq!(lane.snapshot()["occupancy"]["executing"]["bytes"],3840);
        good.finish(Ok(true));
        let output=good.output(32);assert_eq!(lane.snapshot()["occupancy"]["output_pending"]["bytes"],32);
        output.consumed();drop(output);
        let mut failed=lane.job(false,480,48000.0,json!(480),3840);failed.start();failed.finish(Err("inference failed"));
        drop(lane.job(false,480,48000.0,json!(960),3840));
        let mut rejected=lane.job(false,480,48000.0,json!(1440),3840);rejected.reject("input full");
        let snapshot=lane.snapshot();let p=&snapshot["packets"];
        assert_eq!(p["accepted"],3);assert_eq!(p["started"],2);assert_eq!(p["succeeded"],1);
        assert_eq!(p["failed"],1);assert_eq!(p["cancelled"],1);assert_eq!(p["rejected"],1);
        assert_eq!(p["successful_samples"],480);assert_eq!(p["successful_media_seconds"],0.01);
        assert_eq!(p["in_flight"],0);assert_eq!(snapshot["held_reference_bytes"],0);
    }
    #[test]
    fn search_return_and_in_flight_history_remain_accounted() {
        let directory=Directory::default();let identity=lane().identity.clone();
        let lane=directory.lane(identity.clone());let mut search=lane.job(true,180000,12000.0,json!(0),720000);
        search.start();search.returning();
        assert_eq!(lane.snapshot()["occupancy"]["search_return_pending"]["bytes"],720000);
        for generation in 10..300 {let mut next=identity.clone();next.generation=generation;directory.lane(next);}
        assert!(directory.snapshot().as_array().unwrap().len()<=HISTORY);
        assert!(directory.snapshot().as_array().unwrap().iter().any(|row|row["processing_generation"]=="7"));
        search.finish(Ok(false));assert_eq!(lane.snapshot()["searches"]["cancelled_after_start"],1);
    }
    #[test]
    fn lifecycle_query_keeps_old_output_and_tail_scoped_to_uid_and_generation() {
        let directory=Directory::default();let identity=lane().identity.clone();
        let lane=directory.lane(identity);
        assert!(!directory.retains_output(3,"C3",None));
        assert!(directory.retains_output(3,"C3",Some(7)));
        assert!(!directory.retains_output(3,"C3",Some(8)));
        for stage in ["output_pending","tail"] {
            let hold=lane.hold_output(json!(0),32,stage);
            assert!(directory.retains_output(3,"C3",None));
            assert!(!directory.retains_output(1,"C3",Some(7)));
            assert!(!directory.retains_output(3,"new-C3",Some(7)));
            drop(hold);assert!(!directory.retains_output(3,"C3",None));
        }
        let job=lane.job(false,480,48000.0,json!(0),3840);
        assert!(!directory.retains_output(3,"C3",None));drop(job);
    }
    #[test]
    fn observations_are_bounded_and_report_their_measured_overhead() {
        use websa_channelizer::receiver::{InputClock,Receiver,ReceiverConfig};
        let iq:Vec<f32>=(0..4096).flat_map(|index|[0.3+0.1*(index as f32*0.2).sin(),0.0]).collect();
        let mut elapsed=Vec::new();
        for observed in [false,true] {
            let lane=lane();let mut receiver=Receiver::new(ReceiverConfig::default()).unwrap();
            let began=Instant::now();
            for index in 0..100 {
                let clock=InputClock {first_sample_index:index*4096,input_rate:48000.0,delay_input_samples:0.0};
                let mut job=observed.then(||lane.job(false,4096,48000.0,json!({"input_index":clock.first_sample_index}),32768));
                if let Some(job)=job.as_mut() {job.start();}
                std::hint::black_box(receiver.process(&iq,clock,Some(-50.0)).unwrap());
                if let Some(job)=job.as_mut() {job.finish(Ok(true));}
            }
            elapsed.push(began.elapsed().as_secs_f64());
            assert_eq!(lane.snapshot()["held_reference_bytes"],0);
        }
        println!("statistics_overhead_dsp_seconds_without={} with={} ratio={} packets=100; observation_only_not_admission",
            elapsed[0],elapsed[1],elapsed[1]/elapsed[0]);
    }
    #[test]
    fn queue_wait_cpu_and_wall_time_are_distinct_from_media_time() {
        let lane=lane();let mut job=lane.job(false,480,48000.0,json!({"input_index":"9000000000"}),3840);
        std::thread::sleep(std::time::Duration::from_millis(3));job.start();
        std::thread::sleep(std::time::Duration::from_millis(5));job.finish(Ok(true));
        let snapshot=lane.snapshot();let totals=&snapshot["packets"];
        assert!(totals["queue_wait_ns"].as_u64().unwrap()>=2_000_000);
        assert!(totals["wall_ns"].as_u64().unwrap()>=4_000_000);
        assert!(totals["thread_cpu_ns"].as_u64().unwrap()<totals["wall_ns"].as_u64().unwrap());
        assert_eq!(totals["successful_media_seconds"],0.01);
        assert_eq!(snapshot["watermarks"]["succeeded"]["input_index"],"9000000000");
    }
}
