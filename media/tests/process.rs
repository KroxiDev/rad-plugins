//! The plugin as a process, speaking the protocol over real pipes with the environment the host
//! gives it: a runtime directory and the address of the session bus, nothing else.
//!
//! The bus is a private one with fake players on it; the session bus of the machine is never
//! reached.
mod support;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    os::fd::AsRawFd,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use support::*;

const BRAVE: &str = "org.mpris.MediaPlayer2.brave.instance4242";

struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
}

impl Process {
    fn spawn(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn plugin");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, request: u64, message: Value) {
        let payload = serde_json::to_vec(&json!({
            "protocol": 1,
            "correlation": {"generation": 3, "request": request},
            "message": message,
        }))
        .unwrap();
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&payload);
        self.stdin.as_mut().unwrap().write_all(&frame).unwrap();
        self.stdin.as_mut().unwrap().flush().unwrap();
    }

    fn hello(&mut self, api: u32) {
        self.send(
            1,
            json!({"kind":"hello","host_api":api,"target":"x86_64-unknown-linux-gnu",
                   "plugin_id":"org.rad.media","plugin_version":{"major":0,"minor":1,"patch":1}}),
        );
    }

    /// The next frame, or `None` at the end of the stream.
    fn recv(&mut self) -> Option<Value> {
        let mut header = [0u8; 4];
        self.stdout.read_exact(&mut header).ok()?;
        let mut payload = vec![0u8; u32::from_be_bytes(header) as usize];
        self.stdout.read_exact(&mut payload).ok()?;
        Some(serde_json::from_slice(&payload).unwrap())
    }

    /// The next frame answering `request`, skipping visuals of other subscriptions.
    fn recv_for(&mut self, request: u64) -> Value {
        loop {
            let frame = self.recv().expect("the plugin closed its output");
            if frame["correlation"]["request"] == request {
                return frame;
            }
        }
    }

    /// The next frame answering `request`, or `None` if none arrives within `within`.
    fn recv_for_within(&mut self, request: u64, within: Duration) -> Option<Value> {
        let end = Instant::now() + within;
        loop {
            let left = end.checked_duration_since(Instant::now())?;
            let mut poll = libc::pollfd {
                fd: self.stdout.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `poll` points to one valid `pollfd` for the duration of the call.
            let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis().max(1) as i32) };
            if ready <= 0 {
                continue;
            }
            let frame = self.recv()?;
            if frame["correlation"]["request"] == request {
                return Some(frame);
            }
        }
    }

    fn close_input(&mut self) {
        self.stdin = None;
    }

    fn threads(&self) -> Vec<String> {
        threads_of(self.child.id())
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }

    fn rss_kib(&self) -> u64 {
        fs::read_to_string(format!("/proc/{}/status", self.child.id()))
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .unwrap_or(0)
    }
}

/// The threads of the process `pid`, as (id, name).
fn threads_of(pid: u32) -> Vec<(String, String)> {
    fs::read_dir(format!("/proc/{pid}/task"))
        .map(|tasks| {
            tasks
                .filter_map(|task| {
                    let task = task.ok()?;
                    let name = fs::read_to_string(task.path().join("comm")).ok()?;
                    Some((
                        task.file_name().to_string_lossy().into_owned(),
                        name.trim().to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a census saw of one kind of thread.
#[derive(Debug, Default)]
struct Seen {
    /// The most there were at once.
    at_once: usize,
    /// Every thread of the kind that was ever seen.
    ever: HashSet<String>,
}

/// How long a thread must keep a name to count as one of that kind. A thread starts with the
/// name of the thread that started it, and takes its own a moment later: the helper threads
/// of the bus library, started by a `media-` thread, are not `media-` threads.
const NAMED_FOR: Duration = Duration::from_millis(10);

/// Watches the threads of a process while it works, and remembers what there was of each kind of
/// `media-` thread.
struct Census {
    stop: Arc<AtomicBool>,
    watcher: JoinHandle<HashMap<String, Seen>>,
}

impl Census {
    fn start(pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let watcher = std::thread::spawn(move || {
            let mut seen: HashMap<String, Seen> = HashMap::new();
            // Since when each thread has had the name it has.
            let mut named: HashMap<(String, String), Instant> = HashMap::new();
            while !stopping.load(Ordering::Acquire) {
                let now = Instant::now();
                let mut counted: HashMap<String, usize> = HashMap::new();
                let mut present = HashSet::new();
                for (id, name) in threads_of(pid) {
                    if !name.starts_with("media-") {
                        continue;
                    }
                    let key = (id.clone(), name.clone());
                    let since = *named.entry(key.clone()).or_insert(now);
                    present.insert(key);
                    if now.duration_since(since) >= NAMED_FOR {
                        *counted.entry(name.clone()).or_default() += 1;
                        seen.entry(name).or_default().ever.insert(id);
                    }
                }
                named.retain(|key, _| present.contains(key));
                for (name, count) in counted {
                    let kind = seen.entry(name).or_default();
                    kind.at_once = kind.at_once.max(count);
                }
                std::thread::sleep(Duration::from_micros(200));
            }
            seen
        });
        Self { stop, watcher }
    }

    fn finish(self) -> Sightings {
        self.stop.store(true, Ordering::Release);
        Sightings(self.watcher.join().unwrap())
    }
}

/// What a [`Census`] saw.
#[derive(Debug)]
struct Sightings(HashMap<String, Seen>);

impl Sightings {
    fn at_once(&self, kind: &str) -> usize {
        self.0.get(kind).map_or(0, |seen| seen.at_once)
    }

    fn ever(&self, kind: &str) -> usize {
        self.0.get(kind).map_or(0, |seen| seen.ever.len())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What RAD gives a plugin: no PATH, nothing but a runtime directory and the bus address.
fn real_plugin(bus: &PrivateBus, runtime: &std::path::Path) -> Process {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rad-plugin-media"));
    command
        .env_clear()
        .env("XDG_RUNTIME_DIR", runtime)
        .env("DBUS_SESSION_BUS_ADDRESS", bus.address());
    Process::spawn(command)
}

#[test]
fn the_real_binary_handshakes_lists_options_acts_and_shows_a_visual() {
    let bus = PrivateBus::start();
    let runtime = scratch_dir("runtime", false);
    let mut player = FakePlayer::start(&bus, BRAVE);
    player
        .command("identity Brave")
        .command("status Playing")
        .sync();
    let mut plugin = real_plugin(&bus, &runtime);

    plugin.hello(2);
    let ready = plugin.recv().expect("ready");
    assert_eq!(ready["correlation"], json!({"generation":3,"request":1}));
    assert_eq!(
        ready["message"],
        json!({"kind":"ready","plugin_id":"org.rad.media","plugin_version":{"major":0,"minor":1,"patch":1}})
    );

    plugin.send(
        2,
        json!({"kind":"options","scope":"action","definition":"control","field":"player",
               "context":{"schema_version":1,"values":{}}}),
    );
    assert_eq!(
        plugin.recv_for(2)["message"]["list"],
        json!({"availability":"available","options":[
            {"value":"brave","label":"Brave"},
            {"value":"strawberry","label":"Strawberry (not running)"},
        ]})
    );

    plugin.send(
        3,
        json!({"kind":"invoke","action":"control","parameters":{"schema_version":1,
               "values":{"operation":"next","player":"brave"}}}),
    );
    assert_eq!(
        plugin.recv_for(3)["message"]["outcome"],
        json!({"status":"success"})
    );
    assert_eq!(player.calls(), ["Next"]);

    plugin.send(
        4,
        json!({"kind":"invoke","action":"control","parameters":{"schema_version":1,
               "values":{"operation":"next","player":"strawberry"}}}),
    );
    assert_eq!(
        plugin.recv_for(4)["message"]["outcome"],
        json!({"status":"failure","code":"unavailable"}),
        "a player that is not running is not replaced by the one that is"
    );
    assert_eq!(player.calls(), ["Next"]);

    plugin.send(
        5,
        json!({"kind":"subscribe","source":"now_playing","parameters":{"schema_version":1,"values":{"player":"brave"}}}),
    );
    let visual = plugin.recv_for(5);
    assert_eq!(visual["message"]["kind"], "visual");
    assert_eq!(visual["message"]["state"]["availability"], "current");
    assert_eq!(visual["message"]["state"]["content"]["text"], "Playing");

    // The player changes behind the plugin's back, and the next frame says so.
    player.command("status Paused");
    let started = Instant::now();
    loop {
        let frame = plugin.recv_for(5);
        if frame["message"]["state"]["content"]["text"] == "Paused" {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(2), "never paused");
    }

    // The host leaving ends the plugin cleanly.
    plugin.close_input();
    let status = plugin.child.wait().unwrap();
    assert!(status.success(), "{status:?}");
}

#[test]
fn without_displays_the_plugin_runs_no_listening_thread_and_stays_small() {
    let bus = PrivateBus::start();
    let runtime = scratch_dir("runtime", false);
    let _player = FakePlayer::start(&bus, BRAVE);
    let mut plugin = real_plugin(&bus, &runtime);
    plugin.hello(2);
    plugin.recv().expect("ready");
    let names = |plugin: &Process| {
        plugin
            .threads()
            .into_iter()
            .filter(|n| n.starts_with("media-"))
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&plugin), Vec::<String>::new());

    plugin.send(
        2,
        json!({"kind":"subscribe","source":"now_playing","parameters":{"schema_version":1,"values":{"player":"brave"}}}),
    );
    plugin.recv_for(2);
    let mut working = names(&plugin);
    working.sort();
    assert_eq!(working, ["media-monitor", "media-reader"]);
    // A plugin at rest stays well under 20 MiB, with the bus library loaded.
    assert!(plugin.rss_kib() < 20 * 1024, "{} KiB", plugin.rss_kib());

    plugin.send(
        3,
        json!({"kind":"unsubscribe","subscription":{"generation":3,"request":2}}),
    );
    let started = Instant::now();
    while !names(&plugin).is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            names(&plugin)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_real_binary_refuses_a_host_api_it_does_not_support() {
    let bus = PrivateBus::start();
    let runtime = scratch_dir("runtime", false);
    let mut plugin = real_plugin(&bus, &runtime);
    plugin.hello(1);
    assert!(
        plugin.recv().is_none(),
        "no ready for an unsupported host API"
    );
    let status = plugin.child.wait().unwrap();
    assert!(!status.success(), "{status:?}");
}

#[test]
fn the_real_binary_without_a_bus_address_reports_everything_unavailable() {
    let runtime = scratch_dir("runtime", false);
    let mut command = Command::new(env!("CARGO_BIN_EXE_rad-plugin-media"));
    command
        .env_clear()
        .env("XDG_RUNTIME_DIR", runtime.as_os_str());
    let mut plugin = Process::spawn(command);
    plugin.hello(2);
    plugin.recv().expect("ready");
    plugin.send(
        2,
        json!({"kind":"options","scope":"visual","definition":"now_playing","field":"player",
               "context":{"schema_version":1,"values":{}}}),
    );
    assert_eq!(
        plugin.recv_for(2)["message"]["list"],
        json!({"availability":"unavailable"})
    );
    plugin.send(
        3,
        json!({"kind":"invoke","action":"control","parameters":{"schema_version":1,
               "values":{"operation":"play","player":"brave"}}}),
    );
    assert_eq!(
        plugin.recv_for(3)["message"]["outcome"],
        json!({"status":"failure","code":"unavailable"})
    );
}

#[test]
fn the_real_binary_finds_the_standard_session_bus_when_the_variable_is_missing() {
    // The session bus of a user is the socket `bus` in the runtime directory; the host does not
    // have to pass its address on.
    let bus = PrivateBus::start_at_fixed_address();
    let mut player = FakePlayer::start(&bus, BRAVE);
    player.command("identity Brave").sync();
    let mut command = Command::new(env!("CARGO_BIN_EXE_rad-plugin-media"));
    command
        .env_clear()
        .env("XDG_RUNTIME_DIR", bus.runtime_dir());
    let mut plugin = Process::spawn(command);
    plugin.hello(2);
    plugin.recv().expect("ready");
    plugin.send(
        2,
        json!({"kind":"options","scope":"action","definition":"control","field":"player",
               "context":{"schema_version":1,"values":{}}}),
    );
    assert_eq!(
        plugin.recv_for(2)["message"]["list"],
        json!({"availability":"available","options":[
            {"value":"brave","label":"Brave"},
            {"value":"strawberry","label":"Strawberry (not running)"},
        ]})
    );
}

/// Subscribes to `player` as request `request`; the answer is a visual, whenever there is one.
fn subscribe(plugin: &mut Process, request: u64, player: &str) {
    plugin.send(
        request,
        json!({"kind":"subscribe","source":"now_playing",
               "parameters":{"schema_version":1,"values":{"player":player}}}),
    );
}

fn unsubscribe(plugin: &mut Process, request: u64, subscription: u64) {
    plugin.send(
        request,
        json!({"kind":"unsubscribe","subscription":{"generation":3,"request":subscription}}),
    );
}

/// Waits until the plugin has no `media-` thread left.
fn wait_until_idle(plugin: &Process, within: Duration) {
    let started = Instant::now();
    loop {
        let left: Vec<_> = plugin
            .threads()
            .into_iter()
            .filter(|n| n.starts_with("media-"))
            .collect();
        if left.is_empty() {
            return;
        }
        assert!(started.elapsed() < within, "still running: {left:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_burst_of_subscribing_and_unsubscribing_keeps_the_threads_few() {
    // The player is frozen, so the reading of it that starts with the first button lasts for
    // longer than the whole burst.
    let bus = PrivateBus::start();
    let runtime = scratch_dir("runtime", false);
    let frozen = FakePlayer::start(&bus, BRAVE);
    frozen.freeze();
    let mut plugin = real_plugin(&bus, &runtime);
    plugin.hello(2);
    plugin.recv().expect("ready");

    let census = Census::start(plugin.child.id());
    for pair in 0..300 {
        let request = 10 + 2 * pair;
        subscribe(&mut plugin, request, "brave");
        unsubscribe(&mut plugin, request + 1, request);
    }
    let seen = census.finish();
    // The first reader and the first monitor served the whole burst: no other was ever started.
    assert!(seen.ever("media-reader") <= 1, "{seen:?}");
    assert!(seen.ever("media-monitor") <= 1, "{seen:?}");
    // And what the burst started ends by itself once the frozen player has timed out.
    wait_until_idle(&plugin, Duration::from_secs(6));
    frozen.thaw();
}

#[test]
fn many_applications_that_hang_are_read_by_a_bounded_number_of_threads() {
    // A hundred applications, all of them one frozen process. The host may have 64 buttons at
    // once; here 64 come and go and 36 more come while the readers of the first ones are still
    // waiting for the frozen process, which is more readers than the host could ever need.
    let bus = PrivateBus::start();
    let runtime = scratch_dir("runtime", false);
    let names: Vec<String> = (0..100)
        .map(|n| format!("org.mpris.MediaPlayer2.app{n:03}"))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let frozen = FakePlayer::start_owning(&bus, &names);
    let mut healthy = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.strawberry");
    healthy.command("status Playing").sync();
    frozen.freeze();
    let mut plugin = real_plugin(&bus, &runtime);
    plugin.hello(2);
    plugin.recv().expect("ready");

    let census = Census::start(plugin.child.id());
    for app in 0..64u64 {
        subscribe(&mut plugin, 10 + app, &format!("app{app:03}"));
    }
    // Every reader is now waiting for the frozen process (it answers after 1.5 s at the most).
    std::thread::sleep(Duration::from_millis(300));
    for app in 0..64u64 {
        unsubscribe(&mut plugin, 100 + app, 10 + app);
    }
    for app in 64..100u64 {
        subscribe(&mut plugin, 10 + app, &format!("app{app:03}"));
    }
    // A healthy application is shown too: its button waits for its turn, it is not refused.
    subscribe(&mut plugin, 500, "strawberry");
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let frame = plugin.recv_for_within(500, left).expect("never shown");
        if frame["message"]["state"]["content"]["text"] == "Playing" {
            break;
        }
    }
    // Whatever readers there are have been there long enough to be counted.
    std::thread::sleep(2 * NAMED_FOR);
    let seen = census.finish();
    assert!(
        (1..=64).contains(&seen.at_once("media-reader")),
        "at most as many readers as the host may have buttons: {seen:?}"
    );
    assert!(seen.at_once("media-monitor") <= 1, "{seen:?}");
    frozen.thaw();
}

#[test]
fn a_button_that_comes_back_within_a_moment_is_served_by_the_same_threads() {
    let bus = PrivateBus::start();
    let runtime = scratch_dir("runtime", false);
    let mut player = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.strawberry");
    player.command("status Playing").sync();
    let mut plugin = real_plugin(&bus, &runtime);
    plugin.hello(2);
    plugin.recv().expect("ready");

    let census = Census::start(plugin.child.id());
    for round in 0..10u64 {
        let request = 10 + 2 * round;
        subscribe(&mut plugin, request, "strawberry");
        // Every button gets the state, also the ones that join a reader that is already there.
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let frame = plugin.recv_for_within(request, left).expect("never shown");
            if frame["message"]["state"]["content"]["text"] == "Playing" {
                break;
            }
        }
        // Long enough to be seen at work, and for a reader that ended with its button to be gone.
        std::thread::sleep(Duration::from_millis(30));
        unsubscribe(&mut plugin, request + 1, request);
        std::thread::sleep(Duration::from_millis(30));
    }
    let seen = census.finish();
    assert_eq!(seen.ever("media-reader"), 1, "{seen:?}");
    assert_eq!(seen.ever("media-monitor"), 1, "{seen:?}");
    // And once nobody comes back they are gone.
    wait_until_idle(&plugin, Duration::from_secs(5));
}

#[test]
fn the_fake_player_refuses_to_run_unless_told_the_bus_is_private() {
    // It registers a player that does not exist: on a bus that is somebody's own session, that
    // would be a false player among the real ones.
    let bus = PrivateBus::start();
    let output = Command::new(example("fake_player"))
        .arg("org.mpris.MediaPlayer2.fake")
        .env_clear()
        .env("DBUS_SESSION_BUS_ADDRESS", bus.address())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{:?}", output.status);
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("ready"),
        "it registered its name anyway"
    );
}

#[test]
fn cover_conversion_is_shared_and_retires_before_starting_again() {
    let bus = PrivateBus::start();
    let runtime = scratch_dir("cover-worker", false);
    let path = runtime.join("cover.png");
    image::RgbaImage::from_pixel(256, 256, image::Rgba([30, 120, 220, 255]))
        .save(&path)
        .unwrap();
    let mut player = FakePlayer::start_owning(&bus, &[BRAVE, "org.mpris.MediaPlayer2.strawberry"]);
    player
        .command(&format!("art file://{}", path.display()))
        .sync();
    let mut plugin = real_plugin(&bus, &runtime);
    plugin.hello(2);
    plugin.recv().expect("ready");
    for round in 0..2 {
        let first = 10 + round * 10;
        for (request, app) in [(first, "brave"), (first + 1, "strawberry")] {
            subscribe(&mut plugin, request, app);
            let frame = plugin
                .recv_for_within(request, Duration::from_secs(5))
                .expect("cover");
            assert_eq!(
                frame["message"]["state"]["content"]["jpeg_hex"],
                rad_plugin_media::cover::jpeg_hex(&path).unwrap(),
                "the shared CPU pipeline preserves the cover result"
            );
        }
        assert_eq!(
            plugin
                .threads()
                .iter()
                .filter(|n| *n == "media-cover")
                .count(),
            1,
            "cover conversion must share one worker, not retain decoder scratch per app"
        );
        unsubscribe(&mut plugin, first + 2, first);
        unsubscribe(&mut plugin, first + 3, first + 1);
        wait_until_idle(&plugin, Duration::from_secs(5));
    }
}

#[test]
fn sigkill_releases_monitor_reader_and_converter_without_sdk_teardown() {
    let bus = PrivateBus::start();
    let runtime = scratch_dir("kill-cover-worker", false);
    let path = runtime.join("cover.png");
    image::RgbaImage::from_pixel(256, 256, image::Rgba([30, 120, 220, 255]))
        .save(&path)
        .unwrap();
    let mut player = FakePlayer::start(&bus, BRAVE);
    player
        .command(&format!("art file://{}", path.display()))
        .sync();
    let mut plugin = real_plugin(&bus, &runtime);
    plugin.hello(2);
    plugin.recv().expect("ready");
    subscribe(&mut plugin, 2, "brave");
    let frame = plugin
        .recv_for_within(2, Duration::from_secs(5))
        .expect("cover");
    assert!(frame["message"]["state"]["content"]["jpeg_hex"].is_string());
    let threads = threads_of(plugin.child.id());
    assert!(threads.iter().any(|(_, name)| name == "media-cover"));
    assert!(threads.iter().any(|(_, name)| name == "media-monitor"));
    assert!(threads.iter().any(|(_, name)| name == "media-reader"));
    // The host's normal stop is SIGKILL, not graceful EOF; no Rust destructor is assumed.
    plugin.child.kill().unwrap();
    let status = plugin.child.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(threads_of(plugin.child.id()).is_empty());
    let end = Instant::now() + Duration::from_secs(3);
    while bus.client_count() != 1 {
        assert!(Instant::now() < end, "killed plugin retained bus resources");
        std::thread::sleep(Duration::from_millis(10));
    }
}
