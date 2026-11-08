//! Immutable normalized IQ retained across native DSP and encoded only at an
//! external boundary. A sample allocation's lifetime, including execution, is
//! accounted independently from logical receiver references.
use std::ops::{Deref,DerefMut};
use std::sync::{Arc,OnceLock};
use std::sync::atomic::{AtomicU64,Ordering};
use crate::{BasebandMagic,encode_baseband,MAX_BASEBAND_SAMPLES};

static BLOCKS:AtomicU64=AtomicU64::new(0);
static BYTES:AtomicU64=AtomicU64::new(0);
static ENCODINGS:AtomicU64=AtomicU64::new(0);

#[derive(Debug)]
pub struct IqSamples {values:Box<[f32]>}
impl IqSamples {
    pub fn new(values:Vec<f32>)->Result<Arc<Self>,&'static str> {
        if values.len()%2!=0 || values.len()/2>MAX_BASEBAND_SAMPLES || values.iter().any(|value|!value.is_finite()) {
            return Err("invalid immutable IQ samples");
        }
        let values=values.into_boxed_slice();BLOCKS.fetch_add(1,Ordering::Relaxed);BYTES.fetch_add((values.len()*4) as u64,Ordering::Relaxed);
        Ok(Arc::new(Self {values}))
    }
    pub fn bytes(&self)->usize {self.values.len()*4}
}
impl Deref for IqSamples {type Target=[f32];fn deref(&self)->&[f32] {&self.values}}
impl Drop for IqSamples {
    fn drop(&mut self) {BLOCKS.fetch_sub(1,Ordering::Relaxed);BYTES.fetch_sub(self.bytes() as u64,Ordering::Relaxed);}
}

#[derive(Clone,Debug)]
pub struct NativeIq {
    pub samples:Arc<IqSamples>,
    pub source_id:Arc<str>,
    pub source_generation:u64,
    pub input_rate:f64,
    pub source_fraction_num:u64,
    pub source_fraction_den:u64,
}
#[derive(Clone)]
struct Storage {header:Vec<u8>,native:Option<NativeIq>,wire:OnceLock<Vec<u8>>}
#[derive(Clone)]
pub struct IqFrame(Arc<Storage>);
impl IqFrame {
    pub fn native(magic:BasebandMagic,sequence:u32,rate:f64,frequency:f64,channel:u64,epoch:u64,index:u64,delay:f64,
        input_rate:f64,values:Vec<f32>)->Result<Self,&'static str> {
        if (sequence==0)!=values.is_empty() || !input_rate.is_finite() || input_rate<rate {return Err("invalid native IQ clock");}
        let samples=IqSamples::new(values)?;
        let mut header=encode_baseband(magic,0,rate,frequency,channel,epoch,index,delay,&[])?;
        header[8..12].copy_from_slice(&sequence.to_le_bytes());
        header[12..16].copy_from_slice(&((samples.len()/2) as u32).to_le_bytes());
        Ok(Self(Arc::new(Storage {header,native:Some(NativeIq {samples,source_id:Arc::from("primary"),source_generation:epoch,
            input_rate,source_fraction_num:0,source_fraction_den:1}),wire:OnceLock::new()})))
    }
    pub fn header(&self)->&[u8] {&self.0.header}
    pub fn native_iq(&self)->Option<&NativeIq> {self.0.native.as_ref()}
    pub fn wire_ready(&self)->bool {self.0.wire.get().is_some()}
    pub fn wire(&self)->&[u8] {
        self.0.wire.get_or_init(|| {
            let mut encoded=self.0.header.clone();
            if let Some(native)=self.native_iq() {
                encoded.reserve(native.samples.bytes());
                for value in native.samples.iter() {encoded.extend_from_slice(&value.to_le_bytes());}
                ENCODINGS.fetch_add(1,Ordering::Relaxed);
            }
            encoded
        })
    }
    pub fn into_wire(self)->Vec<u8> {
        self.wire();
        match Arc::try_unwrap(self.0) {Ok(storage)=>storage.wire.into_inner().unwrap(),Err(storage)=>storage.wire.get().unwrap().clone()}
    }
    pub fn retention()->serde_json::Value {
        serde_json::json!({"unique_native_blocks":BLOCKS.load(Ordering::Relaxed),"unique_native_bytes":BYTES.load(Ordering::Relaxed),
            "wire_encodings":ENCODINGS.load(Ordering::Relaxed)})
    }
}
impl From<Vec<u8>> for IqFrame {
    fn from(value:Vec<u8>)->Self {
        let header=value[..value.len().min(56)].to_vec();
        Self(Arc::new(Storage {header,native:None,wire:OnceLock::from(value)}))
    }
}
impl From<IqFrame> for Vec<u8> {fn from(frame:IqFrame)->Self {frame.into_wire()}}
impl Deref for IqFrame {type Target=[u8];fn deref(&self)->&[u8] {self.wire()}}
impl DerefMut for IqFrame {
    fn deref_mut(&mut self)->&mut [u8] {
        self.wire();let storage=Arc::make_mut(&mut self.0);storage.native=None;
        storage.wire.get_mut().unwrap()
    }
}
impl AsRef<[u8]> for IqFrame {fn as_ref(&self)->&[u8] {self.wire()}}
impl PartialEq for IqFrame {fn eq(&self,other:&Self)->bool {self.wire()==other.wire()}}
impl PartialEq<Vec<u8>> for IqFrame {fn eq(&self,other:&Vec<u8>)->bool {self.wire()==other}}
impl PartialEq<IqFrame> for Vec<u8> {fn eq(&self,other:&IqFrame)->bool {self==other.wire()}}
impl Eq for IqFrame {}
impl std::fmt::Debug for IqFrame {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
        f.debug_struct("IqFrame").field("header",&self.0.header).field("native",&self.0.native.is_some()).field("encoded",&self.wire_ready()).finish()
    }
}
pub trait IqInput {
    fn iq_header(&self)->&[u8];
    fn native_samples(&self)->Option<&NativeIq> {None}
    fn iq_wire(&self)->&[u8];
}
impl IqInput for IqFrame {
    fn iq_header(&self)->&[u8] {if self.native_iq().is_some() {self.header()} else {self.wire()}}
    fn native_samples(&self)->Option<&NativeIq> {self.native_iq()}
    fn iq_wire(&self)->&[u8] {self.wire()}
}
impl IqInput for Vec<u8> {fn iq_header(&self)->&[u8] {self} fn iq_wire(&self)->&[u8] {self}}
impl IqInput for [u8] {fn iq_header(&self)->&[u8] {self} fn iq_wire(&self)->&[u8] {self}}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_shared_samples_preserve_bits_clock_and_lazy_wire_lifetime() {
        let values=vec![0.25,-0.0,f32::from_bits(1),-0.75];
        let frame=IqFrame::native(BasebandMagic::Aux,1,48000.0,100e6,7,11,9007199254740999,3.25,62500000.0,values.clone()).unwrap();
        let copy=frame.clone();let samples=frame.native_iq().unwrap().samples.clone();let weak=Arc::downgrade(&samples);
        assert!(!frame.wire_ready());assert!(Arc::ptr_eq(&samples,&copy.native_iq().unwrap().samples));
        assert_eq!(frame.native_iq().unwrap().input_rate,62500000.0);
        let expected=encode_baseband(BasebandMagic::Aux,1,48000.0,100e6,7,11,9007199254740999,3.25,&values).unwrap();
        assert_eq!(frame.wire(),expected);assert!(copy.wire_ready());assert_eq!(frame.wire().as_ptr(),copy.wire().as_ptr());
        drop(frame);drop(copy);assert!(weak.upgrade().is_some());drop(samples);assert!(weak.upgrade().is_none());
    }
    #[test]
    fn framer_native_batches_and_short_tail_equal_the_external_clock_contract() {
        let mut framer=crate::BasebandFramer::new(3,17,100e6,48000.0).unwrap().with_source_rate(96000.0).unwrap();
        let iq:Vec<_>=(0..2000).flat_map(|index|[index as f32,-0.0]).collect();
        let mut frames=framer.push_native(&crate::FineOutput {channel_id:3,epoch:17,bin:0,seq:0,stamp:777,
            input_index:9007199254740993,output_index:0,output_rate:48000.0,center_offset_hz:None,delay_samples:9.5,iq:&iq[..3000]}).unwrap();
        assert_eq!(frames.len(),1); // identity marker, 1500 samples remain buffered.
        frames.extend(framer.push_native(&crate::FineOutput {channel_id:3,epoch:17,bin:0,seq:1,stamp:3777,
            input_index:9007199254743993,output_index:1500,output_rate:48000.0,center_offset_hz:None,delay_samples:9.5,iq:&iq[3000..]}).unwrap());
        assert_eq!(frames.len(),2);assert!(frames.iter().all(|frame|!frame.wire_ready()));
        let frame=&frames[1];let native=frame.native_iq().unwrap();
        assert_eq!(native.samples.len(),4000);assert_eq!(native.source_fraction_num,0);assert_eq!(native.source_fraction_den,1);
        let expected:Vec<_>=iq.iter().map(|sample|sample/32768.0).collect();
        assert_eq!(native.samples.iter().map(|sample|sample.to_bits()).collect::<Vec<_>>(),expected.iter().map(|sample|sample.to_bits()).collect::<Vec<_>>());
        assert_eq!(frame.wire(),encode_baseband(BasebandMagic::Primary,1,48000.0,100e6,3,17,9007199254740993,9.5,&expected).unwrap());
        assert!(framer.finish_native().unwrap().is_none());
        let mut short=crate::BasebandFramer::new(3,18,100e6,48000.0).unwrap();
        short.push_native(&crate::FineOutput {channel_id:3,epoch:18,bin:0,seq:0,stamp:0,input_index:0,output_index:0,
            output_rate:48000.0,center_offset_hz:None,delay_samples:0.0,iq:&iq[..32]}).unwrap();
        let tail=short.finish_native().unwrap().unwrap();assert!(!tail.wire_ready());assert_eq!(tail.native_iq().unwrap().samples.len(),32);
    }
}
