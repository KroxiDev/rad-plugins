//! A scripted host: speaks the wire protocol with hand-written JSON and hand-written framing, so
//! these tests do not depend on the SDK's own encoder.
#![allow(dead_code)]
use rad_plugin_sdk::{Config, Error, Plugin, Version, run};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    ops::RangeInclusive,
    os::unix::net::UnixStream,
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub const GENERATION: u64 = 7;
pub const VERSION: Version = Version {
    major: 0,
    minor: 3,
    patch: 1,
};

pub fn config(api: RangeInclusive<u32>) -> Config {
    Config::new("org.example.probe", VERSION, api)
}

pub struct Host {
    pub stream: UnixStream,
    pub plugin: Option<JoinHandle<Result<(), Error>>>,
}

impl Host {
    /// Runs `plugin` on its own thread over a socket pair; the returned `Host` is the other end.
    pub fn start<P: Plugin>(config: Config, plugin: P) -> Host {
        let (host_end, plugin_end) = UnixStream::pair().unwrap();
        let input = plugin_end.try_clone().unwrap();
        let handle = std::thread::spawn(move || run(config, plugin, input, plugin_end));
        host_end
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Host {
            stream: host_end,
            plugin: Some(handle),
        }
    }

    pub fn send_raw(&mut self, payload: &[u8]) {
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(payload);
        self.stream.write_all(&frame).unwrap();
    }

    pub fn send(&mut self, request: u64, message: Value) {
        let envelope = json!({
            "protocol": 1,
            "correlation": {"generation": GENERATION, "request": request},
            "message": message,
        });
        self.send_raw(&serde_json::to_vec(&envelope).unwrap());
    }

    pub fn hello(&mut self, host_api: u32) {
        self.send(
            1,
            json!({"kind":"hello","host_api":host_api,"target":"x86_64-unknown-linux-gnu",
                   "plugin_id":"org.example.probe","plugin_version":{"major":0,"minor":3,"patch":1}}),
        );
    }

    /// Hello with an API inside the plugin's range, then consumes the `ready` answer.
    pub fn handshake(&mut self, host_api: u32) {
        self.hello(host_api);
        let ready = self.recv();
        assert_eq!(
            ready["message"]["kind"], "ready",
            "handshake answer: {ready}"
        );
    }

    /// The next frame, or `None` if nothing arrives within `wait`. Panics on end of stream.
    pub fn recv_within(&mut self, wait: Duration) -> Option<Value> {
        self.stream.set_read_timeout(Some(wait)).unwrap();
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
                    assert_eq!(got, 0, "timed out inside a frame header");
                    return None;
                }
                Err(e) => panic!("read failed: {e}"),
            }
        }
        let len = u32::from_be_bytes(header) as usize;
        assert!(
            (1..=65536).contains(&len),
            "plugin wrote a frame of {len} bytes"
        );
        let mut payload = vec![0u8; len];
        self.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        self.stream.read_exact(&mut payload).unwrap();
        Some(serde_json::from_slice(&payload).expect("plugin frames are JSON"))
    }

    pub fn recv(&mut self) -> Value {
        self.recv_within(Duration::from_secs(5))
            .expect("no frame from the plugin within 5 s")
    }

    /// Asserts the plugin wrote nothing more and closed its end.
    pub fn expect_closed(&mut self) {
        self.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut byte = [0u8; 1];
        match self.stream.read(&mut byte) {
            Ok(0) => {}
            Ok(_) => panic!("plugin wrote unexpected data"),
            Err(e) => panic!("expected end of stream, got {e}"),
        }
    }

    /// Closes the host's end of the pipes and returns what `run` returned.
    pub fn finish(mut self) -> Result<(), Error> {
        self.stream.shutdown(std::net::Shutdown::Write).ok();
        self.plugin.take().unwrap().join().expect("run panicked")
    }

    /// Waits for `run` to return by itself (a protocol failure) without closing the pipe first.
    pub fn outcome(mut self) -> Result<(), Error> {
        let started = Instant::now();
        let handle = self.plugin.take().unwrap();
        while !handle.is_finished() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "run did not stop"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        handle.join().expect("run panicked")
    }
}

pub fn correlation(frame: &Value) -> (u64, u64) {
    (
        frame["correlation"]["generation"].as_u64().unwrap(),
        frame["correlation"]["request"].as_u64().unwrap(),
    )
}

pub fn parameters(values: Value) -> Value {
    json!({"schema_version": 1, "values": values})
}
