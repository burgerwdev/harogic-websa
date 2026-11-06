//! Fair, bounded cold preparation. Receiver execution stays per owner thread.
use std::collections::VecDeque;
use std::sync::{Mutex,Condvar,LazyLock};
use std::time::{Instant,Duration};
use serde_json::{json,Value};
#[derive(Default)]
struct State {next:u64,queue:VecDeque<u64>,active:usize,completed:u64,failed:u64,cancelled:u64,
    wall_ns:u64,max_ns:u64,observed_rss_growth:u64}
static STATE:LazyLock<(Mutex<State>,Condvar)>=LazyLock::new(||(Mutex::new(State::default()),Condvar::new()));
pub(crate) struct Preparation {began:Instant,rss:u64,done:bool}
impl Preparation {
    pub fn acquire()->Result<Self,String> {
        let host=crate::host_resources::snapshot();
        let resources=host["preparation_parallelism"].as_u64().unwrap_or(1) as usize;
        let available=host["available_memory_bytes"].as_u64().unwrap_or(0);
        let (mutex,wake)=&*STATE;let mut state=mutex.lock().unwrap_or_else(|e|e.into_inner());
        if state.queue.len()>=64 {return Err("model preparation mailbox full; retry after queued preparations finish".into());}
        let ticket=state.next;state.next+=1;state.queue.push_back(ticket);let waiting=Instant::now();
        loop {
            // Unknown first-use memory is measured with one preparation. Later
            // parallelism uses observed process RSS growth and actual headroom.
            let memory=if state.observed_rss_growth==0 || available==0 {1}
                else {(available/state.observed_rss_growth).max(1) as usize};
            let concurrency=resources.min(memory).max(1);
            if state.queue.front()==Some(&ticket)&&state.active<concurrency {
                state.queue.pop_front();state.active+=1;wake.notify_all();
                return Ok(Self {began:Instant::now(),rss:host["rss_bytes"].as_u64().unwrap_or(0),done:false});
            }
            if waiting.elapsed()>Duration::from_secs(30) {
                state.queue.retain(|&queued|queued!=ticket);state.cancelled+=1;wake.notify_all();
                return Err("model preparation queue exceeded 30 seconds; no replacement call was started".into());
            }
            state=wake.wait_timeout(state,Duration::from_millis(50)).unwrap_or_else(|e|e.into_inner()).0;
        }
    }
    pub fn finish(&mut self,success:bool) {
        if self.done {return;}self.done=true;
        let elapsed=self.began.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let rss=std::fs::read_to_string("/proc/self/status").ok().and_then(|text|text.lines()
            .find_map(|line|line.strip_prefix("VmRSS:").and_then(|value|value.split_whitespace().next()?.parse::<u64>().ok()))).unwrap_or(0)*1024;
        let (mutex,wake)=&*STATE;let mut state=mutex.lock().unwrap_or_else(|e|e.into_inner());
        state.active-=1;if success {state.completed+=1;} else {state.failed+=1;}
        state.wall_ns+=elapsed;state.max_ns=state.max_ns.max(elapsed);
        state.observed_rss_growth=state.observed_rss_growth.max(rss.saturating_sub(self.rss));wake.notify_all();
    }
}
impl Drop for Preparation {fn drop(&mut self) {self.finish(false);}}
pub fn snapshot()->Value {
    let state=STATE.0.lock().unwrap_or_else(|e|e.into_inner());
    json!({"queued":state.queue.len(),"active":state.active,"succeeded":state.completed,"failed":state.failed,
        "cancelled":state.cancelled,"wall_ns":state.wall_ns,"max_ns":state.max_ns,
        "observed_process_rss_growth_bytes":state.observed_rss_growth,"rss_precision":"process_growth_during_preparation; not exclusive model weights",
        "max_queued":64,"max_queue_seconds":30})
}
