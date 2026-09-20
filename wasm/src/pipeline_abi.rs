//! The whole receive pipeline over the ABI: one handle, one call per IQ block.
//!
//! The TypeScript side owns the transport and the worklet; the orchestration (which stage runs in
//! which order, on which path) stays in Rust, where the path separation is enforced by types. The
//! worker therefore allocates one IQ block and one PCM block, writes the samples in, and calls
//! `websa_dsp_pipeline_process` — nothing else.
//!
//! Analog path only: this entry point produces PCM. The raw/digital consumers read the RAW stream
//! through the DDC entry points and never touch this one.

use core::cell::RefCell;

use crate::analog;
use crate::audio;
use crate::ddc::{Ddc, DdcConfig};
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

/// Build the analog receive pipeline for `mode` (its id, e.g. `usb`).
///
/// Returns 0 when the mode has no kernel or the geometry is invalid.
///
/// # Safety
/// `mode_ptr` must point to `mode_len` readable bytes (an ASCII plugin id).
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_pipeline_new(
    fs_in: f64,
    offset_hz: f64,
    decimate: u32,
    out_rate: f64,
    mode_ptr: *const u8,
    mode_len: u32,
    if_bw: f64,
    pitch: f64,
) -> u32 {
    let Some(mode) = read_str(mode_ptr, mode_len) else {
        return 0;
    };
    if !(fs_in > 0.0) || decimate == 0 {
        return 0;
    }
    let config = DdcConfig {
        fs_in,
        offset_hz,
        // Anti-alias corner just inside the decimated Nyquist.
        cutoff_hz: (fs_in / decimate as f64) * 0.4,
        decimate: decimate as usize,
        out_rate,
        ntaps: 129,
    };
    let ddc = Ddc::new(config);
    let mut chain = audio::chain_for_mode(&mode);
    // The demodulator already runs the reference AGC (that is what the Python parity fixtures pin
    // down), so the chain's AGC is disabled here rather than left in series to fight it.
    chain.set_stage_enabled("agc", false);
    let mut pipeline = Pipeline::new(ddc, PathKind::Analog, chain);
    let mut demod_config = DemodConfig::new(out_rate, if_bw);
    demod_config.pitch = pitch;
    let Some(demod) = analog::build(&mode, demod_config) else {
        return 0;
    };
    pipeline.set_analog_demod(Box::new(demod));
    let state = PipelineState { pipeline, out: PipelineOutput::default() };

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

/// Run one int16 IQ block through the pipeline; returns the PCM samples written.
///
/// The audio chain may be switched off with `websa_dsp_pipeline_set_audio` (the RAW/digital
/// comparison uses that); it never changes the RAW stream.
///
/// # Safety
/// `iq_ptr` must point to `samples * 2` readable int16 values and `out_ptr` to `capacity` writable
/// f32 values, both inside this module's linear memory.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_pipeline_process(
    handle: u32,
    iq_ptr: *const i16,
    samples: u32,
    out_ptr: *mut f32,
    capacity: u32,
    audio_hold: u32,
) -> u32 {
    if iq_ptr.is_null() || out_ptr.is_null() || samples == 0 {
        return 0;
    }
    let input = core::slice::from_raw_parts(iq_ptr, samples as usize * 2);
    let output = core::slice::from_raw_parts_mut(out_ptr, capacity as usize);
    with_pipeline(handle, |state| {
        state.pipeline.process_i16_into(input, audio_hold != 0, &mut state.out);
        let written = state.out.audio.len();
        if written > capacity as usize {
            return 0;                        // never write past the caller's block
        }
        output[..written].copy_from_slice(&state.out.audio);
        written as u32
    })
    .unwrap_or(0)
}

/// Retune: a new NCO offset (and demod retune), keeping the level control.
#[no_mangle]
pub extern "C" fn websa_dsp_pipeline_retune(handle: u32, offset_hz: f64) -> u32 {
    with_pipeline(handle, |state| {
        state.pipeline.retune(offset_hz);
        1
    })
    .unwrap_or(0)
}

/// Turn the audio-enhancement chain on/off (the RAW-path separation test uses this).
#[no_mangle]
pub extern "C" fn websa_dsp_pipeline_set_audio(handle: u32, enabled: u32) -> u32 {
    with_pipeline(handle, |state| {
        state.pipeline.set_audio_enabled(enabled != 0);
        1
    })
    .unwrap_or(0)
}

/// Clear all streaming state (an IQ stream restart / discontinuity).
#[no_mangle]
pub extern "C" fn websa_dsp_pipeline_reset(handle: u32) -> u32 {
    with_pipeline(handle, |state| {
        state.pipeline.reset();
        1
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn websa_dsp_pipeline_free(handle: u32) -> u32 {
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

    const FS_IN: f64 = 1_000_000.0;
    const OUT_RATE: f64 = 96_000.0;
    const DECIMATE: u32 = 10;

    fn am_block(n: usize, tone_hz: f64) -> Vec<i16> {
        // Two modulation tones, not one: the default chain includes the adaptive notch, which
        // removes the strongest single tone in the channel (correct for interference, and the
        // reason a pure-tone test signal comes out quiet). With two tones the second one survives,
        // so the level assertion measures the pipeline rather than the notch.
        let mut iq = Vec::with_capacity(n * 2);
        for k in 0..n {
            let t = k as f64 / OUT_RATE;
            let envelope = 1.0
                + 0.4 * (2.0 * PI * tone_hz * t).cos()
                + 0.4 * (2.0 * PI * tone_hz * 1.7 * t).cos();
            iq.push((envelope * 0.4 * 32768.0) as i16);
            iq.push(0);
        }
        iq
    }

    #[test]
    fn the_pipeline_abi_produces_audio_from_an_iq_block() {
        let mode = "am";
        let handle = unsafe {
            websa_dsp_pipeline_new(FS_IN, 0.0, DECIMATE, OUT_RATE, mode.as_ptr(), mode.len() as u32,
                                   12_000.0, 700.0)
        };
        assert_ne!(handle, 0, "the pipeline must be created");
        let iq = am_block(20_000, 1_000.0);
        let capacity = 8_192_u32;
        let in_ptr = crate::abi::websa_dsp_alloc(iq.len() * 2) as *mut i16;
        let out_ptr = crate::abi::websa_dsp_alloc(capacity as usize * 4) as *mut f32;
        assert!(!in_ptr.is_null() && !out_ptr.is_null());
        // SAFETY: both blocks were allocated with the sizes used here.
        unsafe {
            core::ptr::copy_nonoverlapping(iq.as_ptr(), in_ptr, iq.len());
            let written = websa_dsp_pipeline_process(handle, in_ptr, (iq.len() / 2) as u32,
                                                     out_ptr, capacity, 0);
            assert!(written > 0, "the pipeline must produce PCM");
            let pcm = core::slice::from_raw_parts(out_ptr, written as usize);
            let rms = (pcm.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt();
            // The ABI-level claim is "an IQ block in, PCM out": the exact level belongs to the
            // chain's policy (the notch removes the strongest tone, the squelch gates the rest) and
            // is verified per mode on the bench by task 12. Asserting a specific level here would
            // encode an audio decision in a transport test.
            assert!(rms > 0.005, "the pipeline must produce audio, rms {rms}");
            assert_eq!(websa_dsp_pipeline_reset(handle), 1);
            assert_eq!(websa_dsp_pipeline_retune(handle, 5_000.0), 1);
            assert_eq!(websa_dsp_pipeline_free(handle), 1);
            assert_eq!(websa_dsp_pipeline_process(handle, in_ptr, 16, out_ptr, capacity, 0), 0,
                       "a freed handle must not process");
            crate::abi::websa_dsp_free(in_ptr as *mut u8, iq.len() * 2);
            crate::abi::websa_dsp_free(out_ptr as *mut u8, capacity as usize * 4);
        }
    }

    #[test]
    fn an_unknown_mode_is_refused_and_the_audio_switch_is_reported() {
        let mode = "nope";
        let handle = unsafe {
            websa_dsp_pipeline_new(FS_IN, 0.0, DECIMATE, OUT_RATE, mode.as_ptr(), mode.len() as u32,
                                   12_000.0, 700.0)
        };
        assert_eq!(handle, 0, "a mode without a kernel must not create a pipeline");

        let mode = "usb";
        let handle = unsafe {
            websa_dsp_pipeline_new(FS_IN, 0.0, DECIMATE, OUT_RATE, mode.as_ptr(), mode.len() as u32,
                                   2_400.0, 700.0)
        };
        assert_ne!(handle, 0);
        assert_eq!(websa_dsp_pipeline_set_audio(handle, 0), 1);
        // The chain is off; the pipeline still produces PCM (the demodulator is not the chain).
        let iq = am_block(4_096, 1_000.0);
        let in_ptr = crate::abi::websa_dsp_alloc(iq.len() * 2) as *mut i16;
        let out_ptr = crate::abi::websa_dsp_alloc(4_096 * 4) as *mut f32;
        // SAFETY: both blocks are allocated above.
        unsafe {
            core::ptr::copy_nonoverlapping(iq.as_ptr(), in_ptr, iq.len());
            let written = websa_dsp_pipeline_process(handle, in_ptr, (iq.len() / 2) as u32,
                                                     out_ptr, 4_096, 0);
            assert!(written > 0);
            websa_dsp_pipeline_free(handle);
            crate::abi::websa_dsp_free(in_ptr as *mut u8, iq.len() * 2);
            crate::abi::websa_dsp_free(out_ptr as *mut u8, 4_096 * 4);
        }
    }

    #[test]
    fn the_analog_demod_used_by_the_pipeline_is_the_registered_one() {
        let mode = "nfm";
        let demod = analog::build(mode, DemodConfig::new(OUT_RATE, 12_000.0)).expect("nfm builds");
        assert_eq!(demod.id(), mode);
    }
}
