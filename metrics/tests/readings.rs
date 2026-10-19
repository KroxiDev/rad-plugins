use rad_plugin_metrics::readings::Cpu;

#[test]
fn aggregate_cpu_excludes_guest_and_detects_resets_and_idle() {
    let first = Cpu::parse("cpu 100 0 20 800 10 5 5 60 70 0\ncpu0 0").unwrap();
    let next = Cpu::parse("cpu 120 0 30 850 20 5 5 70 90 0\n").unwrap();
    // 100 ticks total: idle+iowait 60; busy including steal 40.
    assert_eq!(next.percent_since(first), Some(40));
    assert_eq!(first.percent_since(next), None);
    assert_eq!(next.percent_since(next), None);
    assert!(Cpu::parse("cpu 1 2 broken").is_none());
}

#[test]
fn ram_uses_available_not_free_and_rejects_invalid_or_unbounded_input() {
    use rad_plugin_metrics::readings::{MAX_PROC_BYTES, Memory, read_bounded};
    let ram = Memory::parse("MemTotal: 1000 kB\nMemFree: 10 kB\nMemAvailable: 400 kB\n").unwrap();
    assert_eq!(
        (ram.used, ram.available, ram.total),
        (600 * 1024, 400 * 1024, 1000 * 1024)
    );
    for bad in [
        "MemTotal: 1 kB\nMemAvailable: 2 kB",
        "MemTotal: 0 kB\nMemAvailable: 0 kB",
        "MemTotal: 4 MB\nMemAvailable: 2 kB",
        "MemTotal: 4 kB",
        "MemTotal: 4 kB\nMemTotal: 8 kB\nMemAvailable: 2 kB",
    ] {
        assert!(Memory::parse(bad).is_none(), "{bad}");
    }
    assert!(read_bounded(vec![b'a'; MAX_PROC_BYTES as usize + 1].as_slice()).is_err());
    assert_eq!(read_bounded(b"fixture".as_slice()).unwrap(), "fixture");
}
