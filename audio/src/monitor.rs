//! The event monitor: keeps a `pactl subscribe` child while something is watching, and turns the
//! server's events into refreshes of the device picture.
//!
//! Events only say "something about a sink, source or the defaults changed"; the picture is then
//! read again (see [`server::snapshot`]). A burst of events is one refresh: it waits for a quiet
//! moment, but never longer than a bound. If the event stream ends, the server went away: the
//! watchers are told, and the monitor reconnects after a wait that grows while the server stays
//! away. A failure inside the watching (a panic) ends only that session, and the watching starts
//! over after the same kind of growing wait.
//!
//! There is at most one monitor thread, and so at most one event child. Stopping only asks the
//! thread to wind down; if watching is wanted again before it has, the same thread starts over
//! instead of a second one being added. Each time watching is wanted again is a new epoch, which
//! tags the pictures the thread publishes, so a picture that began before the stop is never taken
//! for a fresh one.
use crate::{
    Pactl, Snapshot, Timing,
    server::{self, Failure},
};
use std::{
    io::Read,
    panic::{AssertUnwindSafe, catch_unwind},
    process::{Child, ChildStdout},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread,
    time::{Duration, Instant},
};

/// Longest event line kept; real ones are about 30 bytes, anything longer is not an event.
const MAX_LINE: usize = 256;

/// Receives each new picture of the server, or why there is none, tagged with the epoch it was
/// taken in. Called on the monitor's thread.
pub(crate) type Publish = Box<dyn Fn(u64, Result<Snapshot, Failure>) + Send + Sync + 'static>;

/// The shortest first wait: doubling zero would never get anywhere.
const MIN_RETRY_WAIT: Duration = Duration::from_millis(1);

/// The waits between tries: `first`, then doubling up to `most`, until the server answers.
struct Backoff {
    first: Duration,
    most: Duration,
    next: Duration,
}

impl Backoff {
    fn new(timing: &Timing) -> Self {
        let first = timing.retry_wait.max(MIN_RETRY_WAIT);
        Self {
            first,
            most: timing.retry_max.max(first),
            next: first,
        }
    }

    /// The wait to use now; the one after it is twice as long, within the limit.
    fn next(&mut self) -> Duration {
        let wait = self.next;
        self.next = wait.saturating_mul(2).min(self.most);
        wait
    }

    fn reset(&mut self) {
        self.next = self.first;
    }
}

struct Shared {
    pactl: Pactl,
    timing: Timing,
    publish: Publish,
    control: Control,
}

/// Watches the server while asked to. Cheap to create: the thread exists only while watching.
pub(crate) struct Monitor {
    shared: Arc<Shared>,
}

impl Monitor {
    pub(crate) fn new(pactl: Pactl, timing: Timing, publish: Publish) -> Self {
        Self {
            shared: Arc::new(Shared {
                pactl,
                timing,
                publish,
                control: Control::default(),
            }),
        }
    }

    /// Starts watching, or resumes if a stop has not finished yet, and returns the new epoch.
    /// `None` if the thread could not be started.
    pub(crate) fn start(&self) -> Option<u64> {
        let control = &self.shared.control;
        let mut state = control.lock();
        if !state.wanted {
            state.wanted = true;
            state.epoch += 1;
            control.wake.notify_all();
        }
        if !state.running {
            state.running = true;
            let shared = Arc::clone(&self.shared);
            let spawned = thread::Builder::new()
                .name("audio-monitor".into())
                .spawn(move || thread_main(&shared));
            if spawned.is_err() {
                state.running = false;
                state.wanted = false;
                return None;
            }
        }
        Some(state.epoch)
    }

    /// Coalesces a request for a new observation with pending source events.
    pub(crate) fn refresh(&self) {
        self.shared.control.event();
    }

    /// Asks the thread to wind down: it ends its event child and exits unless watching is wanted
    /// again by then.
    pub(crate) fn stop(&self) {
        self.shared.control.lock().wanted = false;
        self.shared.control.wake.notify_all();
    }
}

/// What wakes the monitor: events from the reader thread, the end of the stream, a stop, or a
/// change of epoch.
#[derive(Default)]
struct Control {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    wanted: bool,
    epoch: u64,
    /// The monitor thread is alive and has not decided to exit.
    running: bool,
    /// The event stream closed.
    ended: bool,
    /// When the first and the latest unprocessed events arrived.
    pending: Option<(Instant, Instant)>,
}

enum Wake {
    Stop,
    Events,
    Ended,
    /// The time set for trying again has come.
    Retry,
}

impl Control {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn event(&self) {
        let now = Instant::now();
        let mut state = self.lock();
        let first = state.pending.map_or(now, |(first, _)| first);
        state.pending = Some((first, now));
        self.wake.notify_all();
    }

    fn end(&self) {
        self.lock().ended = true;
        self.wake.notify_all();
    }
}

/// One stretch of watching: it ends when watching is no longer wanted or a newer epoch began.
struct Lease<'a> {
    control: &'a Control,
    epoch: u64,
}

impl Lease<'_> {
    fn over(&self, state: &State) -> bool {
        !state.wanted || state.epoch != self.epoch
    }

    fn is_over(&self) -> bool {
        self.over(&self.control.lock())
    }

    /// Forgets the previous stream before a new one starts.
    fn reset(&self) {
        let mut state = self.control.lock();
        state.ended = false;
        state.pending = None;
    }

    /// Blocks until there is something to do: the end of the lease, events (before the end of the
    /// stream) or `retry_at`, if there is one.
    fn wait(&self, retry_at: Option<Instant>) -> Wake {
        let mut state = self.control.lock();
        loop {
            if self.over(&state) {
                return Wake::Stop;
            }
            if state.pending.is_some() {
                return Wake::Events;
            }
            if state.ended {
                return Wake::Ended;
            }
            let Some(retry_at) = retry_at else {
                state = self
                    .control
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            let left = retry_at.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Wake::Retry;
            }
            state = self
                .control
                .wake
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Waits until events have been quiet for `quiet`, or the first has waited `most`, then
    /// takes them. `false` if the lease ended meanwhile.
    fn settle(&self, quiet: Duration, most: Duration) -> bool {
        let mut state = self.control.lock();
        loop {
            if self.over(&state) {
                return false;
            }
            let Some((first, last)) = state.pending else {
                return true;
            };
            let end = (last + quiet).min(first + most);
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() || state.ended {
                state.pending = None;
                return true;
            }
            state = self
                .control
                .wake
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Sleeps for `duration` unless the lease ends first; `false` if it ended. A duration beyond
    /// what the clock can add to now never runs out: the sleep lasts until the lease ends.
    fn sleep(&self, duration: Duration) -> bool {
        let end = Instant::now().checked_add(duration);
        let mut state = self.control.lock();
        while !self.over(&state) {
            let Some(end) = end else {
                state = self
                    .control
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            state = self
                .control
                .wake
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        false
    }
}

fn thread_main(shared: &Arc<Shared>) {
    // The wait before starting over after a panic. It grows while the panics keep coming, so a
    // failure that always happens is not retried in a tight loop, and starts over once a lease
    // has run its course (see also `follow`).
    let mut backoff = Backoff::new(&shared.timing);
    loop {
        let epoch = {
            let mut state = shared.control.lock();
            if !state.wanted {
                // Deciding to exit and saying so happen under one lock, so a start that comes
                // next sees no thread and makes a new one.
                state.running = false;
                return;
            }
            state.epoch
        };
        let lease = Lease {
            control: &shared.control,
            epoch,
        };
        // A panic in the watching (a watcher that fails, say) must not leave the plugin
        // believing it is watching when nothing is: the session's event child is ended on the
        // way out, and the lease starts over after a wait.
        match catch_unwind(AssertUnwindSafe(|| watch(shared, &lease, &mut backoff))) {
            Ok(()) => backoff.reset(),
            Err(_) => {
                lease.sleep(backoff.next());
            }
        }
    }
}

/// The `pactl subscribe` child and the thread reading it. Ends both when dropped, also while
/// unwinding from a panic.
struct EventStream {
    child: Child,
    reader: Option<thread::JoinHandle<()>>,
}

impl Drop for EventStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Watches the server for one lease, reconnecting as often as it takes. `backoff` sets the waits
/// between tries.
fn watch(shared: &Arc<Shared>, lease: &Lease<'_>, backoff: &mut Backoff) {
    let Shared {
        pactl,
        timing,
        publish,
        ..
    } = &**shared;
    let publish = |picture| publish(lease.epoch, picture);
    while !lease.is_over() {
        lease.reset();
        let mut scheduled = None;
        if let Ok((child, stdout)) = pactl.subscribe() {
            let mut stream = EventStream {
                child,
                reader: None,
            };
            let reader_shared = Arc::clone(shared);
            stream.reader = thread::Builder::new()
                .name("audio-events".into())
                .spawn(move || read_events(stdout, &reader_shared.control))
                .ok();
            if stream.reader.is_some() {
                scheduled = follow(pactl, timing, lease, &publish, backoff);
            }
        }
        if lease.is_over() {
            return;
        }
        // The stream ended or never started: the server is away. Wait as long as the last failed
        // refresh already said to, or one step further if nothing had failed yet.
        publish(Err(Failure::Unavailable));
        if !lease.sleep(scheduled.unwrap_or_else(|| backoff.next())) {
            return;
        }
    }
}

fn refresh(pactl: &Pactl, timing: &Timing) -> Result<Snapshot, Failure> {
    server::snapshot(pactl, Instant::now() + timing.refresh_deadline)
}

/// Keeps the picture current until the stream ends or the lease is over: a refresh at the start
/// (after subscribing, so no change falls between the two), one after each burst of events, and,
/// while the server cannot be read, another try after each wait. The wait ends early on an event,
/// the end of the lease or the end of the stream.
///
/// If the stream ends while the last refresh had failed, returns the wait that refresh set.
fn follow(
    pactl: &Pactl,
    timing: &Timing,
    lease: &Lease<'_>,
    publish: &dyn Fn(Result<Snapshot, Failure>),
    backoff: &mut Backoff,
) -> Option<Duration> {
    let mut retry = None::<(Instant, Duration)>;
    let mut retry_at = Some(Instant::now());
    loop {
        match lease.wait(retry_at) {
            Wake::Stop => return None,
            Wake::Ended => return retry.map(|(_, wait)| wait),
            Wake::Retry => {}
            Wake::Events => {
                if !lease.settle(timing.settle, timing.max_settle) {
                    return None;
                }
            }
        }
        let picture = refresh(pactl, timing);
        let worked = picture.is_ok();
        if worked {
            retry = None;
        } else {
            let wait = backoff.next();
            retry = Some((Instant::now() + wait, wait));
        }
        retry_at = Some(retry.map_or_else(|| Instant::now() + timing.recheck, |(at, _)| at));
        publish(picture);
        // Only now: a picture whose publication panics does not make this a good session.
        if worked {
            backoff.reset();
        }
    }
}

/// Reads the event stream line by line, bounding the line length, and signals the events that
/// can change a device or a default.
fn read_events(mut stdout: ChildStdout, control: &Control) {
    let mut line = Vec::new();
    let mut too_long = false;
    let mut chunk = [0u8; 1024];
    loop {
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                for &byte in &chunk[..n] {
                    if byte == b'\n' {
                        if !too_long && is_device_event(&line) {
                            control.event();
                        }
                        line.clear();
                        too_long = false;
                    } else if line.len() < MAX_LINE {
                        line.push(byte);
                    } else {
                        too_long = true;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    control.end();
}

/// `Event '<what>' on <facility> #<index>`, for the facilities that carry devices and defaults.
/// Clients, streams, modules and cards are noise here: a refresh itself creates client events.
fn is_device_event(line: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(line) else {
        return false;
    };
    let Some(rest) = text.strip_prefix("Event '") else {
        return false;
    };
    let Some((_, after)) = rest.split_once("' on ") else {
        return false;
    };
    matches!(
        after.split([' ', '#']).next(),
        Some("sink" | "source" | "server")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Dir, alive, all_gone, emit_event, fake, gone, pids};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn wait_until(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + within;
        while !condition() {
            if Instant::now() >= end {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
        true
    }

    #[test]
    fn the_wait_still_grows_when_the_first_one_is_zero() {
        let timing = Timing::default()
            .retry_wait(Duration::ZERO)
            .retry_max(Duration::from_millis(50));
        let mut backoff = Backoff::new(&timing);
        let waits: Vec<_> = (0..10).map(|_| backoff.next()).collect();
        assert!(waits[0] > Duration::ZERO, "{waits:?}");
        assert!(
            waits.windows(2).all(|w| w[0] <= w[1]),
            "never shorter: {waits:?}"
        );
        assert!(waits[3] > waits[0], "it grows: {waits:?}");
        assert_eq!(
            waits[9],
            Duration::from_millis(50),
            "up to the limit: {waits:?}"
        );
    }

    #[test]
    fn the_wait_doubles_to_the_limit_and_starts_over_on_reset() {
        let timing = Timing::default()
            .retry_wait(Duration::from_millis(30))
            .retry_max(Duration::from_millis(200));
        let mut backoff = Backoff::new(&timing);
        let ms = |d: Duration| d.as_millis();
        let waits: Vec<_> = (0..6).map(|_| ms(backoff.next())).collect();
        assert_eq!(waits, [30, 60, 120, 200, 200, 200]);
        backoff.reset();
        assert_eq!(ms(backoff.next()), 30);
        // A limit below the first wait cannot make the wait shrink.
        let timing = timing.retry_max(Duration::from_millis(5));
        let mut backoff = Backoff::new(&timing);
        assert_eq!((ms(backoff.next()), ms(backoff.next())), (30, 30));
    }

    /// A monitor whose watchers always fail: every picture it publishes panics. Returns it with
    /// the times of those panics.
    fn always_panicking(dir: &Dir, timing: Timing) -> (Monitor, Arc<Mutex<Vec<Instant>>>) {
        let pactl = Pactl::new(fake(), Some(dir.0.clone().into_os_string()));
        let panics = Arc::new(Mutex::new(Vec::new()));
        let publish: Publish = {
            let panics = Arc::clone(&panics);
            Box::new(move |_, _| {
                panics.lock().unwrap().push(Instant::now());
                panic!("injected panic in a watcher");
            })
        };
        (Monitor::new(pactl, timing, publish), panics)
    }

    /// Stops the monitor and checks that every event child it ever started is gone for good.
    fn stop_and_check_no_child_is_left(monitor: &Monitor, dir: &Dir) {
        monitor.stop();
        assert!(
            wait_until(Duration::from_secs(3), || all_gone(dir, "subscribe-start ")),
            "event children left behind (running or zombie): {:?}",
            pids(dir, "subscribe-start ")
        );
    }

    fn panics_so_far(panics: &Mutex<Vec<Instant>>) -> usize {
        panics.lock().unwrap().len()
    }

    /// The time from each panic to the next one: at least the wait that came between them.
    fn gaps(panics: &Mutex<Vec<Instant>>) -> Vec<Duration> {
        let times = panics.lock().unwrap().clone();
        times.windows(2).map(|pair| pair[1] - pair[0]).collect()
    }

    #[test]
    fn a_wait_too_long_to_double_stays_as_long_instead_of_overflowing() {
        let timing = Timing::default().retry_wait(Duration::MAX);
        let mut backoff = Backoff::new(&timing);
        assert_eq!(backoff.next(), Duration::MAX);
        assert_eq!(backoff.next(), Duration::MAX);
    }

    #[test]
    fn an_absurdly_long_restart_wait_lasts_until_the_stop_and_the_thread_ends_cleanly() {
        let dir = Dir::with_devices("panic-huge-wait");
        let timing = Timing::default().retry_wait(Duration::MAX);
        let (monitor, panics) = always_panicking(&dir, timing);
        monitor.start().unwrap();
        assert!(wait_until(Duration::from_secs(5), || panics_so_far(
            &panics
        ) >= 1));
        stop_and_check_no_child_is_left(&monitor, &dir);
        // A thread that died on the wait would still be marked as running, and so never replaced.
        assert!(
            wait_until(Duration::from_secs(3), || !monitor
                .shared
                .control
                .lock()
                .running),
            "the monitor thread is gone but believes it is running"
        );
    }

    #[test]
    fn a_watcher_that_keeps_panicking_is_restarted_only_after_a_wait() {
        let dir = Dir::with_devices("panic-rhythm");
        let wait = Duration::from_millis(200);
        let timing = Timing::default().retry_wait(wait).retry_max(wait);
        let (monitor, panics) = always_panicking(&dir, timing);
        monitor.start().unwrap();
        assert!(
            wait_until(Duration::from_secs(5), || panics_so_far(&panics) >= 4),
            "{} panics: the monitor gave up",
            panics_so_far(&panics)
        );
        stop_and_check_no_child_is_left(&monitor, &dir);
        // Without a wait it would start over hundreds of times a second.
        for gap in gaps(&panics) {
            assert!(gap >= wait, "restarted after {gap:?}, wait is {wait:?}");
        }
    }

    #[test]
    fn the_wait_between_restarts_grows_while_the_panics_keep_coming() {
        let dir = Dir::with_devices("panic-backoff");
        let timing = Timing::default()
            .retry_wait(Duration::from_millis(100))
            .retry_max(Duration::from_millis(400));
        let (monitor, panics) = always_panicking(&dir, timing);
        monitor.start().unwrap();
        assert!(
            wait_until(Duration::from_secs(5), || panics_so_far(&panics) >= 4),
            "{} panics: the monitor gave up",
            panics_so_far(&panics)
        );
        stop_and_check_no_child_is_left(&monitor, &dir);
        let least = [100, 200, 400].map(Duration::from_millis);
        for (gap, least) in gaps(&panics).into_iter().zip(least) {
            assert!(
                gap >= least,
                "restarted after {gap:?}, at least {least:?} expected"
            );
        }
    }

    #[test]
    fn a_session_that_works_starts_the_waits_after_a_panic_over() {
        let dir = Dir::with_devices("panic-reset");
        let pactl = Pactl::new(fake(), Some(dir.0.clone().into_os_string()));
        let timing = Timing::default()
            .settle(Duration::from_millis(10))
            .retry_wait(Duration::from_millis(200))
            .retry_max(Duration::from_millis(800));
        let failing = Arc::new(AtomicBool::new(true));
        let panics = Arc::new(Mutex::new(Vec::new()));
        let pictures = Arc::new(AtomicUsize::new(0));
        let publish: Publish = {
            let (failing, panics, pictures) = (
                Arc::clone(&failing),
                Arc::clone(&panics),
                Arc::clone(&pictures),
            );
            Box::new(move |_, picture| {
                if failing.load(Ordering::SeqCst) {
                    panics.lock().unwrap().push(Instant::now());
                    panic!("injected panic in a watcher");
                }
                if picture.is_ok() {
                    pictures.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        let monitor = Monitor::new(pactl, timing, publish);
        monitor.start().unwrap();
        // Two panics: the waits went 200 ms and 400 ms, and the next one would be 800 ms.
        assert!(wait_until(Duration::from_secs(5), || panics_so_far(
            &panics
        ) >= 2));
        failing.store(false, Ordering::SeqCst);
        assert!(
            wait_until(Duration::from_secs(5), || pictures.load(Ordering::SeqCst)
                >= 1),
            "the monitor never recovered"
        );
        // A change on the server makes the next picture fail again.
        failing.store(true, Ordering::SeqCst);
        emit_event(&dir, "Event 'change' on sink #1");
        assert!(wait_until(Duration::from_secs(5), || panics_so_far(
            &panics
        ) >= 4));
        stop_and_check_no_child_is_left(&monitor, &dir);
        let after_the_good_session = gaps(&panics)[2];
        assert!(
            after_the_good_session < Duration::from_millis(700),
            "waited {after_the_good_session:?}: the wait did not start over"
        );
    }

    #[test]
    fn a_panic_in_a_watcher_does_not_end_the_watching_nor_leak_the_event_child() {
        let dir = Dir::with_devices("panic");
        let pactl = Pactl::new(fake(), Some(dir.0.clone().into_os_string()));
        let timing = Timing::default()
            .retry_wait(Duration::from_millis(20))
            .retry_max(Duration::from_millis(40));
        let calls = Arc::new(AtomicUsize::new(0));
        let pictures = Arc::new(AtomicUsize::new(0));
        let publish: Publish = {
            let (calls, pictures) = (Arc::clone(&calls), Arc::clone(&pictures));
            Box::new(move |_, picture| {
                // The first picture makes whoever receives it fail.
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("injected panic in a watcher");
                }
                if picture.is_ok() {
                    pictures.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        let monitor = Monitor::new(pactl, timing, publish);
        monitor.start().unwrap();
        assert!(
            wait_until(Duration::from_secs(3), || pictures.load(Ordering::SeqCst)
                >= 1),
            "no picture after the panic: the monitor gave up"
        );
        // The session that panicked had its event child ended and waited for (not a zombie); the
        // new session has its own.
        let panicked_session = pids(&dir, "subscribe-start ")[0];
        assert!(
            gone(panicked_session),
            "the first event child was left behind"
        );
        assert!(
            wait_until(Duration::from_secs(2), || alive(&dir, "subscribe-start ")
                == 1),
            "{} event children running",
            alive(&dir, "subscribe-start ")
        );
        monitor.stop();
        assert!(wait_until(Duration::from_secs(3), || alive(
            &dir,
            "subscribe-start "
        ) == 0));
    }

    #[test]
    fn stopping_ends_the_event_child_and_waits_for_it() {
        let dir = Dir::with_devices("stop-reap");
        let pactl = Pactl::new(fake(), Some(dir.0.clone().into_os_string()));
        let pictures = Arc::new(AtomicUsize::new(0));
        let publish: Publish = {
            let pictures = Arc::clone(&pictures);
            Box::new(move |_, picture| {
                if picture.is_ok() {
                    pictures.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        let monitor = Monitor::new(pactl, Timing::default(), publish);
        monitor.start().unwrap();
        assert!(
            wait_until(Duration::from_secs(3), || pictures.load(Ordering::SeqCst)
                >= 1
                && alive(&dir, "subscribe-start ") == 1),
            "the monitor never got going"
        );
        monitor.stop();
        assert!(
            wait_until(Duration::from_secs(3), || all_gone(
                &dir,
                "subscribe-start "
            )),
            "the event child outlived the stop (running or zombie): {:?}",
            pids(&dir, "subscribe-start ")
        );
    }
}
