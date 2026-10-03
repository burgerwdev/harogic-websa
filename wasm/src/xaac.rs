//! FFI bindings for the prebuilt Ittiam libxaac (xHE-AAC / MPEG-D USAC) decoder.
//!
//! libxaac's decoder is a command-based API (`ixheaacd_dec_api(obj, cmd, idx, value)`).
//! This module wraps the minimal decode flow (see `wasm/xaac/FFI_NOTES.md`): open the
//! API object, configure for mp4 input, allocate the memory tables, feed the ASC then
//! each access unit, and read PCM from the output memory table.

use std::alloc::{alloc, dealloc, Layout};

extern "C" {
    fn ixheaacd_dec_api(obj: *mut core::ffi::c_void, cmd: i32, idx: i32, value: *mut core::ffi::c_void) -> i32;
}

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

/// Allocate `size` bytes aligned to `align` (both powers-of-two-ish).
unsafe fn alloc_aligned(size: usize, align: usize) -> *mut u8 {
    let align = align.max(8).next_power_of_two();
    let layout = Layout::from_size_align_unchecked(size.max(1), align);
    alloc(layout)
}

/// A minimal xHE-AAC decoder over the prebuilt libxaac archive.
pub struct XaacDecoder {
    obj: *mut core::ffi::c_void,
    memtabs: *mut u8,
    mem: Vec<(*mut u8, usize)>,
    in_ptr: *mut u8,
    out_ptr: *mut u8,
}

impl XaacDecoder {
    pub fn new() -> Option<Self> {
        unsafe {
            let mut size: u32 = 0;
            if ixheaacd_dec_api(core::ptr::null_mut(), CMD_GET_API_SIZE, 0, &mut size as *mut u32 as *mut _) != 0 {
                return None;
            }
            let obj = alloc_aligned(size as usize, 8) as *mut core::ffi::c_void;
            if obj.is_null() {
                return None;
            }
            if ixheaacd_dec_api(obj, CMD_INIT, INIT_PRE_CONFIG, core::ptr::null_mut()) != 0 {
                return None;
            }
            let mut mp4: u32 = 1;
            let mut wdsz: u32 = 16;
            ixheaacd_dec_api(obj, CMD_SET_CONFIG_PARAM, CFG_MP4FLAG, &mut mp4 as *mut u32 as *mut _);
            ixheaacd_dec_api(obj, CMD_SET_CONFIG_PARAM, CFG_PCM_WDSZ, &mut wdsz as *mut u32 as *mut _);

            // The memory-table descriptor must be set before POST_CONFIG: POST_CONFIG
            // fills the descriptor via ixheaacd_fill_aac_mem_tables.
            let mut mt_size: u32 = 0;
            ixheaacd_dec_api(obj, CMD_GET_MEMTABS_SIZE, 0, &mut mt_size as *mut u32 as *mut _);
            let memtabs = alloc_aligned(mt_size as usize, 8);
            ixheaacd_dec_api(obj, CMD_SET_MEMTABS_PTR, 0, memtabs as *mut _);

            if ixheaacd_dec_api(obj, CMD_INIT, INIT_POST_CONFIG, core::ptr::null_mut()) != 0 {
                return None;
            }

            let mut mem = Vec::new();
            let mut in_ptr = core::ptr::null_mut();
            let mut out_ptr = core::ptr::null_mut();
            for i in 0..4 {
                let (mut msize, mut malign, mut mtype) = (0u32, 0u32, 0u32);
                ixheaacd_dec_api(obj, CMD_GET_MEM_INFO_SIZE, i, &mut msize as *mut u32 as *mut _);
                ixheaacd_dec_api(obj, CMD_GET_MEM_INFO_ALIGNMENT, i, &mut malign as *mut u32 as *mut _);
                ixheaacd_dec_api(obj, CMD_GET_MEM_INFO_TYPE, i, &mut mtype as *mut u32 as *mut _);
                let p = alloc_aligned(msize as usize, malign as usize);
                ixheaacd_dec_api(obj, CMD_SET_MEM_PTR, i, p as *mut _);
                mem.push((p, msize as usize));
                if mtype == MEMTYPE_INPUT {
                    in_ptr = p;
                } else if mtype == MEMTYPE_OUTPUT {
                    out_ptr = p;
                }
            }
            Some(Self { obj, memtabs, mem, in_ptr, out_ptr })
        }
    }

    /// Feed one access unit (or the ASC) and decode; returns the PCM written to the
    /// output buffer (16-bit interleaved samples) or `None` on error.
    pub fn feed(&mut self, data: &[u8], init: bool) -> Option<Vec<i16>> {
        unsafe {
            if self.in_ptr.is_null() {
                return None;
            }
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.in_ptr, data.len());
            let mut n = data.len() as u32;
            if ixheaacd_dec_api(self.obj, CMD_SET_INPUT_BYTES, 0, &mut n as *mut u32 as *mut _) != 0 {
                return None;
            }
            if init {
                if ixheaacd_dec_api(self.obj, CMD_INIT, INIT_PROCESS, core::ptr::null_mut()) != 0 {
                    return None;
                }
                let mut done: u32 = 0;
                ixheaacd_dec_api(self.obj, CMD_INIT, INIT_DONE_QUERY, &mut done as *mut u32 as *mut _);
                let _ = done;
                return Some(Vec::new());
            }
            if ixheaacd_dec_api(self.obj, CMD_EXECUTE, DO_EXECUTE, core::ptr::null_mut()) != 0 {
                return None;
            }
            let mut out_bytes: u32 = 0;
            ixheaacd_dec_api(self.obj, CMD_GET_OUTPUT_BYTES, 0, &mut out_bytes as *mut u32 as *mut _);
            let n_samples = out_bytes as usize / 2;
            let mut pcm = vec![0i16; n_samples];
            core::ptr::copy_nonoverlapping(self.out_ptr as *const i16, pcm.as_mut_ptr(), n_samples);
            Some(pcm)
        }
    }

    pub fn finish(&mut self) {
        unsafe {
            ixheaacd_dec_api(self.obj, CMD_INPUT_OVER, 0, core::ptr::null_mut());
        }
    }
}

impl Drop for XaacDecoder {
    fn drop(&mut self) {
        unsafe {
            for &(p, size) in &self.mem {
                if !p.is_null() {
                    dealloc(p, Layout::from_size_align_unchecked(size.max(1), 8));
                }
            }
            if !self.memtabs.is_null() {
                dealloc(self.memtabs, Layout::from_size_align_unchecked(128, 8));
            }
            if !self.obj.is_null() {
                dealloc(self.obj as *mut u8, Layout::from_size_align_unchecked(1, 8));
            }
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
