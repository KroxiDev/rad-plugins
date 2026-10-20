use rad_plugin_metrics::polling::{Poller, Sample};
use rad_plugin_sdk::VisualState;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}
fn current(s: &VisualState) -> bool {
    matches!(s, VisualState::Current { .. })
}
#[test]
fn one_shared_poll_per_second_independent_of_indicator_count() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let (tx, rx) = mpsc::channel();
    let mut poller = Poller::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
        tx.send(()).unwrap();
        Sample {
            cpu: Some(25),
            gpu: Some(80),
            ..Default::default()
        }
    })
    .unwrap();
    poller.tick(ms(0), false);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    poller.tick(ms(0), true);
    rx.recv_timeout(Duration::from_secs(1))
        .expect("first shared read");
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let states = poller.tick(ms(1), true);
        if current(&states[0]) {
            assert!(current(&states[2]));
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    for t in 2..1000 {
        poller.tick(ms(t), true);
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    poller.tick(ms(1000), true);
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn blocked_reader_expires_without_retries_or_blocking_teardown() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let mut poller = Poller::new(move || {
        let n = c.fetch_add(1, Ordering::SeqCst);
        started_tx.send(()).unwrap();
        if n > 0 {
            release_rx.recv().unwrap();
        }
        Sample {
            cpu: Some(25),
            ..Default::default()
        }
    })
    .unwrap();
    poller.tick(ms(0), true);
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !current(&poller.tick(ms(1), true)[0]) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    poller.tick(ms(1000), true);
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        poller.tick(ms(1500), true)[0],
        VisualState::Stale { .. }
    ));
    for t in 2500..10000 {
        assert!(!current(&poller.tick(ms(t), true)[0]));
    }
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let before = std::time::Instant::now();
    drop(poller);
    assert!(before.elapsed() < Duration::from_millis(100));
    release_tx.send(()).unwrap();
}

#[test]
fn missing_invalid_and_failed_samples_never_masquerade_as_current() {
    use rad_plugin_metrics::readings::Memory;
    let (tx, rx) = mpsc::channel();
    let mut poller = Poller::new(move || {
        tx.send(()).unwrap();
        Sample {
            cpu: Some(101),
            gpu: None,
            ram: Some(Memory {
                used: 11,
                available: 0,
                total: 10,
            }),
            vram: None,
        }
    })
    .unwrap();
    poller.tick(ms(0), true);
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    for t in 1..1000 {
        assert!(
            poller
                .tick(ms(t), true)
                .iter()
                .all(|s| matches!(s, VisualState::Unavailable))
        );
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        assert!(
            poller
                .tick(ms(1000), true)
                .iter()
                .all(|s| matches!(s, VisualState::Unavailable))
        );
        if rx.try_recv().is_ok() {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn late_completion_is_discarded_and_reader_is_not_replaced() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let mut poller = Poller::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Sample {
            cpu: Some(90),
            ..Default::default()
        }
    })
    .unwrap();
    poller.tick(ms(0), true);
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        poller.tick(ms(1500), true)[0],
        VisualState::Unavailable
    ));
    release_tx.send(()).unwrap();
    poller.tick(ms(1501), false);
    poller.tick(ms(1502), true);
    for t in 1503..10000 {
        assert!(matches!(
            poller.tick(ms(t), true)[0],
            VisualState::Unavailable
        ));
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn last_release_and_resubscribe_never_reuse_current_sample() {
    let mut poller = Poller::new(|| Sample {
        cpu: Some(25),
        ..Default::default()
    })
    .unwrap();
    poller.tick(ms(0), true);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !current(&poller.tick(ms(1), true)[0]) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!current(&poller.tick(ms(2), false)[0]));
    assert!(!current(&poller.tick(ms(3), true)[0]));
}

#[test]
fn in_flight_sample_from_previous_subscription_epoch_is_discarded() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut poller = Poller::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Sample {
            cpu: Some(37),
            ..Default::default()
        }
    })
    .unwrap();
    poller.tick(ms(0), true);
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    poller.tick(ms(1), false);
    poller.tick(ms(2), true);
    release_tx.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let states = poller.tick(ms(1000), true);
        assert!(!current(&states[0]), "old in-flight epoch became Current");
        if entered_rx.try_recv().is_ok() {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    drop(poller);
    release_tx.send(()).unwrap();
}
