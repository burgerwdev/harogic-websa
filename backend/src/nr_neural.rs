//! Native CPU streaming ONNX inference. Models are compiled/warmed once per
//! receiver thread, while explicit recurrent caches and PCM state belong to each
//! listening channel. No Python runtime or subprocess participates in reception.
use super::nr::NrAlgorithm;
use ort::{session::{builder::GraphOptimizationLevel, Session, SessionInputValue},
    value::{Tensor, ValueType, TensorElementType}};
use rustfft::{num_complex::Complex32 as C, Fft, FftPlanner};
use std::{cell::RefCell, collections::VecDeque, path::PathBuf, rc::Rc, sync::{Arc, OnceLock}};

type Model = Rc<RefCell<Session>>;
thread_local! {
    static MODELS: RefCell<[Option<Result<Model,String>>;3]> = const { RefCell::new([None,None,None]) };
}
static ORT: OnceLock<Result<(), String>> = OnceLock::new();

fn runtime() -> Result<(), String> {
    ORT.get_or_init(|| {
        let library = "libonnxruntime.so.1.23.2";
        let mut paths = Vec::new();
        if let Some(path) = std::env::var_os("WEBSA_ORT_DYLIB") { paths.push(PathBuf::from(path)); }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() { paths.push(dir.join(library)); }
        }
        paths.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../runtime/linux-x86_64").join(library));
        let path = paths.into_iter().find(|p| p.is_file()).ok_or_else(|| format!("missing native {library}"))?;
        ort::init_from(path).map_err(|e| e.to_string())?.with_name("websa-listener-nr").commit();
        Ok(())
    }).clone()
}

fn index(algorithm: NrAlgorithm) -> usize {
    match algorithm { NrAlgorithm::Gtcrn => 0, NrAlgorithm::UlUnas => 1, NrAlgorithm::FastEnhancer => 2, _ => unreachable!() }
}
fn bytes(algorithm: NrAlgorithm) -> &'static [u8] {
    match algorithm {
        NrAlgorithm::Gtcrn => include_bytes!("../models/nr/gtcrn_dns3_epoch96.onnx"),
        NrAlgorithm::UlUnas => include_bytes!("../models/nr/ulunas_dns3_stream.onnx"),
        NrAlgorithm::FastEnhancer => include_bytes!("../models/nr/fastenhancer_t_48k.onnx"), _ => unreachable!(),
    }
}
fn shapes(algorithm: NrAlgorithm) -> Vec<Vec<usize>> {
    match algorithm {
        NrAlgorithm::Gtcrn => vec![vec![1,257,1,2],vec![2,1,16,16,33],vec![2,3,1,1,16],vec![2,1,33,16]],
        NrAlgorithm::UlUnas => vec![vec![1,257,1,2],vec![1,5358],vec![1,402],vec![1,1056]],
        NrAlgorithm::FastEnhancer => vec![vec![1,513,1,2],vec![1,24,20],vec![1,24,20]], _ => unreachable!(),
    }
}
fn zeros(shape: &[usize]) -> Result<Tensor<f32>, String> {
    Tensor::from_array((shape.to_vec(), vec![0.0;shape.iter().product()])).map_err(|e|e.to_string())
}
fn build(algorithm: NrAlgorithm) -> Result<Model, String> {
    let mut preparation=crate::model_preparation::Preparation::acquire()?;
    let result=(|| {
    #[cfg(feature="bench-instrument")]
    crate::execution_probe::run("prepare-onnx",index(algorithm) as u64)?;
    runtime()?;
    let mut session = Session::builder().map_err(|e|e.to_string())?
        .with_intra_threads(1).map_err(|e|e.to_string())?
        .with_inter_threads(1).map_err(|e|e.to_string())?
        .with_optimization_level(GraphOptimizationLevel::Level3).map_err(|e|e.to_string())?
        .commit_from_memory(bytes(algorithm)).map_err(|e|e.to_string())?;
    let expected = shapes(algorithm);
    if session.inputs().len() != expected.len() || session.outputs().len() != expected.len() {
        return Err("streaming ONNX input/output count mismatch".into());
    }
    for (outlet, expected) in session.inputs().iter().chain(session.outputs()).zip(expected.iter().cycle()) {
        match outlet.dtype() {
            ValueType::Tensor {ty: TensorElementType::Float32, shape, ..}
                if shape.iter().copied().eq(expected.iter().map(|x|*x as i64)) => {},
            other => return Err(format!("unexpected streaming tensor {}: {other:?}", outlet.name())),
        }
    }
    // Warm allocation/operator kernels before the worker accepts IQ. Caches are
    // explicit model inputs; discard these outputs so every real stream starts at zero.
    let warm = expected.iter().map(|s| zeros(s)).collect::<Result<Vec<_>,_>>()?;
    let input: Vec<SessionInputValue<'_>> = warm.iter().map(Into::into).collect();
    let output = session.run(input.as_slice()).map_err(|e|e.to_string())?;
    for (_,value) in output.iter() {
        let (_, data) = value.try_extract_tensor::<f32>().map_err(|e|e.to_string())?;
        if data.iter().any(|v| !v.is_finite()) { return Err("ONNX warmup produced nonfinite values".into()); }
    }
    drop(output);
    Ok(Rc::new(RefCell::new(session)))
    })();preparation.finish(result.is_ok());result
}
fn model(algorithm: NrAlgorithm) -> Result<Model, String> {
    MODELS.with(|models|models.borrow_mut()[index(algorithm)].get_or_insert_with(||build(algorithm)).clone())
}
pub(super) fn retain_selected(algorithm:NrAlgorithm) {
    MODELS.with(|models| {for (slot,model) in models.borrow_mut().iter_mut().enumerate() {
        if !matches!(algorithm,NrAlgorithm::Gtcrn|NrAlgorithm::UlUnas|NrAlgorithm::FastEnhancer)
            || slot!=index(algorithm) || model.as_ref().is_some_and(Result::is_err) {*model=None;}
    }});
}
pub(super) fn session_count()->usize {MODELS.with(|models|models.borrow().iter().filter(|model|model.as_ref().is_some_and(Result::is_ok)).count())}
pub(super) fn prepare() {
    for algorithm in [NrAlgorithm::Gtcrn, NrAlgorithm::UlUnas, NrAlgorithm::FastEnhancer] { let _ = model(algorithm); }
}

// An isolated fixed 3:1 converter: 241-tap Blackman-windowed sinc, 7.2 kHz
// cutoff. Causal decimation phase 2 followed by phase-0 interpolation has exactly
// 238 samples of combined delay at 48 kHz. No floating clock accumulator.
const TAPS: usize = 241;
const CONVERTER_DELAY: usize = TAPS - 3;
struct Rate3 {
    taps: Vec<f32>, down: [f32; TAPS], down_pos: usize, phase: usize,
    up: [f32; TAPS.div_ceil(3)], up_pos: usize,
}
impl Rate3 {
    fn new() -> Self {
        let mut taps: Vec<f32> = (0..TAPS).map(|i| {
            let x = i as f32 - (TAPS-1) as f32/2.0;
            let sinc = if x == 0.0 {2.0*7200.0/48000.0}
                else {(std::f32::consts::TAU*7200.0/48000.0*x).sin()/(std::f32::consts::PI*x)};
            let t = std::f32::consts::TAU*i as f32/(TAPS-1) as f32;
            sinc*(0.42-0.5*t.cos()+0.08*(2.0*t).cos())
        }).collect();
        let sum: f32 = taps.iter().sum(); for v in &mut taps { *v /= sum; }
        Self {taps,down:[0.0;TAPS],down_pos:0,phase:0,up:[0.0;TAPS.div_ceil(3)],up_pos:0}
    }
    fn down(&mut self, sample: f32) -> Option<f32> {
        self.down[self.down_pos] = sample;
        self.down_pos = (self.down_pos+1)%TAPS;
        self.phase = (self.phase+1)%3;
        if self.phase != 0 { return None; }
        Some(self.taps.iter().enumerate().map(|(i,c)|c*self.down[(self.down_pos+TAPS-1-i)%TAPS]).sum())
    }
    fn up(&mut self, sample: f32) -> [f32; 3] {
        let n = self.up.len();
        self.up[self.up_pos] = sample;
        self.up_pos = (self.up_pos+1)%n;
        std::array::from_fn(|phase| (phase..TAPS).step_by(3).enumerate()
            .map(|(i,k)|3.0*self.taps[k]*self.up[(self.up_pos+n-1-i)%n]).sum())
    }
}

pub(super) struct NeuralStream {
    model: Model, caches: Vec<Tensor<f32>>, shape: Vec<usize>,
    fft: Arc<dyn Fft<f32>>, ifft: Arc<dyn Fft<f32>>, scratch: Vec<C>, spectrum: Vec<C>,
    pcm: Vec<f32>, filled: usize, window: Vec<f32>, ola: Vec<f32>, norm: Vec<f32>,
    converter: Option<Rate3>, delay: Vec<f32>, delay_pos: usize, raw: VecDeque<f32>,
    atten_db: f32, warmed: bool,
}
impl NeuralStream {
    pub(super) fn new(algorithm: NrAlgorithm, atten_db: f32) -> Result<Self, String> {
        let model = model(algorithm)?;
        let mut shapes = shapes(algorithm).into_iter();
        let shape = shapes.next().unwrap();
        let n = (shape[1]-1)*2;
        let caches = shapes.map(|s|zeros(&s)).collect::<Result<Vec<_>,_>>()?;
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(n); let ifft = planner.plan_fft_inverse(n);
        let scratch = vec![C::default();fft.get_inplace_scratch_len().max(ifft.get_inplace_scratch_len())];
        let resample = algorithm != NrAlgorithm::FastEnhancer;
        let latency = if resample {3*(n/2)+CONVERTER_DELAY} else {n/2};
        Ok(Self { model,caches,shape,fft,ifft,scratch,spectrum:vec![C::default();n],pcm:vec![0.0;n],filled:0,
            window:(0..n).map(|i|{
                let hann = (0.5-0.5*(std::f32::consts::TAU*i as f32/n as f32).cos()).max(0.0);
                if algorithm == NrAlgorithm::Gtcrn {hann.sqrt()} else {hann}
            }).collect(),ola:vec![0.0;n],norm:vec![0.0;n],
            converter:if resample {Some(Rate3::new())} else {None},delay:vec![0.0;latency],delay_pos:0,
            raw:VecDeque::with_capacity(n*3),atten_db,warmed:false })
    }
    pub(super) fn latency(&self) -> usize { self.delay.len() }
    pub(super) fn set_atten(&mut self, db: f32) { self.atten_db = db.clamp(0.0,100.0); }
    pub(super) fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<(), String> {
        output.clear();
        for &sample in input {
            self.raw.push_back(self.delay[self.delay_pos]);
            self.delay[self.delay_pos] = sample;
            self.delay_pos = (self.delay_pos+1)%self.delay.len();
            let converted = if let Some(converter) = &mut self.converter {converter.down(sample)} else {Some(sample)};
            if let Some(sample) = converted {
                let hop = self.pcm.len()/2;
                self.pcm[hop+self.filled] = sample;
                self.filled += 1;
                if self.filled == hop { self.frame(output)?; self.filled = 0; }
            }
        }
        Ok(())
    }
    fn frame(&mut self, output: &mut Vec<f32>) -> Result<(), String> {
        let n = self.pcm.len(); let hop = n/2;
        for i in 0..n {self.spectrum[i] = C::new(self.pcm[i]*self.window[i],0.0);}
        self.fft.process_with_scratch(&mut self.spectrum,&mut self.scratch);
        let flat: Vec<f32> = self.spectrum[..self.shape[1]].iter().flat_map(|v|[v.re,v.im]).collect();
        let spectrum = Tensor::from_array((self.shape.clone(),flat)).map_err(|e|e.to_string())?;
        let mut input: Vec<SessionInputValue<'_>> = vec![(&spectrum).into()];
        input.extend(self.caches.iter().map(Into::into));
        let mut session = self.model.borrow_mut();
        let result = session.run(input.as_slice()).map_err(|e|format!("streaming ONNX: {e}"))?;
        let (_, enhanced) = result[0].try_extract_tensor::<f32>().map_err(|e|e.to_string())?;
        if enhanced.len()!=self.shape[1]*2 || enhanced.iter().any(|x|!x.is_finite()) {return Err("nonfinite/invalid neural spectrum".into());}
        for (i,ri) in enhanced.chunks_exact(2).enumerate() {self.spectrum[i] = C::new(ri[0],ri[1]);}
        drop(input);
        for (i,cache) in self.caches.iter_mut().enumerate() {
            let (_,data) = result[i+1].try_extract_tensor::<f32>().map_err(|e|e.to_string())?;
            let (_,target) = cache.try_extract_tensor_mut::<f32>().map_err(|e|e.to_string())?;
            if data.len()!=target.len() || data.iter().any(|x|!x.is_finite()) {return Err("nonfinite/invalid neural cache".into());}
            target.copy_from_slice(data);
        }
        for i in self.shape[1]..n {self.spectrum[i]=self.spectrum[n-i].conj();}
        self.ifft.process_with_scratch(&mut self.spectrum,&mut self.scratch);
        for i in 0..n { self.ola[i]+=self.spectrum[i].re/n as f32*self.window[i]; self.norm[i]+=self.window[i].powi(2); }
        let a = 10.0_f32.powf(-self.atten_db/20.0);
        for i in 0..hop {
            // The first emitted hop represents NEGATIVE source time. A Hann
            // synthesis window has no preceding overlap here; dividing a model's
            // nonlocal spectral estimate by that partial window can amplify it
            // without bound. Emit defined zero pre-roll, while retaining the
            // second half for the first real source samples in the next frame.
            let sample = if self.warmed {self.ola[i]/self.norm[i].max(1e-12)} else {0.0};
            if let Some(converter) = &mut self.converter {
                for enhanced in converter.up(sample) {
                    let original = self.raw.pop_front().ok_or("neural output exceeded input clock")?;
                    output.push((1.0-a)*enhanced+a*original);
                }
            } else {
                let original = self.raw.pop_front().ok_or("neural output exceeded input clock")?;
                output.push((1.0-a)*sample+a*original);
            }
        }
        self.ola.copy_within(hop..,0); self.ola[hop..].fill(0.0);
        self.norm.copy_within(hop..,0); self.norm[hop..].fill(0.0);
        self.pcm.copy_within(hop..,0);
        self.warmed = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn neural_startup_defines_negative_time_without_amplifying_partial_windows() {
        for algorithm in [NrAlgorithm::Gtcrn,NrAlgorithm::UlUnas,NrAlgorithm::FastEnhancer] {
            let mut stream=NeuralStream::new(algorithm,100.0).unwrap();
            let first_hop=if algorithm==NrAlgorithm::FastEnhancer {512} else {768};
            let input:Vec<f32>=(0..first_hop).map(|i|0.2*(i as f32*0.071).cos()).collect();
            let mut output=Vec::new();stream.process(&input,&mut output).unwrap();
            assert_eq!(output.len(),first_hop);
            assert!(output.iter().all(|x|*x==0.0),"{algorithm:?}: negative source time must be silence");
            stream.process(&input,&mut output).unwrap();
            assert!(output.iter().all(|x|x.is_finite() && x.abs()<0.5),"{algorithm:?}: first real hop must have bounded overlap normalization");
        }
    }
    #[test]
    fn fixed_converter_rejects_aliases_and_has_declared_delay() {
        fn tone(hz:f32)->f32 {
            let mut converter=Rate3::new();let mut out=Vec::new();
            for i in 0..24000 {if let Some(v)=converter.down((std::f64::consts::TAU*hz as f64*i as f64/48000.0).sin() as f32) {out.push(v);}}
            (out[500..].iter().map(|x|x*x).sum::<f32>()/(out.len()-500) as f32).sqrt()
        }
        assert!((tone(1000.0)-0.5_f32.sqrt()).abs()<0.001);
        assert!(tone(10000.0)<0.0001);
        // Compare a 1kHz round-trip with its analytic, 238-sample delayed input.
        let mut converter=Rate3::new();let mut out=Vec::new();
        for i in 0..12000 {if let Some(v)=converter.down((std::f64::consts::TAU*1000.0*i as f64/48000.0).sin() as f32) {out.extend(converter.up(v));}}
        let error=out.iter().enumerate().skip(1000).map(|(i,x)|
            (x-(std::f64::consts::TAU*1000.0*(i-CONVERTER_DELAY) as f64/48000.0).sin() as f32).abs()).fold(0.0,f32::max);
        assert!(error<0.001,"converter delay error {error}");
    }
}
