//! The SSTV mode table over the ABI.
//!
//! The decode window's mode buttons are built from this list, so the buttons cannot offer a mode the
//! decoder does not know — the same rule the plugin manifest follows for the demodulator list. The
//! table lives in `digital::sstv::SSTV_MODES`; this module only copies it out.
//!
//! The two reads a caller makes per mode: the display name (a string, copied into the caller's
//! buffer) and its metadata (`vis`, width, height — three `u32` words).

use crate::digital::sstv::{mode_vis, SSTV_MODES};

/// Number of modes this build can decode.
#[no_mangle]
pub extern "C" fn websa_dsp_sstv_mode_count() -> u32 {
    SSTV_MODES.len() as u32
}

/// Length of a mode's display name in bytes (0 for an unknown index).
#[no_mangle]
pub extern "C" fn websa_dsp_sstv_mode_name_len(index: u32) -> u32 {
    SSTV_MODES.get(index as usize).map_or(0, |(name, _)| name.len() as u32)
}

/// Copy a mode's display name into `buf` (utf-8, not null-terminated). Returns the bytes written, or
/// 0 when the index is unknown or the buffer is too small.
///
/// # Safety
/// `buf` must point to `capacity` writable bytes inside this module's linear memory.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_sstv_mode_name(index: u32, buf: *mut u8, capacity: u32) -> u32 {
    if buf.is_null() {
        return 0;
    }
    match SSTV_MODES.get(index as usize) {
        Some((name, _)) if name.len() <= capacity as usize => {
            core::ptr::copy_nonoverlapping(name.as_ptr(), buf, name.len());
            name.len() as u32
        }
        _ => 0,
    }
}

/// Write a mode's metadata into `meta` as three `u32` words — `[vis, width, height]` — and return
/// the word count (0 for an unknown index or a null pointer).
///
/// # Safety
/// `meta` must point to 3 writable `u32` inside this module's linear memory.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_sstv_mode_meta(index: u32, meta: *mut u32) -> u32 {
    if meta.is_null() {
        return 0;
    }
    let Some((_, mode)) = SSTV_MODES.get(index as usize) else {
        return 0;
    };
    let (width, height) = mode.resolution();
    let words = [u32::from(mode_vis(*mode)), width, height];
    core::ptr::copy_nonoverlapping(words.as_ptr(), meta, words.len());
    words.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_is_readable_through_the_abi() {
        let count = websa_dsp_sstv_mode_count() as usize;
        assert_eq!(count, 18, "the vendored table lists 18 modes");
        for index in 0..count as u32 {
            let len = websa_dsp_sstv_mode_name_len(index) as usize;
            assert!(len > 0);
            let mut buf = alloc::vec![0_u8; len];
            // SAFETY: the buffer is exactly the length the module just reported.
            let written = unsafe { websa_dsp_sstv_mode_name(index, buf.as_mut_ptr(), len as u32) };
            assert_eq!(written as usize, len);
            assert_eq!(core::str::from_utf8(&buf).unwrap(), SSTV_MODES[index as usize].0);

            let mut meta = [0_u32; 3];
            // SAFETY: the buffer is 3 words, as declared.
            let words = unsafe { websa_dsp_sstv_mode_meta(index, meta.as_mut_ptr()) };
            assert_eq!(words, 3);
            let (width, height) = SSTV_MODES[index as usize].1.resolution();
            assert_eq!(meta, [u32::from(mode_vis(SSTV_MODES[index as usize].1)), width, height]);
        }
    }

    #[test]
    fn unknown_indices_and_short_buffers_read_as_absent() {
        assert_eq!(websa_dsp_sstv_mode_name_len(18), 0);
        let mut buf = [0_u8; 1];
        // SAFETY: the buffer is 1 byte, as declared.
        assert_eq!(unsafe { websa_dsp_sstv_mode_name(0, buf.as_mut_ptr(), 1) }, 0,
                   "a too-small buffer must not produce a truncated name");
        let mut meta = [0_u32; 3];
        // SAFETY: the buffer is 3 words, as declared.
        assert_eq!(unsafe { websa_dsp_sstv_mode_meta(18, meta.as_mut_ptr()) }, 0);
    }
}
