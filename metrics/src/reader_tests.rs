use super::*;
use std::{cell::Cell, ffi::CString, rc::Rc};

#[test]
fn cpu_and_ram_demand_do_not_touch_other_sources_and_release_resets_cpu() {
    let root = std::env::temp_dir().join(format!("rad-metrics-demand-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("stat"), "cpu 10 0 0 90 0 0 0 0").unwrap();
    std::fs::write(
        root.join("meminfo"),
        "MemTotal: 1000 kB\nMemAvailable: 400 kB",
    )
    .unwrap();
    // Observe reads of only these synthetic files; no proc/sensors/driver access.
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    assert!(fd >= 0);
    let watch = |file: &str| {
        let path = CString::new(root.join(file).as_os_str().as_encoded_bytes()).unwrap();
        let wd = unsafe { libc::inotify_add_watch(fd, path.as_ptr(), libc::IN_ACCESS) };
        assert!(wd >= 0);
        wd
    };
    let stat = watch("stat");
    let ram = watch("meminfo");
    let reads = || {
        let mut events = [0_u8; 1024];
        let n = unsafe { libc::read(fd, events.as_mut_ptr().cast(), events.len()) };
        if n < 0 {
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock
            );
            return Vec::new();
        }
        let mut result = Vec::new();
        let mut offset = 0;
        while offset < n as usize {
            let event = unsafe {
                std::ptr::read_unaligned(events.as_ptr().add(offset).cast::<libc::inotify_event>())
            };
            result.push(event.wd);
            offset += std::mem::size_of::<libc::inotify_event>() + event.len as usize;
        }
        result
    };
    let loads = Rc::new(Cell::new(0));
    let observed = loads.clone();
    let mut reader = Reader::with_nvml_loader(root.clone(), move || {
        observed.set(observed.get() + 1);
        None
    });
    assert_eq!(loads.get(), 0, "constructor is inert");
    let mut demand = Demand::default();
    assert!(!demand.active());
    reader.read_demand(demand, false);
    assert!(reads().is_empty());
    demand.set([true, false, false, false]);
    assert!(reader.read_demand(demand, true).cpu.is_none());
    assert_eq!(reads(), [stat], "CPU-only must not read meminfo");
    assert_eq!(loads.get(), 0, "CPU-only must not load NVML");
    std::fs::write(root.join("stat"), "cpu 20 0 0 100 0 0 0 0").unwrap();
    assert_eq!(reader.read_demand(demand, true).cpu, Some(50));
    reads();
    demand.set([false, true, false, false]);
    assert_eq!(
        reader.read_demand(demand, true).ram.unwrap().used,
        600 * 1024
    );
    assert_eq!(reads(), [ram], "RAM-only must not read stat");
    assert_eq!(loads.get(), 0, "RAM-only must not load NVML");
    demand.set([true, false, false, false]);
    assert!(
        reader.read_demand(demand, true).cpu.is_none(),
        "fresh CPU baseline after release"
    );
    reads();
    // The publisher can coalesce off/on before the worker sees the intermediate mask.
    demand.set([false; 4]);
    demand.set([true, false, false, false]);
    assert!(reader.read_demand(demand, true).cpu.is_none());
    reads();
    demand.set([false; 4]);
    reader.read_demand(demand, false);
    assert!(reads().is_empty());
    unsafe {
        libc::close(fd);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn gpu_and_vram_share_one_library_query_selectively_and_unload_while_cpu_continues() {
    let root = std::env::temp_dir().join(format!("rad-metrics-demand-nvml-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("fixture.so");
    assert!(
        std::process::Command::new("rustc")
            .args([
                "--crate-type",
                "cdylib",
                "--edition=2024",
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/nvml.rs"),
                "-o"
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let name = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // Hold a diagnostic reference only while reading fake ABI counters.
    let diagnostic = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    assert!(!diagnostic.is_null());
    let counts: unsafe extern "C" fn(usize) -> u32 = unsafe {
        let symbol = libc::dlsym(diagnostic, c"fixture_count".as_ptr());
        assert!(!symbol.is_null());
        std::mem::transmute(symbol)
    };
    let snapshot = || unsafe { [counts(0), counts(1), counts(2), counts(3)] };
    let load_name = name.clone();
    let mut reader =
        Reader::with_nvml_loader(root.clone(), move || unsafe { Nvml::load(&load_name) });
    assert_eq!(snapshot(), [0; 4]);
    let mut demand = Demand::default();
    demand.set([false, false, true, false]);
    assert_eq!(reader.read_demand(demand, true).gpu, Some(73));
    assert_eq!(snapshot(), [1, 1, 0, 0], "GPU does not query VRAM");
    demand.set([false, false, true, true]);
    let sample = reader.read_demand(demand, true);
    assert!(sample.gpu.is_some() && sample.vram.is_some());
    assert_eq!(snapshot(), [1, 2, 1, 0], "GPU and VRAM share NVML");
    demand.set([false, false, false, true]);
    assert!(reader.read_demand(demand, true).vram.is_some());
    assert_eq!(snapshot(), [1, 2, 2, 0], "VRAM does not query utilization");
    demand.set([true, false, false, true]);
    std::fs::write(root.join("stat"), "cpu 10 0 0 90 0 0 0 0").unwrap();
    assert!(reader.read_demand(demand, true).cpu.is_none());
    demand.set([true, false, false, false]);
    reader.read_demand(demand, false);
    assert_eq!(
        snapshot(),
        [1, 2, 3, 1],
        "last NVML demand releases without a sample"
    );
    unsafe {
        libc::dlclose(diagnostic);
    }
    let mapped = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
    assert!(mapped.is_null(), "reader dropped its library handle");
    std::fs::write(root.join("stat"), "cpu 20 0 0 100 0 0 0 0").unwrap();
    assert_eq!(
        reader.read_demand(demand, true).cpu,
        Some(50),
        "CPU baseline survives unrelated GPU release"
    );
    demand.set([true, false, true, false]);
    assert_eq!(reader.read_demand(demand, true).gpu, Some(73));
    demand.set([false; 4]);
    reader.read_demand(demand, false);
    assert!(unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) }.is_null());
    drop(reader);
    std::fs::remove_dir_all(root).unwrap();
}
