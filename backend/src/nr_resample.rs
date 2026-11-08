//! Listener PCM <-> 48 kHz model PCM. Integer clocks survive arbitrary callback
//! boundaries; band-limited interpolation prevents images and aliasing. Each
//! channel owns its filters and its aligned, unfiltered residual-audio path.
use std::{collections::VecDeque, f64::consts::PI};

const MODEL_RATE: u32 = 48_000;
const PHASES: usize = 256;

struct Resampler {
    input_rate: u64,
    output_rate: u64,
    phase: u64,
    taps: Vec<Vec<f32>>,
    history: Vec<f32>,
    pos: usize,
    avx2: bool,
}

impl Resampler {
    fn radius(input: u32, output: u32) -> usize {
        32 * input.div_ceil(output).max(1) as usize
    }

    fn new(input: u32, output: u32, extra_delay: f64) -> Self {
        let radius = Self::radius(input, output) as f64;
        let delay = radius + extra_delay;
        let length = ((2.0 * radius + extra_delay).ceil() as usize + 2).next_multiple_of(8);
        let cutoff = 0.45 * (output as f64 / input as f64).min(1.0);
        let taps = (0..=PHASES).map(|phase| {
            let fraction = phase as f64 / PHASES as f64;
            let mut kernel: Vec<f32> = (0..length).rev().map(|k| {
                let x = k as f64 - delay - fraction;
                if x.abs() > radius { return 0.0; }
                let sinc = if x.abs() < 1e-12 { 2.0 * cutoff }
                    else { (2.0 * PI * cutoff * x).sin() / (PI * x) };
                let w = PI * x / radius;
                (sinc * (0.42 + 0.5 * w.cos() + 0.08 * (2.0 * w).cos())) as f32
            }).collect();
            let sum: f32 = kernel.iter().sum();
            for tap in &mut kernel { *tap /= sum; }
            kernel
        }).collect();
        #[cfg(target_arch = "x86_64")]
        let avx2 = std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma");
        #[cfg(not(target_arch = "x86_64"))]
        let avx2 = false;
        Self { input_rate: input as u64, output_rate: output as u64, phase: 0,
            taps, history: vec![0.0; 2 * length], pos: 0, avx2 }
    }

    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        output.clear();
        let length = self.history.len() / 2;
        for &sample in input {
            self.history[self.pos] = sample;
            self.history[self.pos + length] = sample;
            self.pos = (self.pos + 1) % length;
            self.phase += self.output_rate;
            // Output m samples after floor(n * output/input) input intervals.
            // End-of-interval origins cancel over the two converters, and a
            // round trip can never emit beyond the listener's input clock.
            while self.phase >= self.input_rate {
                self.phase -= self.input_rate;
                let scaled = self.phase * PHASES as u64;
                let phase = (scaled / self.output_rate) as usize;
                let blend = (scaled % self.output_rate) as f32 / self.output_rate as f32;
                let history = &self.history[self.pos..self.pos + length];
                let a = crate::narrow::dot(&self.taps[phase], history, self.avx2);
                let b = if blend == 0.0 { a }
                    else { crate::narrow::dot(&self.taps[phase + 1], history, self.avx2) };
                output.push(a + blend * (b - a));
            }
        }
    }
}

pub(super) struct NeuralRateAdapter {
    to_model: Resampler,
    from_model: Resampler,
    delay: Vec<f32>,
    delay_pos: usize,
    original: VecDeque<f32>,
    pub input: Vec<f32>,
    pub enhanced: Vec<f32>,
}

impl NeuralRateAdapter {
    pub fn new(rate: u32, model_delay: u32) -> Self {
        let before = Resampler::radius(rate, MODEL_RATE);
        let after = Resampler::radius(MODEL_RATE, rate);
        let ticks = (model_delay as u64 + after as u64) * rate as u64;
        let extra = (MODEL_RATE as u64 - ticks % MODEL_RATE as u64) % MODEL_RATE as u64;
        // Round the complete signal delay to an exact listener sample by adding
        // the remaining fraction to the return FIR. Dry/wet mixing and metadata
        // then share the same integral delay, including the 16 kHz model paths.
        let latency = before + (ticks + extra) as usize / MODEL_RATE as usize;
        Self { to_model: Resampler::new(rate, MODEL_RATE, 0.0),
            from_model: Resampler::new(MODEL_RATE, rate, extra as f64 / rate as f64),
            delay: vec![0.0; latency], delay_pos: 0, original: VecDeque::new(),
            input: Vec::new(), enhanced: Vec::new() }
    }

    pub fn latency(&self) -> u32 { self.delay.len() as u32 }

    pub fn ingest(&mut self, input: &[f32]) {
        for &sample in input {
            self.original.push_back(self.delay[self.delay_pos]);
            self.delay[self.delay_pos] = sample;
            self.delay_pos = (self.delay_pos + 1) % self.delay.len();
        }
        self.to_model.process(input, &mut self.input);
    }

    pub fn emit(&mut self, atten_db: f32, output: &mut Vec<f32>) -> Result<(), String> {
        self.from_model.process(&self.enhanced, output);
        let dry = 10.0_f32.powf(-atten_db / 20.0);
        for sample in output {
            let original = self.original.pop_front().ok_or("neural rate adapter exceeded input clock")?;
            *sample = (1.0 - dry) * *sample + dry * original;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(input: &[f32], from: u32, to: u32, packet: usize) -> Vec<f32> {
        let mut resampler = Resampler::new(from, to, 0.0);
        let mut result = Vec::new(); let mut block = Vec::new();
        for chunk in input.chunks(packet) { resampler.process(chunk, &mut block); result.extend_from_slice(&block); }
        assert_eq!(result.len(), input.len() * to as usize / from as usize);
        result
    }

    #[test]
    fn conversion_preserves_clocks_dc_and_packet_independence() {
        let input = vec![0.25; 10001];
        for (from,to) in [(12000,48000),(24000,48000),(48000,12000),(48000,24000),(44100,48000),(48000,8001),(192000,48000)] {
            let reference = convert(&input, from, to, input.len());
            assert_eq!(convert(&input, from, to, 137), reference);
            for sample in &reference[reference.len() / 2..] { assert!((sample - 0.25).abs() < 2e-6); }
        }
    }

    #[test]
    fn conversion_passes_voice_and_rejects_images_and_aliases() {
        let tone = |rate, hz: f64| (0..rate).map(|i| (2.0*PI*hz*i as f64/rate as f64).sin() as f32).collect::<Vec<_>>();
        let rms = |x: &[f32]| (x[x.len()/2..].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / (x.len()-x.len()/2) as f64).sqrt();
        for rate in [12000,24000] {
            let voice = convert(&tone(48000,1000.0),48000,rate,1799);
            assert!((rms(&voice) - 0.5_f64.sqrt()).abs() < 0.001);
            let alias = convert(&tone(48000,rate as f64*0.6),48000,rate,503);
            assert!(rms(&alias) < 0.001,"{rate}: alias RMS {}",rms(&alias));
            let up = convert(&tone(rate,1000.0),rate,48000,137);
            let image = (24000..48000).map(|i| up[i] as f64*(2.0*PI*(rate-1000) as f64*i as f64/48000.0).cos()).sum::<f64>()/24000.0;
            assert!(image.abs() < 0.001,"{rate}: image {image}");
        }
    }

    #[test]
    fn round_trip_impulse_and_dry_path_share_the_reported_integer_latency() {
        for rate in [12000,24000,44100,192000] {
            for model_delay in [512,1006,1440] {
                let mut adapter=NeuralRateAdapter::new(rate,model_delay);
                let delay=adapter.latency() as usize;
                let mut input=vec![0.0;delay+4000];input[1000]=1.0;
                let mut model_history=vec![0.0;model_delay as usize];let mut model_pos=0;
                let mut output=Vec::new();let mut block=Vec::new();
                for chunk in input.chunks(137) {
                    adapter.ingest(chunk);adapter.enhanced.clear();
                    for &sample in &adapter.input {
                        adapter.enhanced.push(model_history[model_pos]);
                        model_history[model_pos]=sample;model_pos=(model_pos+1)%model_history.len();
                    }
                    adapter.emit(100.0,&mut block).unwrap();output.extend_from_slice(&block);
                }
                assert!(output.len()<=input.len());
                let peak=output.iter().enumerate().max_by(|(_,a),(_,b)|a.abs().total_cmp(&b.abs())).unwrap().0;
                assert_eq!(peak,1000+delay,"{rate}/{model_delay}: declared {delay}");
                assert!(output[peak]>0.1);
            }
        }
    }
}
