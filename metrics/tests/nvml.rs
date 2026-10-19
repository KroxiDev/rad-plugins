use rad_plugin_metrics::nvml::Nvml;
use std::{ffi::CString, process::Command};
#[test]
fn nvml_abi_targets_only_index_zero_and_separates_gpu_from_memory_activity() {
    let dir = std::env::temp_dir().join(format!("rad-metrics-nvml-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("fixture.so");
    assert!(
        Command::new("rustc")
            .args([
                "--crate-type",
                "cdylib",
                "--edition=2024",
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/nvml.rs"),
                "-o"
            ])
            .arg(&lib)
            .status()
            .unwrap()
            .success()
    );
    let name = CString::new(lib.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: our fixture implements precisely the NVML ABI used by this adapter.
    let nvml = unsafe { Nvml::load(&name) }.expect("load controlled library");
    let (gpu, ram) = nvml.read();
    assert_eq!(gpu, Some(73)); // not the memory-controller activity value 9
    let ram = ram.unwrap();
    assert_eq!((ram.used, ram.total), (2 * 1073741824, 8 * 1073741824));
    drop(nvml);
    // A temporarily missing external library must recover on the same worker.
    let name_for_loader = name.clone();
    let mut attempts = 0;
    let mut reader = rad_plugin_metrics::reader::Reader::with_nvml_loader(dir.clone(), move || {
        attempts += 1;
        if attempts == 1 {
            None
        } else {
            unsafe { Nvml::load(&name_for_loader) }
        }
    });
    std::fs::write(dir.join("stat"), "cpu 10 0 0 90 0 0 0 0").unwrap();
    std::fs::write(
        dir.join("meminfo"),
        "MemTotal: 1000 kB\nMemAvailable: 400 kB",
    )
    .unwrap();
    let first = reader.read();
    assert!(first.gpu.is_none());
    assert!(first.cpu.is_none(), "CPU needs two samples");
    assert_eq!(first.ram.unwrap().used, 600 * 1024);
    std::fs::write(dir.join("stat"), "cpu 20 0 0 100 0 0 0 0").unwrap();
    let second = reader.read();
    assert_eq!(second.gpu, Some(73));
    assert_eq!(second.cpu, Some(50));
    std::fs::remove_file(dir.join("stat")).unwrap();
    assert!(reader.read().cpu.is_none());
    std::fs::write(dir.join("stat"), "cpu 30 0 0 110 0 0 0 0").unwrap();
    assert!(
        reader.read().cpu.is_none(),
        "no fabricated delta across missing sample"
    );
    drop(reader);
    let missing = CString::new(dir.join("absent.so").as_os_str().as_encoded_bytes()).unwrap();
    assert!(unsafe { Nvml::load(&missing) }.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}
