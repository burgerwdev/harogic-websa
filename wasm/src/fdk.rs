//! Minimal C-runtime shims + FFI bindings for the prebuilt FDK AAC decoder
//! (wasm32 only). The C++ objects in `wasm/fdk/libfdk.a` expect `malloc`/`free`/
//! `calloc`/`realloc` (provided here, backed by Rust's global allocator) and the
//! `operator new/delete` glue (compiled into the archive). `memcpy`/`memset`/… come
//! from Rust's compiler-builtins.

use std::alloc::{alloc, dealloc, Layout};

const HEADER: usize = 16;

#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    let total = size.saturating_add(HEADER).max(1);
    let layout = Layout::from_size_align_unchecked(total, HEADER);
    let p = alloc(layout);
    if p.is_null() {
        return p;
    }
    *(p as *mut usize) = total;
    p.add(HEADER)
}

#[no_mangle]
pub unsafe extern "C" fn free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let p = ptr.sub(HEADER);
    let total = *(p as *mut usize);
    dealloc(p, Layout::from_size_align_unchecked(total, HEADER));
}

#[no_mangle]
pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut u8 {
    let total = n.saturating_mul(size);
    let p = malloc(total);
    if !p.is_null() {
        std::ptr::write_bytes(p, 0, total);
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    if ptr.is_null() {
        return malloc(size);
    }
    let old_total = *(ptr.sub(HEADER) as *mut usize);
    let p = malloc(size);
    if p.is_null() {
        return p;
    }
    let copy = old_total.saturating_sub(HEADER).min(size);
    std::ptr::copy_nonoverlapping(ptr, p, copy);
    free(ptr);
    p
}

// ---------------------------------------------------------------------------
// FDK AAC decoder FFI (C linkage; see libAACdec/include/aacdecoder_lib.h).
// ---------------------------------------------------------------------------

/// TT_DRM transport type.
const TT_DRM: i32 = 12;

extern "C" {
    fn aacDecoder_Open(transport: i32, layers: u32) -> *mut std::ffi::c_void;
    fn aacDecoder_Fill(
        h: *mut std::ffi::c_void,
        buf: *mut *mut u8,
        size: *const u32,
        valid: *mut u32,
    ) -> i32;
    fn aacDecoder_DecodeFrame(h: *mut std::ffi::c_void, pcm: *mut i16, size: i32, flags: u32) -> i32;
    fn aacDecoder_Close(h: *mut std::ffi::c_void);
    fn aacDecoder_GetStreamInfo(h: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

/// A streaming AAC (DRM transport) decoder over the prebuilt FDK AAC library.
pub struct AacDecoder {
    handle: *mut std::ffi::c_void,
}

impl AacDecoder {
    /// Open a decoder for the DRM transport (raw AAC access units with the ASC
    /// signalled in-band).
    pub fn new() -> Option<Self> {
        let handle = unsafe { aacDecoder_Open(TT_DRM, 1) };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    /// Decode one AAC access unit into 16-bit interleaved PCM (channels interleaved).
    pub fn decode(&mut self, au: &[u8]) -> Vec<i16> {
        // Feed the access unit.
        let mut buf = au.as_ptr() as *mut u8;
        let size = au.len() as u32;
        let mut valid = au.len() as u32;
        unsafe {
            aacDecoder_Fill(self.handle, &mut buf, &size, &mut valid);
        }

        // Decode one frame. 4096 samples per channel is more than any AAC frame.
        let mut pcm = vec![0i16; 4096 * 2];
        let err = unsafe { aacDecoder_DecodeFrame(self.handle, pcm.as_mut_ptr(), pcm.len() as i32, 0) };
        if err != 0 {
            return Vec::new();
        }

        let info = unsafe { aacDecoder_GetStreamInfo(self.handle) };
        if info.is_null() {
            return Vec::new();
        }
        let frame_size = unsafe { *(info as *const i32).add(1) } as usize; // frameSize
        let channels = unsafe { *(info as *const i32).add(2) } as usize; // numChannels
        pcm.truncate(frame_size * channels);
        pcm
    }
}

impl Drop for AacDecoder {
    fn drop(&mut self) {
        unsafe { aacDecoder_Close(self.handle) };
    }
}
