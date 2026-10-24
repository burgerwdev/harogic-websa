//! FDK AAC decoder FFI for native and wasm32 receivers.
//!
//! Native builds use the hosted FDK library. The committed wasm archive needs
//! allocator shims backed by Rust; these must never override the native C runtime.

#[cfg(target_arch = "wasm32")]
use std::alloc::{alloc, dealloc, Layout};

#[cfg(target_arch = "wasm32")]
const HEADER: usize = 16;

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    // Round the header-inclusive size up to HEADER so the Rust dlmalloc chunk size
    // and the size stored for `free`/`realloc` agree exactly (a non-aligned size
    // makes dlmalloc round up internally and its `psize <= size + max_overhead`
    // sanity check fails on the next allocation).
    let total = size.saturating_add(HEADER).max(1);
    let total = (total + HEADER - 1) & !(HEADER - 1);
    let layout = Layout::from_size_align_unchecked(total, HEADER);
    let p = alloc(layout);
    if p.is_null() {
        return p;
    }
    *(p as *mut usize) = total;
    p.add(HEADER)
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let p = ptr.sub(HEADER);
    let total = *(p as *mut usize);
    dealloc(p, Layout::from_size_align_unchecked(total, HEADER));
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut u8 {
    let total = n.saturating_mul(size);
    let p = malloc(total);
    if !p.is_null() {
        std::ptr::write_bytes(p, 0, total);
    }
    p
}

#[cfg(target_arch = "wasm32")]
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

/// Prefix of FDK's CStreamInfo through outputDelay. Pointer fields preserve the
/// native ABI alignment as well as the wasm32 layout.
#[repr(C)]
struct StreamInfo {
    sample_rate: i32,
    frame_size: i32,
    channels: i32,
    channel_types: *const i32,
    channel_indices: *const u8,
    aac_sample_rate: i32,
    profile: i32,
    aot: i32,
    channel_config: i32,
    bit_rate: i32,
    aac_samples_per_frame: i32,
    aac_channels: i32,
    extension_aot: i32,
    extension_sample_rate: i32,
    output_delay: u32,
}

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

/// A streaming AAC / HE-AAC / xHE-AAC (DRM transport) decoder over FDK AAC.
pub struct AacDecoder {
    handle: *mut std::ffi::c_void,
    /// Error code of the last decode call, for diagnostics.
    pub last_error: i32,
    /// `(sampleRate, frameSize, numChannels)` of the last decoded frame, for diagnostics.
    pub last_stream_info: (u32, u32, u32),
    /// Additional decoder delay in PCM frames at last_stream_info.0, reported by FDK.
    pub last_output_delay: Option<u32>,
}

// SAFETY: the handle is exclusively owned and moved between owners, never shared.
// FDK keeps no thread-affine global state (the same argument DecDRM's FdkDrmDecoder
// makes for its own `unsafe impl`).
unsafe impl Send for AacDecoder {}

impl AacDecoder {
    /// Open a decoder for the DRM transport (raw AAC access units with the ASC
    /// signalled in-band).
    pub fn new() -> Option<Self> {
        let handle = unsafe { aacDecoder_Open(TT_DRM, 1) };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle, last_error: 0, last_stream_info: (0, 0, 0), last_output_delay: None })
        }
    }

    /// Configure the decoder from the SDC type-9 audio information bytes (2 bytes for
    /// AAC; the xHE-AAC config is appended by the caller for xHE-AAC).
    pub fn configure(&mut self, type9: &[u8]) -> bool {
        let mut conf = type9.as_ptr() as *mut u8;
        let len = type9.len() as u32;
        self.last_error = unsafe { aacDecoder_ConfigRaw(self.handle, &mut conf, &len) };
        self.last_error == 0
    }

    /// Decode one AAC access unit into 16-bit interleaved PCM (channels interleaved).
    pub fn decode(&mut self, au: &[u8]) -> Vec<i16> {
        // FDK copies the access unit into its bit buffer. Its C API takes a mutable
        // pointer, but does not modify this input. Pass its exact length: DRM SBR is
        // located relative to the end of the unit.
        let mut buf = au.as_ptr() as *mut u8;
        let size = au.len() as u32;
        let mut valid = au.len() as u32;
        let err = unsafe { aacDecoder_Fill(self.handle, &mut buf, &size, &mut valid) };
        if err != 0 || valid != 0 {
            self.last_error = if err != 0 { err } else { 0x4005 };
            return Vec::new();
        }

        // Decode one frame. 4096 samples per channel is more than any AAC frame.
        let mut pcm = vec![0i16; 4096 * 2];
        let err = unsafe { aacDecoder_DecodeFrame(self.handle, pcm.as_mut_ptr(), pcm.len() as i32, 0) };
        self.last_error = err;
        // FDK's IS_OUTPUT_VALID contract includes concealed decode errors.
        // Their PCM is part of the output clock and must reach the consumer.
        if err != 0 && !(0x4000..=0x4fff).contains(&err) {
            return Vec::new();
        }

        let info = unsafe { aacDecoder_GetStreamInfo(self.handle) };
        if info.is_null() {
            return Vec::new();
        }
        let info = unsafe { &*(info as *const StreamInfo) };
        let sample_rate = info.sample_rate;
        let frame_size = info.frame_size;
        let channels = info.channels;
        if sample_rate <= 0 || frame_size <= 0 || channels <= 0
            || frame_size as usize > pcm.len() / channels as usize
        {
            self.last_error = -1;
            return Vec::new();
        }
        self.last_stream_info = (sample_rate as u32, frame_size as u32, channels as u32);
        self.last_output_delay = Some(info.output_delay);
        pcm.truncate(frame_size as usize * channels as usize);
        pcm
    }
}

impl Drop for AacDecoder {
    fn drop(&mut self) {
        unsafe { aacDecoder_Close(self.handle) };
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn drm_aac_fixture_and_malformed_access_unit() {
        let mut decoder = AacDecoder::new().unwrap();
        assert!(decoder.configure(&[0x03, 0x00]));
        let bytes = include_bytes!("../../tests/fixtures/drm/aac_sine_24k.drm");
        let mut pcm = Vec::new();
        for frame in bytes.chunks_exact(36) { pcm.extend(decoder.decode(frame)); }
        assert_eq!(decoder.last_stream_info, (24_000, 960, 1));
        let peak = pcm.iter().map(|&sample| i32::from(sample).abs()).max().unwrap_or(0);
        assert!(peak > 1000, "AAC fixture must contain its tone, peak={peak}");
        let energy = |frequency: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (index, &sample) in pcm.iter().enumerate() {
                let phase = 2.0 * std::f64::consts::PI * frequency * index as f64 / 24_000.0;
                re += f64::from(sample) * phase.cos();
                im += f64::from(sample) * phase.sin();
            }
            re * re + im * im
        };
        assert!(energy(1000.0) > 5.0 * energy(500.0).max(energy(2000.0)), "AAC tone pitch must remain 1 kHz");
        let mut malformed = AacDecoder::new().unwrap();
        assert!(malformed.configure(&[0x03, 0x00]));
        malformed.decode(&[]);
        assert_ne!(malformed.last_error, 0, "empty AU must report a codec error");
        // A damaged DRM CRC still yields FDK's concealed PCM. Preserve its
        // duration and allow the next intact units to resume the station.
        let mut corrupt = bytes[..36].to_vec();
        corrupt[0] ^= 0xff;
        let concealed = decoder.decode(&corrupt);
        assert!((0x4000..=0x4fff).contains(&decoder.last_error), "CRC must be reported");
        assert_eq!(concealed.len(), 960, "concealed PCM retains one audio frame");
        let recovered: Vec<_> = bytes.chunks_exact(36).flat_map(|frame| decoder.decode(frame)).collect();
        let peak = recovered.iter().map(|&sample| i32::from(sample).abs()).max().unwrap_or(0);
        assert!(peak > 1000, "intact AAC units after corruption must resume the tone, peak={peak}");
    }

    #[test]
    fn usac_fixture_and_truncated_access_unit() {
        let bytes = include_bytes!("../../tests/fixtures/drm/usac_tone.bin");
        // The fixture is an MPEG-4 ASC followed by one 2333-byte AU and 44 768-byte AUs.
        let handle = unsafe { aacDecoder_Open(0, 1) }; // TT_MP4_RAW
        assert!(!handle.is_null());
        let mut decoder = AacDecoder { handle, last_error: 0, last_stream_info: (0, 0, 0), last_output_delay: None };
        assert!(decoder.configure(&bytes[..22]));
        let mut pcm = decoder.decode(&bytes[22..22 + 2333]);
        for frame in bytes[22 + 2333..].chunks_exact(768) { pcm.extend(decoder.decode(frame)); }
        let peak = pcm.iter().map(|&sample| i32::from(sample).abs()).max().unwrap_or(0);
        assert!(peak > 1000, "USAC fixture must contain its tone, peak={peak}");
        assert!(decoder.last_stream_info.0 > 0 && decoder.last_stream_info.2 > 0);
        decoder.decode(&bytes[22..23]);
        assert_ne!(decoder.last_error, 0);
        let mut recovered = decoder.decode(&bytes[22..22 + 2333]);
        for frame in bytes[22 + 2333..].chunks_exact(768) { recovered.extend(decoder.decode(frame)); }
        let peak = recovered.iter().map(|&sample| i32::from(sample).abs()).max().unwrap_or(0);
        assert!(peak > 1000, "intact USAC units after truncation must resume the tone, peak={peak}");
    }
}

/// Diagnostic: decode the AU dump format of `examples/dump_drm_au.rs` inside this
/// module ([type9_len u32][type9][crc u8][len u32][data]...). Returns the number of
/// access units decoded, or a negative error. Temporary, for localising the trap.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_fdk_decode_dump(ptr: *mut u8, len: u32) -> i32 {
    let data = std::slice::from_raw_parts(ptr, len as usize);
    let mut pos = 0usize;
    let tl = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;
    let mut dec = match AacDecoder::new() {
        Some(d) => d,
        None => return -1,
    };
    if !dec.configure(&data[pos..pos + tl]) {
        return -2;
    }
    pos += tl;
    let mut frames = 0i32;
    let mut err = 0i32;
    while pos + 5 <= len as usize {
        let crc = data[pos];
        let alen = u32::from_le_bytes(data[pos + 1..pos + 5].try_into().unwrap()) as usize;
        pos += 5;
        if pos + alen > len as usize {
            break;
        }
        let mut au = Vec::with_capacity(alen + 1);
        au.push(crc);
        au.extend_from_slice(&data[pos..pos + alen]);
        pos += alen;
        let pcm = dec.decode(&au);
        err = dec.last_error;
        if err != 0 {
            break;
        }
        if !pcm.is_empty() {
            frames += 1;
        }
    }
    if err != 0 {
        return -1000 - err;
    }
    frames
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
        // AAC 24 kHz, no SBR, mono: type-9 bytes 0x03 0x00 (the AAC-LC core at 24 kHz;
        // the SBR case 0x23 0x00 adds the SBR tail the fixture does not carry).
        let mut conf = [0x03u8, 0x00];
        let mut cp = conf.as_mut_ptr();
        let clen = 2u32;
        if aacDecoder_ConfigRaw(h, &mut cp, &clen) != 0 {
            aacDecoder_Close(h);
            return -2;
        }
        // One access unit per call: the DRM transport has no length prefix, so the
        // caller feeds a single frame. The decoder has a two-frame priming delay, so a
        // lone frame decodes silently; the fixture test drives several frames.
        let mut buf = ptr;
        let size = len;
        let mut valid = len;
        aacDecoder_Fill(h, &mut buf, &size, &mut valid);
        let mut frame_size = 0i32;
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
        }
        aacDecoder_Close(h);
        // frame_size proves the access unit decodes (a lone frame is the decoder's
        // priming frame and therefore silent); the non-silent assertion lives in the
        // DRM AAC fixture test (tools/fixtures/verify_drm_aac.c).
        if frame_size > 0 {
            frame_size
        } else {
            -3 // no frame decoded
        }
    }
}

/// Fixture-test export: decode a run of equal-sized DRM AAC access units (the committed
/// `aac_sine_24k` fixture) and return the frame size when the run decodes non-silently,
/// or a negative code. Feeds one access unit per call (the DRM transport has no length
/// prefix), which passes the decoder's two-frame priming delay.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_fdk_decode_drm_frames(ptr: *mut u8, len: u32, frame_len: u32) -> i32 {
    if ptr.is_null() || frame_len == 0 || len % frame_len != 0 {
        return -4;
    }
    let h = aacDecoder_Open(12, 1); // TT_DRM
    if h.is_null() {
        return -1;
    }
    let mut conf = [0x03u8, 0x00]; // AAC 24 kHz, no SBR, mono
    let mut cp = conf.as_mut_ptr();
    let clen = 2u32;
    if aacDecoder_ConfigRaw(h, &mut cp, &clen) != 0 {
        aacDecoder_Close(h);
        return -2;
    }
    let frames = (len / frame_len) as usize;
    let mut frame_size = 0i32;
    let mut energy = 0i64;
    for k in 0..frames {
        let mut buf = ptr.add(k * frame_len as usize);
        let size = frame_len;
        let mut valid = frame_len;
        aacDecoder_Fill(h, &mut buf, &size, &mut valid);
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
