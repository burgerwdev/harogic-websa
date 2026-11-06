//! Bounded receipts travel with ordered input, never on a competing control loop.
use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration,Instant};
pub(super) const CAPACITY:usize=64;
#[derive(Default)]
pub(super) struct Log {rows:Mutex<VecDeque<Arc<Mutex<Value>>>>}
pub(super) struct Command {pub receipt:Arc<Mutex<Value>>,pub generation:u64,pub page:bool,pub reset:bool,
    expires:Option<Instant>,done:bool}
impl Log {
    pub fn available(&self)->bool {self.rows.lock().unwrap_or_else(|e|e.into_inner()).iter().filter(|r|
        r.lock().unwrap_or_else(|e|e.into_inner())["status"]=="queued").count()<CAPACITY}
    pub fn accept(&self,uid:&str,service:&str,revision:u64,generation:u64,id:&str,page:bool,reset:bool,action_id:Option<&str>,remaining:Option<Duration>)->Result<Command,String> {
        if !self.available() {return Err("receiver command mailbox full (64); retry after data advances".into());}
        let receipt=Arc::new(Mutex::new(json!({"receiver_uid":uid,"service_id":service,
            "revision":revision.to_string(),"processing_generation":generation.to_string(),"request_id":id,
            "action_id":action_id,"status":"queued","boundary":null})));
        let mut rows=self.rows.lock().unwrap_or_else(|e|e.into_inner());
        while rows.len()>=128 {if let Some(at)=rows.iter().position(|r|r.lock().unwrap_or_else(|e|e.into_inner())["status"]!="queued") {rows.remove(at);} else {break;}}
        rows.push_back(receipt.clone());
        Ok(Command {receipt,generation,page,reset,expires:remaining.map(|duration|Instant::now()+duration),done:false})
    }
    pub fn snapshot(&self,uid:&str)->Value {json!(self.rows.lock().unwrap_or_else(|e|e.into_inner()).iter()
        .map(|r|r.lock().unwrap_or_else(|e|e.into_inner()).clone()).filter(|r|r["receiver_uid"]==uid).collect::<Vec<_>>())}
}
impl Command {
    pub fn expired(&self)->bool {self.expires.is_some_and(|at|Instant::now()>=at)}
    pub fn finish(&mut self,status:&str,boundary:Value,reason:Option<&str>) {
        if self.done {return;}self.done=true;
        let mut receipt=self.receipt.lock().unwrap_or_else(|e|e.into_inner());
        receipt["status"]=json!(status);receipt["boundary"]=boundary;receipt["reason"]=json!(reason);
    }
    pub fn configuration(&self)->bool {!self.page&&!self.reset}
}
impl Drop for Command {fn drop(&mut self) {self.finish("cancelled",Value::Null,Some("input or processing generation retired before application"));}}
