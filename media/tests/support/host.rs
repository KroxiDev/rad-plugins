//! A scripted host: speaks the plugin protocol over a socket pair to a plugin run in-process.
use rad_plugin_media::{MediaPlugin, Timing, config};
use rad_plugin_sdk::Error;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub const GENERATION: u64 = 11;

/// Timings short enough for tests: the production values only matter against a real player.
pub fn quick() -> Timing {
    Timing {
        action_deadline: Duration::from_millis(900),
        options_deadline: Duration::from_millis(900),
        label_deadline: Duration::from_millis(250),
        refresh_deadline: Duration::from_millis(600),
        settle: Duration::from_millis(15),
        max_settle: Duration::from_millis(60),
        // Long: the tests that look at the quiet of an idle plugin must not be broken by it.
        recheck: Duration::from_secs(60),
        linger: Duration::from_millis(100),
        retry_wait: Duration::from_millis(60),
    }
}

/// What a `visual` frame carried.
#[derive(Debug, Clone, PartialEq)]
pub struct Visual {
    pub availability: String,
    pub text: Option<String>,
    pub color: Option<[u8; 4]>,
    pub jpeg_hex: Option<String>,
}

impl Visual {
    fn from_frame(frame: &Value) -> Self {
        let state = &frame["message"]["state"];
        let content = &state["content"];
        Self {
            availability: state["availability"].as_str().unwrap().to_owned(),
            text: content["text"].as_str().map(str::to_owned),
            color: content["color"]
                .as_array()
                .map(|c| [0, 1, 2, 3].map(|i| c[i].as_u64().unwrap() as u8)),
            jpeg_hex: content["jpeg_hex"].as_str().map(str::to_owned),
        }
    }

    pub fn is(&self, availability: &str, text: Option<&str>) -> bool {
        self.availability == availability && self.text.as_deref() == text
    }

    /// The image bytes, if the visual carries a cover.
    pub fn jpeg(&self) -> Option<Vec<u8>> {
        let hex = self.jpeg_hex.as_ref()?;
        Some(
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect(),
        )
    }
}

pub struct Host {
    stream: UnixStream,
    inbox: Vec<Value>,
    next_request: u64,
    plugin: Option<JoinHandle<Result<(), Error>>>,
}

impl Host {
    pub fn start(bus_address: Option<&str>, timing: Timing) -> Host {
        let plugin = MediaPlugin::new(bus_address.map(str::to_owned), timing);
        let (host_end, plugin_end) = UnixStream::pair().unwrap();
        let input = plugin_end.try_clone().unwrap();
        let handle =
            std::thread::spawn(move || rad_plugin_sdk::run(config(), plugin, input, plugin_end));
        let mut host = Host {
            stream: host_end,
            inbox: Vec::new(),
            next_request: 2,
            plugin: Some(handle),
        };
        host.send(
            1,
            json!({"kind":"hello","host_api":2,"target":"x86_64-unknown-linux-gnu",
                   "plugin_id":"org.rad.media","plugin_version":{"major":0,"minor":1,"patch":1}}),
        );
        let ready = host
            .take(|f| f["message"]["kind"] == "ready", Duration::from_secs(5))
            .expect("ready");
        assert_eq!(ready["message"]["plugin_id"], "org.rad.media");
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

    /// The `(value, label)` pairs of an available list; panics if it is unavailable.
    pub fn choices(&mut self, scope: &str, definition: &str) -> Vec<(String, String)> {
        let list = self.options(scope, definition, "player");
        assert_eq!(list["availability"], "available", "{list}");
        list["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                (
                    o["value"].as_str().unwrap().to_owned(),
                    o["label"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    pub fn ask_invoke(&mut self, operation: &str, player: &str) -> u64 {
        self.request(json!({"kind":"invoke","action":"control","parameters":
            {"schema_version":1,"values":{"operation":operation,"player":player}}}))
    }

    pub fn ask_invoke_values(&mut self, action: &str, values: Value) -> u64 {
        self.request(json!({"kind":"invoke","action":action,"parameters":
            {"schema_version":1,"values":values}}))
    }

    pub fn outcome_of(&mut self, request: u64, within: Duration) -> Option<Value> {
        self.answer(request, within).map(|f| {
            assert_eq!(f["message"]["kind"], "result");
            f["message"]["outcome"].clone()
        })
    }

    /// Runs the `control` action and returns its outcome JSON.
    pub fn invoke(&mut self, operation: &str, player: &str) -> Value {
        let request = self.ask_invoke(operation, player);
        self.outcome_of(request, Duration::from_secs(5))
            .expect("result")
    }

    pub fn subscribe(&mut self, player: &str) -> u64 {
        self.request(json!({"kind":"subscribe","source":"now_playing",
            "parameters":{"schema_version":1,"values":{"player":player}}}))
    }

    pub fn unsubscribe(&mut self, subscription: u64) {
        self.request(json!({"kind":"unsubscribe",
            "subscription":{"generation":GENERATION,"request":subscription}}));
    }

    /// The next visual of `subscription`.
    pub fn next_visual(&mut self, subscription: u64, within: Duration) -> Option<Visual> {
        let frame = self.take(
            |f| f["correlation"]["request"] == subscription && f["message"]["kind"] == "visual",
            within,
        )?;
        Some(Visual::from_frame(&frame))
    }

    /// Waits until a visual of `subscription` satisfies `wanted`; returns how long that took, or
    /// every visual seen meanwhile.
    pub fn wait_visual(
        &mut self,
        subscription: u64,
        wanted: impl Fn(&Visual) -> bool,
        within: Duration,
    ) -> Result<(Duration, Visual), Vec<Visual>> {
        let started = Instant::now();
        let mut seen = Vec::new();
        while let Some(left) = within.checked_sub(started.elapsed()) {
            match self.next_visual(subscription, left) {
                Some(visual) if wanted(&visual) => return Ok((started.elapsed(), visual)),
                Some(other) => seen.push(other),
                None => break,
            }
        }
        Err(seen)
    }

    /// `wait_visual` for a state described by its availability and text.
    pub fn wait_state(
        &mut self,
        subscription: u64,
        availability: &str,
        text: Option<&str>,
        within: Duration,
    ) -> Result<Duration, Vec<Visual>> {
        self.wait_visual(subscription, |v| v.is(availability, text), within)
            .map(|(took, _)| took)
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
