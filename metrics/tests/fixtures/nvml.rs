// Controlled C ABI fixture, not NVIDIA's library. No hardware or system reads.
use std::{
    ffi::{c_uint, c_void},
    sync::atomic::{AtomicU32, Ordering},
};
static COUNTS: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];
#[unsafe(no_mangle)]
pub extern "C" fn fixture_count(index: usize) -> u32 {
    COUNTS[index].load(Ordering::SeqCst)
}
#[repr(C)]
pub struct Util {
    gpu: c_uint,
    memory: c_uint,
}
#[repr(C)]
pub struct Mem {
    total: u64,
    free: u64,
    used: u64,
}
#[unsafe(no_mangle)]
pub extern "C" fn nvmlInit_v2() -> i32 {
    COUNTS[0].fetch_add(1, Ordering::SeqCst);
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn nvmlShutdown() -> i32 {
    COUNTS[3].fetch_add(1, Ordering::SeqCst);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nvmlDeviceGetHandleByIndex_v2(
    index: c_uint,
    out: *mut *mut c_void,
) -> i32 {
    if index != 0 {
        return 6;
    }
    unsafe { *out = std::ptr::dangling_mut::<c_void>() };
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nvmlDeviceGetUtilizationRates(_: *mut c_void, out: *mut Util) -> i32 {
    COUNTS[1].fetch_add(1, Ordering::SeqCst);
    unsafe { *out = Util { gpu: 73, memory: 9 } };
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nvmlDeviceGetMemoryInfo(_: *mut c_void, out: *mut Mem) -> i32 {
    COUNTS[2].fetch_add(1, Ordering::SeqCst);
    unsafe {
        *out = Mem {
            total: 8 * 1073741824,
            free: 6 * 1073741824,
            used: 2 * 1073741824,
        }
    };
    0
}
