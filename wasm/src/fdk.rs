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
    fn aacDecoder_ConfigRaw(
        h: *mut std::ffi::c_void,
        conf: *mut *mut u8,
        len: *const u32,
    ) -> i32;
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

    /// Configure the decoder from the SDC type-9 audio information bytes (2 bytes for
    /// AAC; the xHE-AAC config is appended by the caller for xHE-AAC).
    pub fn configure(&mut self, type9: &[u8]) -> bool {
        let mut conf = type9.as_ptr() as *mut u8;
        let len = type9.len() as u32;
        unsafe { aacDecoder_ConfigRaw(self.handle, &mut conf, &len) == 0 }
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

/// Smoke-test export: decode an ADTS AAC buffer (ptr/len into linear memory) and
/// return the decoded frame size in samples, or a negative code on failure. Used by
/// the runtime harness to prove the linked codec actually decodes.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn websa_dsp_fdk_decode_adts(ptr: *mut u8, len: u32) -> i32 {
    unsafe {
        let h = aacDecoder_Open(2, 1); // TT_MP4_ADTS
        if h.is_null() {
            return -1;
        }
        let mut buf = ptr;
        let size = len;
        let mut valid = len;
        aacDecoder_Fill(h, &mut buf, &size, &mut valid);

        // The decoder has a short priming delay: keep decoding until the input is
        // exhausted and total any energy across frames.
        let mut frame_size = 0i32;
        let mut channels = 0i32;
        let mut energy = 0i64;
        for _ in 0..16 {
            let mut pcm = [0i16; 8192];
            let err = aacDecoder_DecodeFrame(h, pcm.as_mut_ptr(), pcm.len() as i32, 0);
            if err != 0 {
                break;
            }
            let info = aacDecoder_GetStreamInfo(h);
            if info.is_null() {
                break;
            }
            frame_size = *(info as *const i32).add(1);
            channels = *(info as *const i32).add(2);
            for &s in &pcm[..(frame_size * channels).min(pcm.len() as i32) as usize] {
                energy += i64::from(s.abs());
            }
        }
        aacDecoder_Close(h);
        if frame_size <= 0 {
            -6 // no frame decoded
        } else {
            ((frame_size << 16) | ((channels & 0xFF) << 8) | if energy > 0 { 0 } else { 1 }) as i32
        }
    }
}

/// Smoke-test export: decode raw DRM AAC access units (TT_DRM transport, configured
/// from SDC type-9 bytes) and return the decoded frame size, or a negative code.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn websa_dsp_fdk_decode_drm(ptr: *mut u8, len: u32) -> i32 {
    unsafe {
        let h = aacDecoder_Open(12, 1); // TT_DRM
        if h.is_null() {
            return -1;
        }
        // AAC 24 kHz, SBR, mono: type-9 bytes 0x23 0x00.
        let mut conf = [0x23u8, 0x00];
        let mut cp = conf.as_mut_ptr();
        let clen = 2u32;
        if aacDecoder_ConfigRaw(h, &mut cp, &clen) != 0 {
            aacDecoder_Close(h);
            return -2;
        }
        let mut buf = ptr;
        let size = len;
        let mut valid = len;
        aacDecoder_Fill(h, &mut buf, &size, &mut valid);
        let mut frame_size = 0i32;
        let mut energy = 0i64;
        for _ in 0..16 {
            let mut pcm = [0i16; 8192];
            if aacDecoder_DecodeFrame(h, pcm.as_mut_ptr(), 8192, 0) != 0 {
                break;
            }
            let info = aacDecoder_GetStreamInfo(h);
            if info.is_null() {
                break;
            }
            frame_size = *(info as *const i32).add(1);
            let ch = *(info as *const i32).add(2);
            for &s in &pcm[..(frame_size * ch).min(8192) as usize] {
                energy += i64::from(s.abs());
            }
        }
        aacDecoder_Close(h);
        if energy > 0 {
            frame_size
        } else {
            -3 // decoded but silent
        }
    }
}
