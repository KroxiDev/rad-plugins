//! Shared test scaffolding: a fake sound server, and a scripted host that speaks the plugin
//! protocol with hand-written JSON and framing.
#![allow(dead_code)]
pub mod fake;
pub mod fake_state;

#[allow(unused_imports)]
pub use fake::{FakeServer, eventually};
use rad_plugin_audio::{AudioPlugin, Timing};
use rad_plugin_sdk::Error;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub const GENERATION: u64 = 11;

/// Timings short enough for tests: the production values only matter against a real server.
pub fn quick() -> Timing {
    Timing::default()
        .action_deadline(Duration::from_millis(600))
        .options_deadline(Duration::from_millis(500))
        .refresh_deadline(Duration::from_millis(600))
        .settle(Duration::from_millis(15))
        .max_settle(Duration::from_millis(80))
        .retry_wait(Duration::from_millis(60))
        .retry_max(Duration::from_millis(240))
}

pub struct Host {
    stream: UnixStream,
    inbox: Vec<Value>,
    next_request: u64,
    plugin: Option<JoinHandle<Result<(), Error>>>,
}

impl Host {
    pub fn start(server: &FakeServer, timing: Timing) -> Host {
        Self::with_plugin(AudioPlugin::new(Some(server.pactl()), timing))
    }

    pub fn with_plugin(plugin: AudioPlugin) -> Host {
        let (host_end, plugin_end) = UnixStream::pair().unwrap();
        let input = plugin_end.try_clone().unwrap();
        let handle = std::thread::spawn(move || {
            rad_plugin_sdk::run(rad_plugin_audio::config(), plugin, input, plugin_end)
        });
        let mut host = Host {
            stream: host_end,
            inbox: Vec::new(),
            next_request: 2,
            plugin: Some(handle),
        };
        host.send(
            1,
            json!({"kind":"hello","capabilities":["dynamic_options","visual_freshness"],"host_api":1,"target":"x86_64-unknown-linux-gnu",
                   "plugin_id":"org.rad.audio","plugin_version":{"major":0,"minor":2,"patch":0}}),
        );
        let ready = host
            .take(|f| f["message"]["kind"] == "ready", Duration::from_secs(5))
            .expect("ready");
        assert_eq!(ready["message"]["plugin_id"], "org.rad.audio");
        host
    }

    pub fn send(&mut self, request: u64, message: Value) {
        let payload = serde_json::to_vec(&json!({
            "protocol": 1,
            "correlation": {"generation": GENERATION, "request": request},
            "message": message,
        }))
        .unwrap();
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&payload);
        self.stream.write_all(&frame).unwrap();
    }

    fn request(&mut self, message: Value) -> u64 {
        let request = self.next_request;
        self.next_request += 1;
        self.send(request, message);
        request
    }

    fn read_frame(&mut self, wait: Duration) -> Option<Value> {
        self.stream
            .set_read_timeout(Some(wait.max(Duration::from_millis(1))))
            .unwrap();
        let mut header = [0u8; 4];
        let mut got = 0;
        while got < 4 {
            match self.stream.read(&mut header[got..]) {
                Ok(0) => panic!("plugin closed the pipe"),
                Ok(n) => got += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    assert_eq!(got, 0, "timed out inside a frame");
                    return None;
                }
                Err(e) => panic!("read failed: {e}"),
            }
        }
        let mut payload = vec![0u8; u32::from_be_bytes(header) as usize];
        self.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        self.stream.read_exact(&mut payload).unwrap();
        Some(serde_json::from_slice(&payload).unwrap())
    }

    /// The first frame, buffered or newly read, that satisfies `wanted`. Others stay buffered.
    pub fn take(&mut self, wanted: impl Fn(&Value) -> bool, within: Duration) -> Option<Value> {
        if let Some(at) = self.inbox.iter().position(&wanted) {
            return Some(self.inbox.remove(at));
        }
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            let frame = self.read_frame(left)?;
            if wanted(&frame) {
                return Some(frame);
            }
            self.inbox.push(frame);
        }
    }

    fn answer(&mut self, request: u64, within: Duration) -> Option<Value> {
        self.take(|f| f["correlation"]["request"] == request, within)
    }

    pub fn ask_options(&mut self, scope: &str, definition: &str, field: &str) -> u64 {
        self.request(
            json!({"kind":"options","scope":scope,"definition":definition,"field":field,
                            "context":{"schema_version":1,"values":{}}}),
        )
    }

    /// The `list` of the choices answer.
    pub fn options(&mut self, scope: &str, definition: &str, field: &str) -> Value {
        let request = self.ask_options(scope, definition, field);
        let frame = self
            .answer(request, Duration::from_secs(5))
            .expect("choices answer");
        assert_eq!(frame["message"]["kind"], "choices");
        frame["message"]["list"].clone()
    }

    pub fn ask_invoke(&mut self, action: &str, operation: &str, device: &str) -> u64 {
        self.request(json!({"kind":"invoke","action":action,"parameters":
            {"schema_version":1,"values":{"operation":operation,"device":device}}}))
    }

    pub fn outcome_of(&mut self, request: u64, within: Duration) -> Option<Value> {
        self.answer(request, within).map(|f| {
            assert_eq!(f["message"]["kind"], "result");
            f["message"]["outcome"].clone()
        })
    }

    /// Runs an action and returns its outcome JSON.
    pub fn invoke(&mut self, action: &str, operation: &str, device: &str) -> Value {
        let request = self.ask_invoke(action, operation, device);
        self.outcome_of(request, Duration::from_secs(5))
            .expect("result")
    }

    pub fn subscribe(&mut self, source: &str, device: &str) -> u64 {
        self.request(json!({"kind":"subscribe","source":source,
            "parameters":{"schema_version":1,"values":{"device":device}}}))
    }

    pub fn unsubscribe(&mut self, subscription: u64) {
        self.request(json!({"kind":"unsubscribe",
            "subscription":{"generation":GENERATION,"request":subscription}}));
    }

    /// The next visual state of `subscription`, as `(availability, text)`.
    pub fn next_visual(
        &mut self,
        subscription: u64,
        within: Duration,
    ) -> Option<(String, Option<String>)> {
        let frame = self.take(
            |f| f["correlation"]["request"] == subscription && f["message"]["kind"] == "visual",
            within,
        )?;
        let state = &frame["message"]["state"];
        Some((
            state["availability"].as_str().unwrap().to_owned(),
            state["content"]["text"].as_str().map(str::to_owned),
        ))
    }

    /// The colour of the next visual of `subscription`, as RGBA, with its availability.
    pub fn next_visual_colour(
        &mut self,
        subscription: u64,
        within: Duration,
    ) -> Option<(String, Option<[u8; 4]>)> {
        let frame = self.take(
            |f| f["correlation"]["request"] == subscription && f["message"]["kind"] == "visual",
            within,
        )?;
        let state = &frame["message"]["state"];
        let colour = state["content"]["color"]
            .as_array()
            .map(|c| [0, 1, 2, 3].map(|i| c[i].as_u64().unwrap() as u8));
        Some((state["availability"].as_str().unwrap().to_owned(), colour))
    }

    /// Waits until a visual of `subscription` shows `(availability, text)`; the full sequence it
    /// went through is returned on failure.
    pub fn wait_visual(
        &mut self,
        subscription: u64,
        availability: &str,
        text: Option<&str>,
        within: Duration,
    ) -> Result<Duration, Vec<(String, Option<String>)>> {
        let started = Instant::now();
        let mut seen = Vec::new();
        while let Some(left) = within.checked_sub(started.elapsed()) {
            match self.next_visual(subscription, left) {
                Some((a, t)) if a == availability && t.as_deref() == text => {
                    return Ok(started.elapsed());
                }
                Some(other) => seen.push(other),
                None => break,
            }
        }
        Err(seen)
    }

    /// Drops everything buffered or already sent, so the next read sees only what happens next.
    pub fn discard_pending(&mut self) {
        self.inbox.clear();
        while self.read_frame(Duration::from_millis(30)).is_some() {}
    }

    pub fn finish(mut self) -> Result<(), Error> {
        self.stream.shutdown(std::net::Shutdown::Write).ok();
        self.plugin.take().unwrap().join().expect("run panicked")
    }
}
