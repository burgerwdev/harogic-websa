//! FFI bindings for the prebuilt Ittiam libxaac (xHE-AAC / MPEG-D USAC) decoder.
//!
//! libxaac's decoder is a command-based API (`ixheaacd_dec_api(obj, cmd, idx, value)`).
//! This module wraps the minimal decode flow (see `dsp/core/xaac/FFI_NOTES.md`): open the
//! API object, configure for mp4 input, allocate the memory tables, feed the ASC then
//! each access unit, and read PCM from the output memory table.

use std::alloc::Layout;
#[cfg(not(test))]
use std::alloc::{alloc, dealloc};
#[cfg(test)]
use tests::{alloc, dealloc, ixheaacd_dec_api};

#[cfg(not(test))]
extern "C" {
    fn ixheaacd_dec_api(obj: *mut core::ffi::c_void, cmd: i32, idx: i32, value: *mut core::ffi::c_void) -> i32;
}

#[cfg(test)]
#[path = "xaac_tests.rs"]
mod tests;

const CMD_GET_API_SIZE: i32 = 0x0002;
const CMD_INIT: i32 = 0x0003;
const CMD_SET_CONFIG_PARAM: i32 = 0x0004;
const CMD_GET_MEMTABS_SIZE: i32 = 0x0006;
const CMD_SET_MEMTABS_PTR: i32 = 0x0007;
const CMD_EXECUTE: i32 = 0x0009;
const CMD_SET_INPUT_BYTES: i32 = 0x000C;
const CMD_GET_OUTPUT_BYTES: i32 = 0x000D;
const CMD_INPUT_OVER: i32 = 0x000E;
const CMD_GET_MEM_INFO_SIZE: i32 = 0x0011;
const CMD_GET_MEM_INFO_ALIGNMENT: i32 = 0x0012;
const CMD_GET_MEM_INFO_TYPE: i32 = 0x0013;
const CMD_SET_MEM_PTR: i32 = 0x0016;

const INIT_PRE_CONFIG: i32 = 0x0100;
const INIT_POST_CONFIG: i32 = 0x0200;
const INIT_PROCESS: i32 = 0x0300;
const INIT_DONE_QUERY: i32 = 0x0400;
const DO_EXECUTE: i32 = 0x0100;

const CFG_PCM_WDSZ: i32 = 0x0000;
const CFG_MP4FLAG: i32 = 0x000C;

const MEMTYPE_INPUT: u32 = 0x02;
const MEMTYPE_OUTPUT: u32 = 0x03;

/// Own the exact allocation geometry, including during partial SDK initialization.
struct OwnedBuffer {
    ptr: *mut u8,
    layout: Layout,
}

impl OwnedBuffer {
    unsafe fn new(size: usize, align: usize) -> Option<Self> {
        if size == 0 || !align.is_power_of_two() {
            return None;
        }
        let layout = Layout::from_size_align(size, align.max(8)).ok()?;
        let ptr = alloc(layout);
        if ptr.is_null() { None } else { Some(Self { ptr, layout }) }
    }
}

impl Drop for OwnedBuffer {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout); }
    }
}

unsafe fn query_u32(obj: *mut core::ffi::c_void, command: i32, index: i32) -> Option<u32> {
    let mut value = 0;
    (ixheaacd_dec_api(obj, command, index, &mut value as *mut u32 as *mut _) == 0)
        .then_some(value)
}

/// A minimal xHE-AAC decoder over the prebuilt libxaac archive.
pub struct XaacDecoder {
    obj: OwnedBuffer,
    _memtabs: OwnedBuffer,
    mem: Vec<OwnedBuffer>,
    input: usize,
    output: usize,
    /// The last libxaac error code (0 = none), for the readout.
    pub last_error: i32,
}

// SAFETY: see `AacDecoder`: the instance is exclusively owned and libxaac keeps no
// thread-affine global state; the wasm module is single-threaded.
unsafe impl Send for XaacDecoder {}

impl XaacDecoder {
    pub fn new() -> Option<Self> {
        unsafe {
            let size = query_u32(core::ptr::null_mut(), CMD_GET_API_SIZE, 0)?;
            let obj = OwnedBuffer::new(size as usize, 8)?;
            let object = obj.ptr.cast();
            if ixheaacd_dec_api(object, CMD_INIT, INIT_PRE_CONFIG, core::ptr::null_mut()) != 0 {
                return None;
            }
            let mut mp4: u32 = 1;
            let mut wdsz: u32 = 16;
            if ixheaacd_dec_api(object, CMD_SET_CONFIG_PARAM, CFG_MP4FLAG, &mut mp4 as *mut u32 as *mut _) != 0
                || ixheaacd_dec_api(object, CMD_SET_CONFIG_PARAM, CFG_PCM_WDSZ, &mut wdsz as *mut u32 as *mut _) != 0 {
                return None;
            }

            // The memory-table descriptor must be set before POST_CONFIG: POST_CONFIG
            // fills the descriptor via ixheaacd_fill_aac_mem_tables.
            let mt_size = query_u32(object, CMD_GET_MEMTABS_SIZE, 0)?;
            let memtabs = OwnedBuffer::new(mt_size as usize, 8)?;
            if ixheaacd_dec_api(object, CMD_SET_MEMTABS_PTR, 0, memtabs.ptr.cast()) != 0 {
                return None;
            }

            if ixheaacd_dec_api(object, CMD_INIT, INIT_POST_CONFIG, core::ptr::null_mut()) != 0 {
                return None;
            }

            let mut mem = Vec::new();
            let mut input = None;
            let mut output = None;
            for i in 0..4 {
                let msize = query_u32(object, CMD_GET_MEM_INFO_SIZE, i)?;
                let malign = query_u32(object, CMD_GET_MEM_INFO_ALIGNMENT, i)?;
                let mtype = query_u32(object, CMD_GET_MEM_INFO_TYPE, i)?;
                let buffer = OwnedBuffer::new(msize as usize, malign as usize)?;
                if ixheaacd_dec_api(object, CMD_SET_MEM_PTR, i, buffer.ptr.cast()) != 0 {
                    return None;
                }
                if mtype == MEMTYPE_INPUT {
                    if input.replace(mem.len()).is_some() { return None; }
                } else if mtype == MEMTYPE_OUTPUT {
                    if output.replace(mem.len()).is_some() { return None; }
                }
                mem.push(buffer);
            }
            Some(Self { obj, _memtabs: memtabs, mem, input: input?, output: output?, last_error: 0 })
        }
    }

    /// Feed one access unit (or the ASC) and decode; returns the PCM written to the
    /// output buffer (16-bit interleaved samples) or `None` on error.
    pub fn feed(&mut self, data: &[u8], init: bool) -> Option<Vec<i16>> {
        unsafe {
            self.last_error = 0;
            let input = &self.mem[self.input];
            if data.len() > input.layout.size() || data.len() > u32::MAX as usize {
                self.last_error = -1;
                return None;
            }
            core::ptr::copy_nonoverlapping(data.as_ptr(), input.ptr, data.len());
            let mut n = data.len() as u32;
            let object = self.obj.ptr.cast();
            let r = ixheaacd_dec_api(object, CMD_SET_INPUT_BYTES, 0, &mut n as *mut u32 as *mut _);
            if r != 0 {
                self.last_error = r;
                return None;
            }
            if init {
                let r = ixheaacd_dec_api(object, CMD_INIT, INIT_PROCESS, core::ptr::null_mut());
                if r != 0 {
                    self.last_error = r;
                    return None;
                }
                let mut done: u32 = 0;
                let r = ixheaacd_dec_api(object, CMD_INIT, INIT_DONE_QUERY, &mut done as *mut u32 as *mut _);
                if r != 0 || done == 0 {
                    self.last_error = if r != 0 { r } else { -1 };
                    return None;
                }
                return Some(Vec::new());
            }
            let r = ixheaacd_dec_api(object, CMD_EXECUTE, DO_EXECUTE, core::ptr::null_mut());
            if r != 0 {
                self.last_error = r;
                return None;
            }
            let mut out_bytes: u32 = 0;
            let r = ixheaacd_dec_api(object, CMD_GET_OUTPUT_BYTES, 0, &mut out_bytes as *mut u32 as *mut _);
            if r != 0 {
                self.last_error = r;
                return None;
            }
            let output = &self.mem[self.output];
            if out_bytes as usize > output.layout.size() || out_bytes % 2 != 0 {
                self.last_error = -1;
                return None;
            }
            let n_samples = out_bytes as usize / 2;
            let mut pcm = vec![0i16; n_samples];
            core::ptr::copy_nonoverlapping(output.ptr as *const i16, pcm.as_mut_ptr(), n_samples);
            Some(pcm)
        }
    }

    pub fn finish(&mut self) {
        unsafe {
            self.last_error = ixheaacd_dec_api(self.obj.ptr.cast(), CMD_INPUT_OVER, 0, core::ptr::null_mut());
        }
    }
}

/// Smoke-test export: decode a raw USAC bitstream (the committed test vector's
/// frame sizes: 22-byte ASC then 45 access units of 2333/768 bytes). Returns the
/// total decoded samples, or a negative code on failure.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn websa_dsp_xaac_decode(ptr: *mut u8, len: u32) -> i32 {
    const ASC: usize = 22;
    const FRAMES: &[usize] = &[
        2333, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768,
        768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768, 768,
        768, 768, 768, 768, 768, 768, 768, 768, 768, 768,
    ];
    let total: usize = ASC + FRAMES.iter().sum::<usize>();
    if (len as usize) < total {
        return -1;
    }
    let Some(mut dec) = XaacDecoder::new() else { return -2 };
    let bytes = unsafe { core::slice::from_raw_parts(ptr, total) };
    if dec.feed(&bytes[..ASC], true).is_none() {
        return -3;
    }
    let mut off = ASC;
    let mut energy = 0i64;
    let mut samples = 0usize;
    for &fs in FRAMES {
        if let Some(pcm) = dec.feed(&bytes[off..off + fs], false) {
            for &s in &pcm {
                energy += i64::from(s.abs());
            }
            samples += pcm.len();
        }
        off += fs;
    }
    dec.finish();
    if energy == 0 {
        -4
    } else {
        samples as i32
    }
}
