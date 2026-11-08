//! Explicit test instrumentation, excluded from ordinary builds. Hooks execute
//! inside the synchronous DSP/search call, after mailbox dispatch.
use std::collections::{BTreeMap,VecDeque};
use std::sync::{mpsc,Arc,Mutex,LazyLock};
use std::sync::atomic::{AtomicBool,Ordering};
use std::time::{Duration,Instant};

pub enum Probe {
    Wait {entered:mpsc::SyncSender<()>,release:Arc<AtomicBool>,limit:Duration},
    Busy {entered:mpsc::SyncSender<()>,duration:Duration},
    Error(String),
    Panic,
}
type Key=(&'static str,u64);
static PROBES:LazyLock<Mutex<BTreeMap<Key,VecDeque<Probe>>>>=LazyLock::new(||Mutex::new(BTreeMap::new()));
pub fn register(kind:&'static str,identity:u64,probe:Probe) {
    PROBES.lock().unwrap_or_else(|e|e.into_inner()).entry((kind,identity)).or_default().push_back(probe);
}
pub(crate) fn run(kind:&'static str,identity:u64)->Result<(),String> {
    let probe={
        let mut probes=PROBES.lock().unwrap_or_else(|e|e.into_inner());
        let value=probes.get_mut(&(kind,identity)).and_then(VecDeque::pop_front);
        if probes.get(&(kind,identity)).is_some_and(VecDeque::is_empty) {probes.remove(&(kind,identity));}
        value
    };
    match probe {
        Some(Probe::Wait {entered,release,limit})=>{
            let _=entered.try_send(());let began=Instant::now();
            while !release.load(Ordering::Acquire) && began.elapsed()<limit {std::thread::sleep(Duration::from_millis(1));}
            if !release.load(Ordering::Acquire) {return Err("execution probe timed out".into());}
        },
        Some(Probe::Busy {entered,duration})=>{
            let _=entered.try_send(());let began=Instant::now();let mut work=1_u64;
            while began.elapsed()<duration {for _ in 0..1024 {work=work.wrapping_mul(6364136223846793005).wrapping_add(1);}std::hint::black_box(work);}
        },
        Some(Probe::Error(error))=>return Err(error),
        Some(Probe::Panic)=>panic!("controlled receiver execution panic"),
        None=>{},
    }
    Ok(())
}
