//! The linear-memory ABI shared by Rust and the TypeScript loader.
//!
//! There is no wasm-bindgen here on purpose. The DSP works on sample blocks, so the boundary
//! is a pointer plus a length into this module's linear memory: the TypeScript side allocates
//! a block once per stream, writes the IQ samples into it, calls a kernel that processes in
//! place, and reads the result back through a typed view. Nothing is marshalled per sample and
//! no object crosses the boundary.
//!
//! Rules that both sides depend on:
//!   * `websa_dsp_alloc(bytes)` returns an 8-byte aligned pointer, or 0 on failure.
//!   * `websa_dsp_free(ptr, bytes)` must be called with the same `bytes` value that was
//!     allocated (the size is rounded up internally, deterministically, so any value in the
//!     same 8-byte bucket frees the same layout).
//!   * The pointer stays valid until freed; the module never moves an allocation.

use core::alloc::Layout;
use core::mem::size_of;

/// ABI/format version. Bumped whenever an exported signature changes meaning, so a stale
/// committed `.wasm` cannot be paired with a newer loader unnoticed.
pub const ABI_VERSION: u32 = 1;

/// Alignment of every block handed to the caller. Wide enough for `f64`, and it keeps
/// `f32`/`i16` views aligned too, so any typed-array view on a returned pointer is legal.
pub const BLOCK_ALIGN: usize = 8;

/// Round `bytes` up to the allocation granularity (also the smallest allocation).
#[inline]
pub fn block_bytes(bytes: usize) -> usize {
    let rounded = bytes.saturating_add(BLOCK_ALIGN - 1) & !(BLOCK_ALIGN - 1);
    rounded.max(BLOCK_ALIGN)
}

#[inline]
fn layout(bytes: usize) -> Layout {
    // block_bytes() guarantees a non-zero size, so Layout::from_size_align cannot fail here.
    match Layout::from_size_align(block_bytes(bytes), BLOCK_ALIGN) {
        Ok(layout) => layout,
        Err(_) => panic!("invalid block layout"),
    }
}

/// Allocate a zeroed block for `bytes` of payload. Returns 0 when the host allocation fails
/// (the caller must treat 0 as "out of memory", never as a valid pointer).
#[no_mangle]
pub extern "C" fn websa_dsp_alloc(bytes: usize) -> *mut u8 {
    let layout = layout(bytes);
    // SAFETY: layout has a non-zero size, which `alloc` requires.
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        return core::ptr::null_mut();
    }
    ptr
}

/// Free a block from [`websa_dsp_alloc`]. `bytes` is the value the block was allocated with.
///
/// # Safety
/// `ptr` must come from `websa_dsp_alloc` (or be null) and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn websa_dsp_free(ptr: *mut u8, bytes: usize) {
    if ptr.is_null() {
        return;
    }
    std::alloc::dealloc(ptr, layout(bytes));
}

/// Size of one `f32` in bytes, so the loader derives strides from the module instead of
/// hardcoding an assumption about the target.
#[no_mangle]
pub extern "C" fn websa_dsp_f32_bytes() -> u32 {
    size_of::<f32>() as u32
}

/// Alignment (bytes) of every block returned by `websa_dsp_alloc`.
#[no_mangle]
pub extern "C" fn websa_dsp_block_align() -> u32 {
    BLOCK_ALIGN as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_are_rounded_up_and_never_zero() {
        assert_eq!(block_bytes(0), BLOCK_ALIGN);
        assert_eq!(block_bytes(1), BLOCK_ALIGN);
        assert_eq!(block_bytes(BLOCK_ALIGN), BLOCK_ALIGN);
        assert_eq!(block_bytes(BLOCK_ALIGN + 1), BLOCK_ALIGN * 2);
    }

    #[test]
    fn allocate_and_free_round_trip() {
        let ptr = websa_dsp_alloc(1024);
        assert!(!ptr.is_null());
        assert_eq!(ptr as usize % BLOCK_ALIGN, 0, "blocks must be aligned for typed views");
        // Zeroed: the loader writes the payload, the kernel must not see garbage before it does.
        // SAFETY: the block is 1024 bytes.
        assert!(unsafe { std::slice::from_raw_parts(ptr, 1024) }.iter().all(|b| *b == 0));
        // SAFETY: same pointer and size that were allocated.
        unsafe { websa_dsp_free(ptr, 1024) };
    }

    #[test]
    fn the_abi_version_is_pinned() {
        // The loader rejects a module whose version it does not know; bump both sides together.
        assert_eq!(ABI_VERSION, 1);
    }
}
