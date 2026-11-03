//! Listener-only noise reduction; Wiener includes the existing downstream impulse blanker.
//! Raw digital decoder audio bypasses both stages.
//! DFN operates on 48 kHz mono PCM in 480-sample hops. Its model is embedded.
//! Reset drops lookahead, recurrent and overlap state on stream changes.
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{ArrayView2, ArrayViewMut2};
use websa_dsp::audio::wiener::Wiener;
use websa_dsp::audio::blanker::ImpulseBlanker;
use websa_dsp::plugin::AudioStage;
use std::cell::OnceCell;
use crate::nr_neural::NeuralStream;

const MODEL: &[u8] = include_bytes!("../../frontend/public/models/dfn/DeepFilterNet3_onnx.tar.gz");
const HOP: usize = 480;

thread_local! {
    // Tract uses Rc-backed tensors, so pristine runtimes stay on their owning
    // thread. Clones own recurrent/overlap state; this template never processes PCM.
    static DFN_TEMPLATE: OnceCell<Result<DfTract, String>> = const { OnceCell::new() };
}

/// Prepare the embedded model before a receiver thread starts accepting IQ.
/// Retain preparation errors for the existing requested-DFN/Wiener fallback.
pub fn prepare_dfn() {
    DFN_TEMPLATE.with(|template| { template.get_or_init(build_dfn); });
}

pub fn prepare_models() { prepare_dfn(); crate::nr_neural::prepare(); }

fn build_dfn() -> Result<DfTract, String> {
    let params = DfParams::from_bytes(MODEL).map_err(|e| format!("DFN model: {e:#}"))?;
    let runtime_params = RuntimeParams::default_with_ch(1);
    let runtime = DfTract::new(params, &runtime_params).map_err(|e| format!("DFN runtime: {e:#}"))?;
    if runtime.sr != 48000 || runtime.hop_size != HOP || runtime.ch != 1 {
        return Err("DFN model requires 48 kHz mono / 480-sample hops".into());
    }
    Ok(runtime)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NrAlgorithm { Off, Wiener, Dfn, Gtcrn, UlUnas, FastEnhancer }

impl NrAlgorithm {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value { "wiener"=>Self::Wiener, "dfn"=>Self::Dfn, "gtcrn"=>Self::Gtcrn,
            "ul-unas"=>Self::UlUnas, "fastenhancer"=>Self::FastEnhancer, _=>return None })
    }
    pub fn name(self) -> &'static str {
        match self { Self::Off=>"off",Self::Wiener=>"wiener",Self::Dfn=>"dfn",
            Self::Gtcrn=>"gtcrn",Self::UlUnas=>"ul-unas",Self::FastEnhancer=>"fastenhancer" }
    }
}

#[derive(Debug, Clone)]
pub struct NrStatus {
    pub requested: NrAlgorithm,
    pub effective: NrAlgorithm,
    pub state: &'static str,
    pub reason: Option<String>,
}

struct DfnStream {
    runtime: DfTract,
    input: [f32; HOP],
    filled: usize,
    enhanced: [f32; HOP],
}

impl DfnStream {
    fn new(atten_db: f32) -> Result<Self, String> {
        let mut runtime = DFN_TEMPLATE.with(|template| {
            template.get_or_init(build_dfn).clone()
        })?;
        runtime.set_atten_lim(atten_db);
        Ok(Self {
            runtime, input: [0.0; HOP], filled: 0, enhanced: [0.0; HOP],
        })
    }

    // Complete model hops only; at most 479 input samples carry over.
    fn process(&mut self, mut input: &[f32], output: &mut Vec<f32>) -> Result<(), String> {
        output.clear();
        output.reserve(input.len() + HOP);
        while !input.is_empty() {
            let count = (HOP - self.filled).min(input.len());
            self.input[self.filled..self.filled + count].copy_from_slice(&input[..count]);
            self.filled += count;
            input = &input[count..];
            if self.filled == HOP {
                let noisy = ArrayView2::from_shape((1, HOP), &self.input[..]).map_err(|e| e.to_string())?;
                let enhanced = ArrayViewMut2::from_shape((1, HOP), &mut self.enhanced[..]).map_err(|e| e.to_string())?;
                self.runtime.process(noisy, enhanced).map_err(|e| format!("DFN process: {e:#}"))?;
                output.extend_from_slice(&self.enhanced);
                self.filled = 0;
            }
        }
        Ok(())
    }
}

pub struct NoiseReduction {
    rate: u32,
    algorithm: NrAlgorithm,
    strength: f32,
    atten_db: f32,
    wiener: Wiener,
    wiener_input: [f32; 512],
    wiener_filled: usize,
    wiener_warmed: bool,
    wiener_output: Vec<f32>,
    blanker: ImpulseBlanker,
    blanker_output: Vec<f32>,
    dfn: Option<DfnStream>,
    neural: Option<NeuralStream>,
    failure: Option<String>,
}

impl NoiseReduction {
    pub fn new(rate: u32) -> Self {
        Self {
            rate, algorithm: NrAlgorithm::Off, strength: 1.0, atten_db: 6.0,
            wiener: Wiener::new(), dfn: None, neural: None, failure: None,
            wiener_input: [0.0; 512], wiener_filled: 0, wiener_warmed: false,
            wiener_output: Vec::with_capacity(512),
            blanker: ImpulseBlanker::new(), blanker_output: Vec::with_capacity(512),
        }
    }

    /// Algorithm/rate changes start a fresh stream. Strength and attenuation
    /// apply live without dropping the current stream's samples.
    pub fn configure(&mut self, rate: u32, algorithm: NrAlgorithm, strength: f32, atten_db: f32) {
        let changed = self.rate != rate || self.algorithm != algorithm;
        self.rate = rate;
        self.algorithm = algorithm;
        self.strength = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { 1.0 };
        self.atten_db = if atten_db.is_finite() { atten_db.clamp(0.0, 100.0) } else { 6.0 };
        self.wiener.set_strength(self.strength as f64);
        self.blanker.set_strength(self.strength as f64);
        if changed { self.reset(); }
        else {
            if let Some(dfn) = &mut self.dfn { dfn.runtime.set_atten_lim(self.atten_db); }
            if let Some(neural) = &mut self.neural { neural.set_atten(self.atten_db); }
        }
    }

    /// Clone pristine state: init() alone does not clear the runtime's GRU state.
    /// Model parsing and compilation are cached before active reception.
    pub fn reset(&mut self) {
        self.dfn = None;
        self.neural = None;
        self.failure = None;
        self.wiener.reset();
        self.blanker.reset();
        self.blanker_output.clear();
        self.wiener_filled = 0;
        self.wiener_warmed = false;
        self.wiener_output.clear();
        if self.algorithm == NrAlgorithm::Dfn {
            if self.rate != 48000 {
                self.failure = Some(format!("rate {}", self.rate));
            } else {
                match DfnStream::new(self.atten_db) {
                    Ok(dfn) => self.dfn = Some(dfn),
                    Err(error) => self.failure = Some(error),
                }
            }
        }
        if matches!(self.algorithm,NrAlgorithm::Gtcrn|NrAlgorithm::UlUnas|NrAlgorithm::FastEnhancer) {
            if self.rate != 48000 { self.failure = Some(format!("rate {}",self.rate)); }
            else { match NeuralStream::new(self.algorithm,self.atten_db) {
                Ok(neural)=>self.neural=Some(neural), Err(error)=>self.failure=Some(error),
            } }
        }
    }

    pub fn status(&self) -> NrStatus {
        let (effective, state) = match self.algorithm {
            NrAlgorithm::Off => (NrAlgorithm::Off, "off"),
            NrAlgorithm::Wiener => (NrAlgorithm::Wiener, "wiener"),
            NrAlgorithm::Dfn if self.dfn.is_some() => (NrAlgorithm::Dfn, "ready"),
            NrAlgorithm::Dfn => (NrAlgorithm::Wiener, "fallback"),
            algorithm if self.neural.is_some() => (algorithm, "ready"),
            _ => (NrAlgorithm::Wiener, "fallback"),
        };
        NrStatus { requested: self.algorithm, effective, state, reason: self.failure.clone() }
    }
    /// Wiener needs a 512-sample window before emitting its first 128 samples.
    /// DFN delay is its STFT overlap plus model lookahead, in output samples.
    pub fn latency_samples(&self) -> Option<u32> {
        Some(if self.algorithm == NrAlgorithm::Off { 0 }
            else if let Some(dfn) = &self.dfn {
                if dfn.runtime.atten_lim == Some(1.0) {0}
                else {(dfn.runtime.fft_size - dfn.runtime.hop_size
                    + dfn.runtime.lookahead * dfn.runtime.hop_size) as u32}
            } else if let Some(neural) = &self.neural { neural.latency() as u32
            } else { 512 - 128 })
    }

    pub fn model_id(&self) -> Option<&'static str> {
        if self.status().state != "ready" {return None;}
        Some(match self.algorithm { NrAlgorithm::Dfn=>"DeepFilterNet3",
            NrAlgorithm::Gtcrn=>"gtcrn-stream-dns3-epoch96",
            NrAlgorithm::UlUnas=>"ul-unas-dns3-stream-00f7c700",
            NrAlgorithm::FastEnhancer=>"fastenhancer-t-48khz-v1", _=>return None })
    }


    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<(), String> {
        if self.algorithm == NrAlgorithm::Off {
            output.clear();
            output.extend_from_slice(input);
            return Ok(());
        }
        if let Some(dfn) = &mut self.dfn {
            // A failed hop may already have consumed PCM. Do not replay it through another reducer.
            return dfn.process(input, output);
        }
        if let Some(neural) = &mut self.neural {return neural.process(input,output);}
        output.clear();
        output.reserve(input.len() + 128);
        let mut input = input;
        while !input.is_empty() {
            let needed = if self.wiener_warmed { 128 } else { 512 };
            let count = (needed - self.wiener_filled).min(input.len());
            self.wiener_input[self.wiener_filled..self.wiener_filled + count]
                .copy_from_slice(&input[..count]);
            self.wiener_filled += count;
            input = &input[count..];
            if self.wiener_filled == needed {
                self.wiener.process_into(&self.wiener_input[..needed], false, &mut self.wiener_output);
                // The first call's trailing 384 values are the stage's padding.
                output.extend_from_slice(&self.wiener_output[..128]);
                self.wiener_filled = 0;
                self.wiener_warmed = true;
            }
        }
        // Retain the original listener policy order: Wiener, then impulse repair.
        // DFN initialization failure uses this Wiener path.
        self.blanker.process_into(output, false, &mut self.blanker_output);
        std::mem::swap(output, &mut self.blanker_output);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEURAL: [NrAlgorithm;4] = [NrAlgorithm::Dfn,NrAlgorithm::Gtcrn,NrAlgorithm::UlUnas,NrAlgorithm::FastEnhancer];
    fn signal() -> Vec<f32> {
        // Include an immediate onset, a long silent pause, wideband content and
        // a final nonzero sample: delay compensation must preserve both ends.
        (0..8191).map(|i| if (3000..4500).contains(&i) {0.0}
            else {0.08*(i as f32*0.17).cos()+0.03*(i as f32*1.77).sin()}).collect()
    }
    fn run(nr:&mut NoiseReduction,input:&[f32],packet:usize) -> Vec<f32> {
        let mut output=Vec::new();let mut block=Vec::new();
        for chunk in input.chunks(packet) {nr.process(chunk,&mut block).unwrap();output.extend_from_slice(&block);}
        output
    }
    #[test]
    fn neural_zero_db_is_aligned_original_including_silence_and_tail() {
        let x=signal();
        for algorithm in NEURAL {
            let mut nr=NoiseReduction::new(48000);nr.configure(48000,algorithm,1.0,0.0);
            assert_eq!(nr.status().effective,algorithm,"{:?}",nr.status());
            let delay=nr.latency_samples().unwrap() as usize;
            let mut padded=x.clone();padded.resize(x.len()+delay+4096,0.0);
            let out=run(&mut nr,&padded,137);
            for (i,(&a,&b)) in out[delay..delay+x.len()].iter().zip(&x).enumerate() {
                assert!((a-b).abs()<3e-6,"{algorithm:?} alignment at {i}: {a} != {b}, delay {delay}");
            }
        }
    }
    #[test]
    fn neural_packets_caches_reset_and_attenuation_are_independent() {
        let input=signal();
        for algorithm in NEURAL {
            let mut nr=NoiseReduction::new(48000);nr.configure(48000,algorithm,1.0,20.0);
            assert_eq!(nr.status().effective,algorithm,"{:?}",nr.status());
            let a=run(&mut nr,&input,1799);
            assert!(a.iter().all(|x|x.is_finite()));
            assert!(a.iter().any(|x|x.abs()>1e-5),"{algorithm:?} must run a real non-silent model");
            let mut independent=NoiseReduction::new(48000);independent.configure(48000,algorithm,1.0,20.0);
            let b=run(&mut independent,&input,503);
            assert_eq!(a,b,"{algorithm:?}: callback sizes must not change model frames or caches");
            nr.reset();assert_eq!(run(&mut nr,&input,1),b);
            nr.configure(48000,algorithm,1.0,100.0);
            let at=run(&mut nr,&input,503);assert_ne!(at,b,"attenuation must change rendered samples");
            nr.configure(24000,algorithm,1.0,20.0);
            assert_eq!(nr.status().state,"fallback");assert_eq!(nr.status().effective,NrAlgorithm::Wiener);
        }
    }

    #[test]
    fn prepared_dfn_keeps_streams_and_resets_independent() {
        let mut used = NoiseReduction::new(48000);
        used.configure(48000, NrAlgorithm::Dfn, 1.0, 12.0);
        let input: Vec<f32> = (0..HOP * 8)
            .map(|n| (n as f32 * 0.071).sin() * 0.1).collect();
        let mut output = Vec::new();
        used.process(&input, &mut output).unwrap();
        assert_eq!(used.status().effective, NrAlgorithm::Dfn, "{:?}", used.status());

        let mut fresh = NoiseReduction::new(48000);
        fresh.configure(48000, NrAlgorithm::Dfn, 1.0, 12.0);
        assert_eq!(fresh.status().effective, NrAlgorithm::Dfn, "{:?}", fresh.status());
        let mut expected = Vec::new();
        fresh.process(&input, &mut expected).unwrap();
        assert_eq!(fresh.status().effective, NrAlgorithm::Dfn, "{:?}", fresh.status());
        used.reset();
        used.process(&input, &mut output).unwrap();
        assert_eq!(used.status().effective, NrAlgorithm::Dfn, "{:?}", used.status());
        assert_eq!(output, expected, "reset must restore pristine recurrent and overlap state");

        fresh.configure(24000, NrAlgorithm::Dfn, 1.0, 12.0);
        let status = fresh.status();
        assert_eq!(status.requested, NrAlgorithm::Dfn);
        assert_eq!(status.effective, NrAlgorithm::Wiener);
        assert_eq!(status.reason.as_deref(), Some("rate 24000"));
    }
}
