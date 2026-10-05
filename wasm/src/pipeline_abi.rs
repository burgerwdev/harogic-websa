//! The receive demodulator over the ABI: one handle, one call per baseband block.
//!
//! The baseband handed in is already channelized (the backend's hardware DDC + software NCO), so
//! what this entry point runs is the demodulator and the audio chain. The TypeScript side owns the
//! transport and the worklet; the orchestration (which stage runs in which order, on which path)
//! stays in Rust, where the path separation is enforced by types.
//!
//! Analog path: `websa_dsp_demod_process` produces PCM. Digital path: `websa_dsp_demod_push` and
//! `websa_dsp_demod_message_at` produce decoded text. One handle table serves both, so a mode that
//! changes from `am` to `ft8` is one free plus one new.

use core::cell::RefCell;

use crate::analog;
use crate::audio::{self, AudioPolicy};
use crate::digital;
use crate::pipeline::{PathKind, Pipeline, PipelineOutput};
use crate::plugin::DemodConfig;

struct PipelineState {
    pipeline: Pipeline,
    out: PipelineOutput,
}

thread_local! {
    static PIPELINES: RefCell<Vec<Option<PipelineState>>> = const { RefCell::new(Vec::new()) };
}

fn with_pipeline<R>(handle: u32, f: impl FnOnce(&mut PipelineState) -> R) -> Option<R> {
    if handle == 0 {
        return None;
    }
    PIPELINES.with(|slot| {
        let mut registry = slot.borrow_mut();
        registry
            .get_mut(handle as usize - 1)
            .and_then(|entry| entry.as_mut())
            .map(f)
    })
}

unsafe fn read_str(ptr: *const u8, len: u32) -> Option<String> {
    if ptr.is_null() || len == 0 || len > 32 {
        return None;
    }
    let bytes = core::slice::from_raw_parts(ptr, len as usize);
    core::str::from_utf8(bytes).ok().map(|s| s.to_string())
}

/// Largest baseband block this ABI accepts, in complex samples (8 MiB of f32 pairs).
///
/// The bound is a guard, not a performance limit: `samples as usize * 2` is computed on wasm32,
/// where `usize` is 32 bits, so any `samples >= 2^31` wraps and hands `from_raw_parts` a length that
/// is not the one the caller allocated. The browser feeds 32k-sample blocks, so this is ~32x the
/// largest real call.
const MAX_PUSH_SAMPLES: u32 = 1 << 20;

fn register(state: PipelineState) -> u32 {
    PIPELINES.with(|slot| {
        let mut registry = slot.borrow_mut();
        for (index, entry) in registry.iter_mut().enumerate() {
            if entry.is_none() {
                *entry = Some(state);
                return index as u32 + 1;
            }
        }
        registry.push(Some(state));
        registry.len() as u32
    })
}

/// Build a demodulator for `mode` (its plugin id, e.g. `usb` or `ft8`).
///
/// `fs_in` is the rate of the channelized baseband (`STATUS.sdr.actual.ddc_rate`) and `out_rate` is
/// the rate the consumer wants: the AudioWorklet's `sampleRate` for an analog mode, 48 kHz for a
/// protocol decoder. Returns 0 when the mode has no kernel or the geometry is invalid.
///
/// # Safety
/// `mode_ptr` must point to `mode_len` readable bytes (an ASCII plugin id).
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_demod_new(
    fs_in: f64,
    out_rate: f64,
    mode_ptr: *const u8,
    mode_len: u32,
    if_bw: f64,
    pitch: f64,
) -> u32 {
    let Some(mode) = read_str(mode_ptr, mode_len) else {
        return 0;
    };
    if !(fs_in > 0.0) || !(out_rate > 0.0) {
        return 0;
    }
    let mut config = DemodConfig::new(fs_in, if_bw);
    // The CW sidetone (and nothing else) uses it; the registry's modes ignore it.
    config.pitch = pitch;
    if let Some(demod) = analog::build(&mode, config, out_rate) {
        // The policy is the listener's (the panel's NR/squelch); its default is "no enhancement",
        // which is what the Python reference runs, so the two paths can be compared.
        let chain = audio::chain_for_mode(&mode, out_rate, AudioPolicy::default());
        let mut pipeline = Pipeline::new(PathKind::Analog, chain, AudioPolicy::default());
        pipeline.set_analog_demod(Box::new(demod));
        return register(PipelineState { pipeline, out: PipelineOutput::default() });
    }
    if let Some(demod) = digital::build(&mode, out_rate) {
        let blank = audio::chain_for_mode(&mode, out_rate, AudioPolicy::default());
        let mut pipeline = Pipeline::new(PathKind::Digital, blank, AudioPolicy::default());
        pipeline.set_digital_demod(demod, fs_in, out_rate);
        return register(PipelineState { pipeline, out: PipelineOutput::default() });
    }
    0
}

/// Run one baseband block through the analog pipeline; returns the PCM samples written (0 for a
/// digital handle: its output is text, not audio).
///
/// The audio chain may be switched off with `websa_dsp_demod_set_audio` (the RAW/digital comparison
/// uses that); it never changes the RAW baseband.
///
/// # Safety
/// `iq_ptr` must point to `samples * 2` readable f32 values and `out_ptr` to `capacity` writable
/// f32 values, both inside this module's linear memory.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_demod_process(
    handle: u32,
    iq_ptr: *const f32,
    samples: u32,
    out_ptr: *mut f32,
    capacity: u32,
    audio_hold: u32,
) -> u32 {
    if iq_ptr.is_null() || out_ptr.is_null() || samples == 0 || samples > MAX_PUSH_SAMPLES {
        return 0;
    }
    let input = core::slice::from_raw_parts(iq_ptr, samples as usize * 2);
    let output = core::slice::from_raw_parts_mut(out_ptr, capacity as usize);
    with_pipeline(handle, |state| {
        if state.pipeline.kind() != PathKind::Analog {
            return 0;
        }
        state.pipeline.process_f32_into(input, audio_hold != 0, &mut state.out);
        let written = state.out.audio.len();
        if written > capacity as usize {
            return 0;                        // never write past the caller's block
        }
        output[..written].copy_from_slice(&state.out.audio);
        written as u32
    })
    .unwrap_or(0)
}

/// Feed one baseband block to the digital pipeline; returns how many messages this block decoded
/// (0 when it completed nothing).
///
/// # Safety
/// `iq_ptr` must point to `samples * 2` readable f32 values inside this module's linear memory.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_demod_push(handle: u32, iq_ptr: *const f32, samples: u32) -> u32 {
    if iq_ptr.is_null() || samples == 0 || samples > MAX_PUSH_SAMPLES {
        return 0;
    }
    let input = core::slice::from_raw_parts(iq_ptr, samples as usize * 2);
    with_pipeline(handle, |state| {
        if state.pipeline.kind() != PathKind::Digital {
            return 0;
        }
        state.pipeline.process_f32_into(input, false, &mut state.out);
        state.out.decoded.len() as u32
    })
    .unwrap_or(0)
}

/// Copy the `index`-th decoded message of the last block into `text_ptr` and its measurements
/// into `metrics_ptr` (`[frequency_hz, time_offset_s, snr_db]`). Returns the text length, or 0
/// when there is no such message.
///
/// # Safety
/// `text_ptr` must point to `text_capacity` writable bytes and `metrics_ptr` to 3 writable f64s.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_demod_message_at(
    handle: u32,
    index: u32,
    text_ptr: *mut u8,
    text_capacity: u32,
    metrics_ptr: *mut f64,
) -> u32 {
    if text_ptr.is_null() || metrics_ptr.is_null() {
        return 0;
    }
    with_pipeline(handle, |state| {
        let Some((text, report)) = state.pipeline.digital_decoded().into_iter().nth(index as usize)
        else {
            return 0;
        };
        if text.len() > text_capacity as usize {
            return 0;
        }
        core::ptr::copy_nonoverlapping(text.as_ptr(), text_ptr, text.len());
        let metrics = core::slice::from_raw_parts_mut(metrics_ptr, 3);
        metrics[0] = report.frequency_hz;
        metrics[1] = report.time_offset_s;
        metrics[2] = report.snr_db;
        text.len() as u32
    })
    .unwrap_or(0)
}

/// Complex samples the digital decoder has buffered towards its next attempt.
#[no_mangle]
pub extern "C" fn websa_dsp_demod_buffered(handle: u32) -> u32 {
    with_pipeline(handle, |state| state.pipeline.digital_buffered() as u32).unwrap_or(0)
}

/// Copy the digital demodulator's constellation points (interleaved I,Q `f64` pairs) into
/// `ptr`. Returns the number of points, or 0 when there are none.
///
/// # Safety
/// `ptr` must point to `capacity * 2` writable `f64`s.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_drm_constellation(handle: u32, ptr: *mut f64, capacity: u32) -> u32 {
    if ptr.is_null() {
        return 0;
    }
    with_pipeline(handle, |state| {
        let points = state.pipeline.digital_constellation();
        let n = points.len().min(capacity as usize);
        let out = core::slice::from_raw_parts_mut(ptr, 2 * n);
        for (i, (re, im)) in points.into_iter().take(n).enumerate() {
            out[2 * i] = re;
            out[2 * i + 1] = im;
        }
        n as u32
    })
    .unwrap_or(0)
}

/// Copy the digital demodulator's decoded audio (16-bit interleaved PCM) into `ptr`
/// and drain it: the call returns everything accumulated since the previous call and
/// clears the buffer, so a streaming consumer can never fall behind a growing one.
/// Returns the number of samples, or 0 when there is none. The sample rate is reported
/// by [`websa_dsp_drm_audio_rate`].
///
/// # Safety
/// `ptr` must point to `capacity` writable `i16`s.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_drm_audio_pcm(handle: u32, ptr: *mut i16, capacity: u32) -> u32 {
    if ptr.is_null() {
        return 0;
    }
    with_pipeline(handle, |state| {
        let pcm = state.pipeline.digital_audio_pcm_limited(capacity as usize);
        let n = pcm.len().min(capacity as usize);
        core::ptr::copy_nonoverlapping(pcm.as_ptr(), ptr, n);
        n as u32
    })
    .unwrap_or(0)
}

/// The decoded audio PCM's sample rate in Hz (0 when there is no audio).
#[no_mangle]
pub extern "C" fn websa_dsp_drm_audio_rate(handle: u32) -> u32 {
    with_pipeline(handle, |state| state.pipeline.digital_audio_rate_hz()).unwrap_or(0)
}


/// Messages decoded by this pipeline (status readout).
#[no_mangle]
pub extern "C" fn websa_dsp_demod_count(handle: u32) -> u32 {
    with_pipeline(handle, |state| state.pipeline.decoded_total() as u32).unwrap_or(0)
}

/// Noise reduction on/off plus its strength (0..1) — the panel's NR control.
#[no_mangle]
pub extern "C" fn websa_dsp_demod_set_nr(handle: u32, enabled: u32, strength: f64) -> u32 {
    with_pipeline(handle, |state| {
        u32::from(state.pipeline.set_noise_reduction(enabled != 0, strength))
    })
    .unwrap_or(0)
}

/// The de-emphasis time constant in microseconds (`<= 0` = none) — the panel's De-emph control.
#[no_mangle]
pub extern "C" fn websa_dsp_demod_set_deemph(handle: u32, tau_us: f64) -> u32 {
    with_pipeline(handle, |state| u32::from(state.pipeline.set_deemph_us(tau_us))).unwrap_or(0)
}

/// The de-emphasis in force, in microseconds (0 = none): what the panel's setting resolved to.
#[no_mangle]
pub extern "C" fn websa_dsp_demod_deemph(handle: u32) -> f64 {
    with_pipeline(handle, |state| state.pipeline.deemph_us()).unwrap_or(-1.0)
}

/// The squelch gate's threshold in dBFS — the panel's squelch control.
#[no_mangle]
pub extern "C" fn websa_dsp_demod_set_squelch(handle: u32, dbfs: f64) -> u32 {
    with_pipeline(handle, |state| u32::from(state.pipeline.set_squelch_dbfs(dbfs))).unwrap_or(0)
}

/// Turn the audio-enhancement chain on/off (the RAW-path separation test uses this).
#[no_mangle]
pub extern "C" fn websa_dsp_demod_set_audio(handle: u32, enabled: u32) -> u32 {
    with_pipeline(handle, |state| {
        state.pipeline.set_audio_enabled(enabled != 0);
        1
    })
    .unwrap_or(0)
}

/// The backend retuned the channel: drop the demodulator's history, keep the level control.
#[no_mangle]
pub extern "C" fn websa_dsp_demod_retune(handle: u32) -> u32 {
    with_pipeline(handle, |state| {
        state.pipeline.retune();
        1
    })
    .unwrap_or(0)
}

/// Clear all streaming state (a baseband stream restart / discontinuity).
#[no_mangle]
pub extern "C" fn websa_dsp_demod_reset(handle: u32) -> u32 {
    with_pipeline(handle, |state| {
        state.pipeline.reset();
        1
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn websa_dsp_demod_free(handle: u32) -> u32 {
    if handle == 0 {
        return 0;
    }
    PIPELINES.with(|slot| {
        let mut registry = slot.borrow_mut();
        match registry.get_mut(handle as usize - 1) {
            Some(entry) if entry.is_some() => {
                *entry = None;
                1
            }
            _ => 0,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::AnalogDemodulator;
    use core::f64::consts::PI;

    const FS_IN: f64 = 48_000.0;
    const OUT_RATE: f64 = 48_000.0;

    fn new(mode: &str, if_bw: f64, out_rate: f64) -> u32 {
        unsafe {
            websa_dsp_demod_new(FS_IN, out_rate, mode.as_ptr(), mode.len() as u32, if_bw, 700.0)
        }
    }

    /// An amplitude-modulated baseband at DC, the shape the backend's DDC hands over.
    fn am_baseband(n: usize, tone_hz: f64) -> Vec<f32> {
        // Two modulation tones, not one: the default chain includes the adaptive notch, which
        // removes the strongest single tone in the channel (correct for interference, and the
        // reason a pure-tone test signal comes out quiet). With two tones the second one survives,
        // so the level assertion measures the pipeline rather than the notch.
        let mut iq = Vec::with_capacity(n * 2);
        for k in 0..n {
            let t = k as f64 / FS_IN;
            let envelope = 1.0
                + 0.4 * (2.0 * PI * tone_hz * t).cos()
                + 0.4 * (2.0 * PI * tone_hz * 1.7 * t).cos();
            iq.push((envelope * 0.25) as f32);
            iq.push(0.0);
        }
        iq
    }

    #[test]
    fn the_demod_abi_produces_audio_from_a_baseband_block() {
        let handle = new("am", 12_000.0, OUT_RATE);
        assert_ne!(handle, 0, "the demodulator must be created");
        let iq = am_baseband(4_096, 1_000.0);
        let capacity = 8_192_u32;
        let in_ptr = crate::abi::websa_dsp_alloc(iq.len() * 4) as *mut f32;
        let out_ptr = crate::abi::websa_dsp_alloc(capacity as usize * 4) as *mut f32;
        assert!(!in_ptr.is_null() && !out_ptr.is_null());
        // SAFETY: both blocks were allocated with the sizes used here.
        unsafe {
            core::ptr::copy_nonoverlapping(iq.as_ptr(), in_ptr, iq.len());
            let written = websa_dsp_demod_process(handle, in_ptr, (iq.len() / 2) as u32,
                                                  out_ptr, capacity, 0);
            assert!(written > 0, "the demodulator must produce PCM");
            let pcm = core::slice::from_raw_parts(out_ptr, written as usize);
            let rms = (pcm.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt();
            // The ABI-level claim is "a baseband block in, PCM out": the exact level belongs to the
            // chain's policy.
            assert!(rms > 0.005, "the demodulator must produce audio, rms {rms}");
            assert_eq!(websa_dsp_demod_retune(handle), 1);
            assert_eq!(websa_dsp_demod_reset(handle), 1);
            assert_eq!(websa_dsp_demod_free(handle), 1);
            assert_eq!(websa_dsp_demod_process(handle, in_ptr, 16, out_ptr, capacity, 0), 0,
                       "a freed handle must not process");
            crate::abi::websa_dsp_free(in_ptr as *mut u8, iq.len() * 4);
            crate::abi::websa_dsp_free(out_ptr as *mut u8, capacity as usize * 4);
        }
    }

    /// The consumer's rate is what comes back: a 44.1 kHz AudioContext must not be handed 48 kHz.
    #[test]
    fn the_audio_rate_follows_the_configured_output_rate() {
        let iq = am_baseband(48_000, 1_000.0);
        let capacity = 32_768_u32;
        let in_ptr = crate::abi::websa_dsp_alloc(iq.len() * 4) as *mut f32;
        let out_ptr = crate::abi::websa_dsp_alloc(capacity as usize * 4) as *mut f32;
        // SAFETY: both blocks are allocated above with these sizes.
        unsafe {
            core::ptr::copy_nonoverlapping(iq.as_ptr(), in_ptr, iq.len());
            let complex = iq.len() / 2;
            for out_rate in [48_000.0_f64, 44_100.0] {
                let handle = new("am", 12_000.0, out_rate);
                assert_ne!(handle, 0);
                let mut total = 0usize;
                let block = 4_096usize;
                for start in (0..complex).step_by(block) {
                    let count = block.min(complex - start);
                    let written = websa_dsp_demod_process(
                        handle, in_ptr.add(start * 2), count as u32, out_ptr, capacity, 0);
                    total += written as usize;
                }
                let expected = complex as f64 / FS_IN * out_rate;
                assert!(
                    (total as f64 - expected).abs() < expected * 0.01,
                    "out_rate {out_rate}: produced {total} samples, expected about {expected:.0}"
                );
                assert_eq!(websa_dsp_demod_free(handle), 1);
            }
            crate::abi::websa_dsp_free(in_ptr as *mut u8, iq.len() * 4);
            crate::abi::websa_dsp_free(out_ptr as *mut u8, capacity as usize * 4);
        }
    }

    #[test]
    fn an_unknown_mode_is_refused_and_the_audio_switch_is_reported() {
        assert_eq!(new("nope", 12_000.0, OUT_RATE), 0, "a mode without a kernel must not create one");

        let handle = new("usb", 2_400.0, OUT_RATE);
        assert_ne!(handle, 0);
        assert_eq!(websa_dsp_demod_set_audio(handle, 0), 1);
        // The chain is off; the demodulator still produces PCM (the detector is not the chain).
        let iq = am_baseband(4_096, 1_000.0);
        let in_ptr = crate::abi::websa_dsp_alloc(iq.len() * 4) as *mut f32;
        let out_ptr = crate::abi::websa_dsp_alloc(4_096 * 4) as *mut f32;
        // SAFETY: both blocks are allocated above.
        unsafe {
            core::ptr::copy_nonoverlapping(iq.as_ptr(), in_ptr, iq.len());
            let written = websa_dsp_demod_process(handle, in_ptr, (iq.len() / 2) as u32,
                                                  out_ptr, 4_096, 0);
            assert!(written > 0);
            websa_dsp_demod_free(handle);
            crate::abi::websa_dsp_free(in_ptr as *mut u8, iq.len() * 4);
            crate::abi::websa_dsp_free(out_ptr as *mut u8, 4_096 * 4);
        }
    }

    #[test]
    fn a_digital_mode_produces_text_and_no_audio() {
        if cfg!(debug_assertions) {
            return;
        }
        // The fixture is the channelized 48 kHz baseband of a real CQ transmission, which is exactly
        // what the backend now hands the decoder.
        const BYTES: &[u8] = include_bytes!("../../tests/fixtures/ft8/ft8_cq_iq.bin");
        let mut iq: Vec<f32> = BYTES
            .chunks_exact(4)
            .map(|q| f32::from_le_bytes([q[0], q[1], q[2], q[3]]))
            .collect();
        // A whole sliding window, not just a slot: the decoder searches `hop + burst` so that a
        // transmission is found whatever phase the feeder's window boundaries have.
        iq.resize(crate::digital::ft8::Ft8Decoder::new(FS_IN as f64).window_samples() * 2, 0.0);
        let handle = new("ft8", OUT_RATE, FS_IN);
        assert_ne!(handle, 0, "the decoder must be created");
        let ptr = crate::abi::websa_dsp_alloc(iq.len() * 4) as *mut f32;
        let mut text = vec![0_u8; 64];
        let mut metrics = [0.0_f64; 3];
        // SAFETY: the block is allocated with the size used here; the buffers are large enough.
        unsafe {
            core::ptr::copy_nonoverlapping(iq.as_ptr(), ptr, iq.len());
            let mut decoded = 0;
            for block in 0..(iq.len() / 2).div_ceil(8_192) {
                let start = block * 8_192;
                let count = (8_192).min(iq.len() / 2 - start);
                if count <= 0 {
                    break;
                }
                decoded = decoded.max(websa_dsp_demod_push(handle, ptr.add(start * 2), count as u32));
            }
            assert_eq!(decoded, 1, "the slot must decode");
            let written = websa_dsp_demod_message_at(
                handle, 0, text.as_mut_ptr(), text.len() as u32, metrics.as_mut_ptr());
            assert_eq!(&text[..written as usize], b"CQ JO1WKO PM95");
            assert_eq!(websa_dsp_demod_count(handle), 1);
            assert!(metrics[0] > 900.0 && metrics[0] < 1_100.0, "frequency {}", metrics[0]);
            // A digital handle has no audio: the analog entry point refuses it.
            let out_ptr = crate::abi::websa_dsp_alloc(1_024 * 4) as *mut f32;
            assert_eq!(websa_dsp_demod_process(handle, ptr, 16, out_ptr, 1_024, 0), 0);
            crate::abi::websa_dsp_free(out_ptr as *mut u8, 1_024 * 4);
            assert_eq!(websa_dsp_demod_free(handle), 1);
            assert_eq!(websa_dsp_demod_push(handle, ptr, 16), 0, "a freed handle must not process");
            crate::abi::websa_dsp_free(ptr as *mut u8, iq.len() * 4);
        }
    }

    #[test]
    fn the_analog_demod_used_by_the_pipeline_is_the_registered_one() {
        let mode = "nfm";
        let demod = analog::build(mode, DemodConfig::new(FS_IN, 12_000.0), OUT_RATE).expect("nfm builds");
        assert_eq!(demod.id(), mode);
    }
}
