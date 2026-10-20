//! Deliberately small NVML allowlist: init, one device, utilization, memory, shutdown.
//! Uses the NVML C ABI: `repr(C)` utilization fields are unsigned ints; v1 memory
//! fields (total/free/used) are unsigned long long byte counts. GPU activity is
//! `utilization.gpu`, not memory activity. Only device index 0 is queried; its
//! physical identity is not stable across reboots. Failed or invalid reads are absent.
use crate::readings::Memory;
use std::ffi::{CStr, c_int, c_uint, c_ulonglong, c_void};

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}
#[repr(C)]
#[derive(Default)]
struct DeviceMemory {
    total: c_ulonglong,
    free: c_ulonglong,
    used: c_ulonglong,
}
type Init = unsafe extern "C" fn() -> c_int;
type Handle = unsafe extern "C" fn(c_uint, *mut *mut c_void) -> c_int;
type Util = unsafe extern "C" fn(*mut c_void, *mut Utilization) -> c_int;
type Mem = unsafe extern "C" fn(*mut c_void, *mut DeviceMemory) -> c_int;

struct Library(*mut c_void);
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: this is the unique owning handle returned by dlopen.
        unsafe {
            libc::dlclose(self.0);
        }
    }
}
pub struct Nvml {
    _library: Library,
    shutdown: Init,
    device: *mut c_void,
    utilization: Util,
    memory: Mem,
}
impl Nvml {
    /// # Safety
    /// The library must implement the documented NVIDIA NVML C ABI. The caller must
    /// keep this object confined to its reader thread; do not concurrently unload it.
    pub unsafe fn load(path: &CStr) -> Option<Self> {
        // SAFETY: path is NUL terminated; all symbols below have their exact NVML ABI.
        unsafe {
            let raw = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if raw.is_null() {
                return None;
            }
            let library = Library(raw);
            macro_rules! symbol {
                ($name:expr, $ty:ty) => {{
                    let ptr = libc::dlsym(raw, $name.as_ptr());
                    if ptr.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, $ty>(ptr)
                }};
            }
            let init = symbol!(c"nvmlInit_v2", Init);
            let shutdown = symbol!(c"nvmlShutdown", Init);
            let handle = symbol!(c"nvmlDeviceGetHandleByIndex_v2", Handle);
            let utilization = symbol!(c"nvmlDeviceGetUtilizationRates", Util);
            let memory = symbol!(c"nvmlDeviceGetMemoryInfo", Mem);
            if init() != 0 {
                return None;
            }
            let mut device = std::ptr::null_mut();
            // Exactly index 0. No enumeration/count, fallback GPU, or sensor scan.
            if handle(0, &mut device) != 0 || device.is_null() {
                shutdown();
                return None;
            }
            Some(Self {
                _library: library,
                shutdown,
                device,
                utilization,
                memory,
            })
        }
    }
    pub fn read(&self) -> (Option<u32>, Option<Memory>) {
        self.read_selected(true, true)
    }
    pub(crate) fn read_selected(&self, gpu: bool, vram: bool) -> (Option<u32>, Option<Memory>) {
        let (mut util, mut mem) = (Utilization::default(), DeviceMemory::default());
        // SAFETY: loaded function signatures and repr(C) outputs match NVML.
        let gpu = (gpu && unsafe { (self.utilization)(self.device, &mut util) == 0 })
            .then_some(util.gpu)
            .filter(|v| *v <= 100);
        let memory = if vram
            && unsafe { (self.memory)(self.device, &mut mem) == 0 }
            && mem.total > 0
            && mem.used <= mem.total
            && mem.free <= mem.total
            && mem.used.checked_add(mem.free) == Some(mem.total)
        {
            Some(Memory {
                used: mem.used,
                available: mem.free,
                total: mem.total,
            })
        } else {
            None
        };
        (gpu, memory)
    }
}
impl Drop for Nvml {
    fn drop(&mut self) {
        // Runs only on the isolated reader, never in plugin/supervisor teardown.
        unsafe {
            (self.shutdown)();
        }
    }
}
