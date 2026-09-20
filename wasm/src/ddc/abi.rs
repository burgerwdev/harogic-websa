//! The DDC's exported ABI: handles plus block processing over linear memory.
//!
//! The loader allocates an input block and an output block once, writes the int16 IQ into the
//! input, calls `websa_dsp_ddc_process`, and reads the complex f32 result back. Nothing is
//! marshalled per sample and no object crosses the boundary.
//!
//! Handles are indices into a small registry (1-based, 0 is invalid). wasm32 is single-threaded,
//! so a thread-local registry is the whole concurrency story.

use core::cell::RefCell;

use super::{Ddc, DdcConfig};

thread_local! {
    static DDCS: RefCell<Vec<Option<Ddc>>> = const { RefCell::new(Vec::new()) };
}

fn with_ddc<R>(handle: u32, f: impl FnOnce(&mut Ddc) -> R) -> Option<R> {
    if handle == 0 {
        return None;
    }
    DDCS.with(|slot| {
        let mut registry = slot.borrow_mut();
        registry
            .get_mut(handle as usize - 1)
            .and_then(|entry| entry.as_mut())
            .map(f)
    })
}

/// Create a channelizer. Returns 0 when the configuration is invalid.
#[no_mangle]
pub extern "C" fn websa_dsp_ddc_new(
    fs_in: f64,
    offset_hz: f64,
    cutoff_hz: f64,
    decimate: u32,
    out_rate: f64,
    ntaps: u32,
) -> u32 {
    if !(fs_in > 0.0) || decimate == 0 {
        return 0;
    }
    let config = DdcConfig {
        fs_in,
        offset_hz,
        cutoff_hz,
        decimate: decimate as usize,
        out_rate,
        ntaps: ntaps as usize,
    };
    DDCS.with(|slot| {
        let mut registry = slot.borrow_mut();
        let ddc = Ddc::new(config);
        for (index, entry) in registry.iter_mut().enumerate() {
            if entry.is_none() {
                *entry = Some(ddc);
                return index as u32 + 1;
            }
        }
        registry.push(Some(ddc));
        registry.len() as u32
    })
}

/// Change the NCO frequency, keeping the filter tails clear and the AGC gain (a retune).
#[no_mangle]
pub extern "C" fn websa_dsp_ddc_retune(handle: u32, offset_hz: f64) -> u32 {
    with_ddc(handle, |ddc| {
        ddc.retune(offset_hz);
        1
    })
    .unwrap_or(0)
}

/// Drop all filter/oscillator history (used when the IQ stream restarts).
#[no_mangle]
pub extern "C" fn websa_dsp_ddc_reset(handle: u32) -> u32 {
    with_ddc(handle, |ddc| {
        ddc.reset();
        1
    })
    .unwrap_or(0)
}

/// Release a channelizer.
#[no_mangle]
pub extern "C" fn websa_dsp_ddc_free(handle: u32) -> u32 {
    if handle == 0 {
        return 0;
    }
    DDCS.with(|slot| {
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

/// Run the full chain over one int16 IQ block.
///
/// Returns the number of complex output samples written (0 on a bad handle, a null pointer or
/// too little capacity). I and Q are interleaved in both buffers.
///
/// # Safety
/// `iq_ptr` must point to `samples * 2` readable int16 values and `out_ptr` to
/// `out_capacity * 2` writable f32 values, both inside this module's linear memory.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_ddc_process(
    handle: u32,
    iq_ptr: *const i16,
    samples: u32,
    out_ptr: *mut f32,
    out_capacity: u32,
) -> u32 {
    if iq_ptr.is_null() || out_ptr.is_null() || samples == 0 {
        return 0;
    }
    let input = core::slice::from_raw_parts(iq_ptr, samples as usize * 2);
    let output = core::slice::from_raw_parts_mut(out_ptr, out_capacity as usize * 2);
    with_ddc(handle, |ddc| {
        let mut block = Vec::new();
        ddc.process_i16_into(input, &mut block);
        let written = block.len() / 2;
        if written > out_capacity as usize {
            return 0;                       // never write past the caller's block
        }
        output[..block.len()].copy_from_slice(&block);
        written as u32
    })
    .unwrap_or(0)
}

/// Apply the level stage to a real f32 block in place (`hold != 0` freezes the adaptation).
///
/// # Safety
/// `ptr` must point to `samples` writable f32 values inside this module's linear memory.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_ddc_agc(handle: u32, ptr: *mut f32, samples: u32, hold: u32) -> u32 {
    if ptr.is_null() || samples == 0 {
        return 0;
    }
    let block = core::slice::from_raw_parts_mut(ptr, samples as usize);
    with_ddc(handle, |ddc| {
        let input = block.to_vec();
        let mut out = Vec::new();
        ddc.agc_into(&input, hold != 0, &mut out);
        block[..out.len()].copy_from_slice(&out);
        out.len() as u32
    })
    .unwrap_or(0)
}

/// The current AGC gain (diagnostics: a wound-up gain explains a loud first block).
#[no_mangle]
pub extern "C" fn websa_dsp_ddc_agc_gain(handle: u32) -> f64 {
    with_ddc(handle, |ddc| ddc.agc().gain()).unwrap_or(1.0)
}
