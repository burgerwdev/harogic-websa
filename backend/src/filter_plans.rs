//! Immutable coefficient cache. Mutable FIR history and NCO phase stay local.
use std::collections::VecDeque;
use std::sync::{Arc,Mutex,LazyLock};
use std::sync::atomic::{AtomicU64,Ordering};
const ENTRIES:usize=64;
const BYTES:usize=8*1024*1024;
#[derive(Clone,Debug,PartialEq,Eq)]
pub(crate) struct Key {
    pub input_rate:u64,pub factor:usize,pub passband:u64,pub stopband:u64,pub length:usize,
    pub implementation:&'static str,pub scalar_type:&'static str,pub alignment:usize,
}
struct Plan {key:Key,taps:Arc<[f32]>}
static CACHE:LazyLock<Mutex<VecDeque<Plan>>>=LazyLock::new(||Mutex::new(VecDeque::new()));
static PREPARED:AtomicU64=AtomicU64::new(0);
static HITS:AtomicU64=AtomicU64::new(0);

pub(crate) fn prepare(key:Key,build:impl FnOnce()->Vec<f32>)->Arc<[f32]> {
    let mut cache=CACHE.lock().unwrap_or_else(|e|e.into_inner());
    if let Some(plan)=cache.iter().find(|plan|plan.key==key) {HITS.fetch_add(1,Ordering::Relaxed);return plan.taps.clone();}
    // Preparation happens only at channel construction. Serializing a small
    // immutable plan build prevents duplicate transient allocations.
    let taps:Arc<[f32]>=build().into();PREPARED.fetch_add(1,Ordering::Relaxed);
    let bytes=taps.len()*4;
    while cache.len()>=ENTRIES || cache.iter().map(|plan|plan.taps.len()*4).sum::<usize>()+bytes>BYTES {
        let Some(index)=cache.iter().position(|plan|Arc::strong_count(&plan.taps)==1) else {break;};
        cache.remove(index);
    }
    if cache.len()<ENTRIES && cache.iter().map(|plan|plan.taps.len()*4).sum::<usize>()+bytes<=BYTES {
        cache.push_back(Plan {key,taps:taps.clone()});
    }
    // A cache full of active plans does not reject a valid new channel. The
    // new owner pins its plan and never consults/rebuilds it while executing.
    taps
}
pub fn statistics()->serde_json::Value {
    let cache=CACHE.lock().unwrap_or_else(|e|e.into_inner());
    serde_json::json!({"cached_plans":cache.len(),"cached_bytes":cache.iter().map(|plan|plan.taps.len()*4).sum::<usize>(),
        "pinned_plans":cache.iter().filter(|plan|Arc::strong_count(&plan.taps)>1).count(),
        "prepared":PREPARED.load(Ordering::Relaxed),"hits":HITS.load(Ordering::Relaxed),
        "max_cached_plans":ENTRIES,"max_cached_bytes":BYTES})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plans_pin_complete_geometry_and_survive_cache_churn() {
        let key=Key {input_rate:48000.0_f64.to_bits(),factor:4,passband:3000.0_f64.to_bits(),stopband:6000.0_f64.to_bits(),
            length:64,implementation:"test-kaiser-v1",scalar_type:"f32",alignment:32};
        let held=prepare(key.clone(),||vec![0.5;64]);
        let same=prepare(key.clone(),||panic!("same geometry rebuilt"));assert!(Arc::ptr_eq(&held,&same));
        for index in 0..ENTRIES*3 {
            let mut next=key.clone();next.input_rate=(48000.0+index as f64).to_bits();next.factor=3;
            let plan=prepare(next,||vec![index as f32;64]);assert!(!Arc::ptr_eq(&held,&plan));
        }
        assert_eq!(held[0],0.5);assert!(Arc::ptr_eq(&held,&prepare(key.clone(),||panic!("pinned plan evicted"))));
        let mut other=key;other.alignment=16;
        assert!(!Arc::ptr_eq(&held,&prepare(other,||vec![0.5;64])));
        assert!(statistics()["cached_plans"].as_u64().unwrap()<=ENTRIES as u64);
    }
}
