//! Fault injection for the Rust libxaac integration, without loading the codec archive.
use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Default)]
struct Sdk {
    fail: Option<(i32, i32)>,
    fail_alloc: usize,
    allocation_attempts: usize,
    allocations: BTreeMap<usize, (Layout, Layout)>,
    layout_mismatches: usize,
    table_ptrs: [usize; 4],
    output_bytes: u32,
    input_bytes_calls: usize,
}

thread_local! { static SDK: RefCell<Sdk> = RefCell::new(Sdk::default()); }

struct Fixture;
impl Fixture {
    fn new() -> Self {
        SDK.with(|slot| {
            assert!(slot.borrow().allocations.is_empty());
            *slot.borrow_mut() = Sdk { output_bytes: 4, ..Sdk::default() };
        });
        Self
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Red tests intentionally expose leaks. Reclaim their real allocations even when an
        // assertion unwinds; the wrapper's failure remains visible in the allocation map.
        SDK.with(|slot| {
            let outstanding = std::mem::take(&mut slot.borrow_mut().allocations);
            for (ptr, (_, actual)) in outstanding {
                unsafe { std::alloc::dealloc(ptr as *mut u8, actual); }
            }
        });
    }
}

pub(super) unsafe fn alloc(layout: Layout) -> *mut u8 {
    SDK.with(|slot| {
        let mut sdk = slot.borrow_mut();
        sdk.allocation_attempts += 1;
        if sdk.fail_alloc == sdk.allocation_attempts { return core::ptr::null_mut(); }
        // The backing allocation includes a guard region so the pre-fix oversized-copy
        // regression can detect an SDK contract violation without writing outside live memory.
        let actual = Layout::from_size_align(layout.size() + 64, layout.align()).unwrap();
        let ptr = std::alloc::alloc_zeroed(actual);
        if !ptr.is_null() { sdk.allocations.insert(ptr as usize, (layout, actual)); }
        ptr
    })
}

pub(super) unsafe fn dealloc(ptr: *mut u8, layout: Layout) {
    SDK.with(|slot| {
        let mut sdk = slot.borrow_mut();
        let (requested, actual) = sdk.allocations.remove(&(ptr as usize)).expect("owned allocation");
        if layout != requested { sdk.layout_mismatches += 1; }
        // Always free the actual allocation correctly, including while testing old wrong layouts.
        std::alloc::dealloc(ptr, actual);
    });
}

pub(super) unsafe fn ixheaacd_dec_api(
    _object: *mut core::ffi::c_void, command: i32, index: i32, value: *mut core::ffi::c_void,
) -> i32 {
    SDK.with(|slot| {
        let mut sdk = slot.borrow_mut();
        if sdk.fail == Some((command, index)) { return -7; }
        match command {
            CMD_GET_API_SIZE | CMD_GET_MEMTABS_SIZE => *(value as *mut u32) = 64,
            CMD_GET_MEM_INFO_SIZE => *(value as *mut u32) = 32,
            CMD_GET_MEM_INFO_ALIGNMENT => *(value as *mut u32) = 64,
            CMD_GET_MEM_INFO_TYPE => *(value as *mut u32) = match index {
                2 => MEMTYPE_INPUT, 3 => MEMTYPE_OUTPUT, _ => 0,
            },
            CMD_SET_MEM_PTR => sdk.table_ptrs[index as usize] = value as usize,
            CMD_SET_INPUT_BYTES => sdk.input_bytes_calls += 1,
            CMD_GET_OUTPUT_BYTES => *(value as *mut u32) = sdk.output_bytes,
            CMD_INIT if index == INIT_DONE_QUERY => *(value as *mut u32) = 1,
            CMD_EXECUTE => {
                let output = sdk.table_ptrs[3] as *mut i16;
                if !output.is_null() { *output = 123; *output.add(1) = -456; }
            }
            _ => {}
        }
        0
    })
}

#[test]
fn initialization_errors_release_every_successful_allocation() {
    for failure in [(CMD_INIT, INIT_PRE_CONFIG), (CMD_INIT, INIT_POST_CONFIG),
        (CMD_GET_MEMTABS_SIZE, 0), (CMD_GET_MEM_INFO_SIZE, 1), (CMD_SET_MEM_PTR, 2)] {
        let _fixture = Fixture::new();
        SDK.with(|slot| slot.borrow_mut().fail = Some(failure));
        let result = XaacDecoder::new();
        let rejected = result.is_none();
        drop(result);
        let remaining = SDK.with(|slot| slot.borrow().allocations.len());
        assert!(rejected, "SDK failure {failure:?} cannot create a usable decoder");
        assert_eq!(remaining, 0, "SDK failure {failure:?} leaked owned buffers");
    }
}

#[test]
fn allocation_failure_cleans_up_earlier_tables() {
    for failure in 1..=6 {
        let _fixture = Fixture::new();
        SDK.with(|slot| slot.borrow_mut().fail_alloc = failure);
        let result = XaacDecoder::new();
        let rejected = result.is_none();
        drop(result);
        assert!(rejected, "allocation {failure} failure must reject initialization");
        assert_eq!(SDK.with(|slot| slot.borrow().allocations.len()), 0);
    }
}

#[test]
fn table_destruction_uses_the_sdk_allocation_alignment() {
    let _fixture = Fixture::new();
    let decoder = XaacDecoder::new().unwrap();
    drop(decoder);
    SDK.with(|slot| {
        let sdk = slot.borrow();
        assert!(sdk.allocations.is_empty());
        assert_eq!(sdk.layout_mismatches, 0, "deallocation must use the exact original Layout");
    });
}

#[test]
fn oversized_input_is_rejected_before_copy_and_the_next_unit_can_decode() {
    let _fixture = Fixture::new();
    let mut decoder = XaacDecoder::new().unwrap();
    let result = decoder.feed(&[0x55; 48], false);
    assert!(result.is_none(), "48 input bytes do not fit the SDK's 32-byte input table");
    assert_ne!(decoder.last_error, 0);
    SDK.with(|slot| {
        let sdk = slot.borrow();
        assert_eq!(sdk.input_bytes_calls, 0);
        let guard = unsafe { core::slice::from_raw_parts((sdk.table_ptrs[2] as *const u8).add(32), 16) };
        assert!(guard.iter().all(|byte| *byte == 0), "input copy crossed the SDK table boundary");
    });
    assert_eq!(decoder.feed(&[1, 2, 3], false), Some(vec![123, -456]));
}

#[test]
fn output_byte_count_is_validated_before_reading_pcm() {
    for output_bytes in [3, 48] {
        let _fixture = Fixture::new();
        let mut decoder = XaacDecoder::new().unwrap();
        SDK.with(|slot| slot.borrow_mut().output_bytes = output_bytes);
        assert!(decoder.feed(&[1], false).is_none(), "invalid PCM byte count {output_bytes}");
        assert_ne!(decoder.last_error, 0);
    }
}
