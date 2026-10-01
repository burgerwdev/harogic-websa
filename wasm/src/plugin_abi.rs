//! The plugin manifest over the ABI.
//!
//! The browser does not carry its own list of modes: it reads this one from the module (see
//! `frontend/modern/src/sdr/registry.ts`). A mode therefore exists in exactly one place, and a UI
//! list that drifts from the DSP is not possible to express.
//!
//! Strings are copied into a caller-provided buffer: the id is ASCII and short, and returning a
//! pointer into a static would make the caller depend on Rust layout.

use crate::plugin::{plugins, PluginDescriptor, PluginKind};

fn descriptor(kind_raw: u32, index: u32) -> Option<&'static PluginDescriptor> {
    let kind = PluginKind::from_u32(kind_raw)?;
    plugins(kind).get(index as usize)
}

/// Number of plugin families (analog, digital, audio, ddc), for the loader's iteration.
#[no_mangle]
pub extern "C" fn websa_dsp_plugin_kind_count() -> u32 {
    PluginKind::all().len() as u32
}

/// Number of plugins in one family (0 for an unknown kind).
#[no_mangle]
pub extern "C" fn websa_dsp_plugin_count(kind: u32) -> u32 {
    PluginKind::from_u32(kind).map(|kind| plugins(kind).len() as u32).unwrap_or(0)
}

/// Length of a plugin id in bytes (0 for an unknown kind/index).
#[no_mangle]
pub extern "C" fn websa_dsp_plugin_id_len(kind: u32, index: u32) -> u32 {
    descriptor(kind, index).map(|plugin| plugin.id.len() as u32).unwrap_or(0)
}

/// Copy a plugin id into `buf` (utf-8, not null-terminated). Returns the bytes written, or 0 when
/// the kind/index is unknown or the buffer is too small.
///
/// # Safety
/// `buf` must point to `capacity` writable bytes inside this module's linear memory.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_plugin_id(kind: u32, index: u32, buf: *mut u8, capacity: u32) -> u32 {
    if buf.is_null() {
        return 0;
    }
    match descriptor(kind, index) {
        Some(plugin) if plugin.id.len() <= capacity as usize => {
            core::ptr::copy_nonoverlapping(plugin.id.as_ptr(), buf, plugin.id.len());
            plugin.id.len() as u32
        }
        _ => 0,
    }
}

/// 1 when the plugin's kernel exists, 0 when it is declared but not written yet.
#[no_mangle]
pub extern "C" fn websa_dsp_plugin_implemented(kind: u32, index: u32) -> u32 {
    descriptor(kind, index).map(|plugin| plugin.implemented as u32).unwrap_or(0)
}

/// 1 when the plugin belongs to the audio-enhancement chain (analog PCM path only).
#[no_mangle]
pub extern "C" fn websa_dsp_plugin_audio_enhancement(kind: u32, index: u32) -> u32 {
    descriptor(kind, index).map(|plugin| plugin.audio_enhancement as u32).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_readable_through_the_abi() {
        assert_eq!(websa_dsp_plugin_kind_count(), 4);
        for kind in PluginKind::all() {
            let count = websa_dsp_plugin_count(kind as u32);
            assert_eq!(count, plugins(kind).len() as u32);
            for index in 0..count {
                let len = websa_dsp_plugin_id_len(kind as u32, index) as usize;
                let expected = plugins(kind)[index as usize];
                assert_eq!(len, expected.id.len());
                let mut buf = vec![0_u8; len];
                // SAFETY: the buffer is exactly the length the module just reported.
                let written = unsafe { websa_dsp_plugin_id(kind as u32, index, buf.as_mut_ptr(), len as u32) };
                assert_eq!(written as usize, len);
                assert_eq!(std::str::from_utf8(&buf).unwrap(), expected.id);
                assert_eq!(websa_dsp_plugin_implemented(kind as u32, index), expected.implemented as u32);
                assert_eq!(websa_dsp_plugin_audio_enhancement(kind as u32, index),
                           expected.audio_enhancement as u32);
            }
        }
    }

    #[test]
    fn a_short_buffer_is_refused_instead_of_truncating() {
        let mut buf = [0_u8; 1];
        // SAFETY: the buffer is 1 byte, as declared.
        let written = unsafe { websa_dsp_plugin_id(PluginKind::Analog as u32, 0, buf.as_mut_ptr(), 1) };
        assert_eq!(written, 0, "a too-small buffer must not produce a truncated id");
    }

    #[test]
    fn unknown_kinds_and_indices_read_as_absent() {
        assert_eq!(websa_dsp_plugin_count(99), 0);
        assert_eq!(websa_dsp_plugin_id_len(99, 0), 0);
        assert_eq!(websa_dsp_plugin_id_len(PluginKind::Analog as u32, 999), 0);
        assert_eq!(websa_dsp_plugin_implemented(99, 0), 0);
        let mut buf = [0_u8; 16];
        // SAFETY: the buffer is 16 bytes and declared as such.
        assert_eq!(unsafe { websa_dsp_plugin_id(99, 0, buf.as_mut_ptr(), 16) }, 0);
    }
}
