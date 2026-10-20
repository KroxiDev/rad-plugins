use rad_plugin_sdk::*;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    time::Duration,
};
struct Probe(Arc<Mutex<Option<Subscription>>>);
impl Plugin for Probe {
    fn invoke(&self, _: Invoke, r: Reply<Outcome>) {
        r.send(Outcome::Success);
    }
    fn options(&self, _: OptionsQuery, r: Reply<ChoiceList>) {
        r.send(ChoiceList::Unavailable);
    }
    fn subscribe(&self, s: Subscription) {
        s.update(state());
        *self.0.lock().unwrap() = Some(s);
    }
    fn unsubscribe(&self, _: SubscriptionId) {}
}
fn state() -> VisualState {
    VisualState::Current {
        content: VisualContent {
            text: Some("Paused".into()),
            png_hex: Some("ab".repeat(8192)),
            ..Default::default()
        },
    }
}
fn send(stream: &mut UnixStream, request: u64, message: serde_json::Value) {
    let payload = serde_json::to_vec(&serde_json::json!({"protocol":1,"correlation":{"generation":1,"request":request},"message":message})).unwrap();
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&payload).unwrap();
}
fn receive(stream: &mut UnixStream) -> serde_json::Value {
    let mut len = [0; 4];
    stream.read_exact(&mut len).unwrap();
    let mut bytes = vec![0; u32::from_be_bytes(len) as usize];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
#[test]
fn observed_unchanged_source_renews_without_image_and_blocked_source_sends_nothing() {
    let (mut host, io) = UnixStream::pair().unwrap();
    host.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let input = io.try_clone().unwrap();
    let subscription = Arc::new(Mutex::new(None));
    let owned = subscription.clone();
    let plugin = std::thread::spawn(move || {
        run(
            Config::new(
                "org.example.probe",
                Version {
                    major: 1,
                    minor: 0,
                    patch: 0,
                },
                1..=1,
            )
            .capabilities(vec!["visual_freshness".into()]),
            Probe(owned),
            input,
            io,
        )
    });
    send(
        &mut host,
        1,
        serde_json::json!({"kind":"hello","host_api":1,"capabilities":["visual_freshness"],"target":"x86_64-unknown-linux-gnu","plugin_id":"org.example.probe","plugin_version":{"major":1,"minor":0,"patch":0}}),
    );
    assert_eq!(receive(&mut host)["message"]["kind"], "ready");
    send(
        &mut host,
        2,
        serde_json::json!({"kind":"subscribe","source":"status","parameters":{"schema_version":1,"values":{}}}),
    );
    let first = receive(&mut host);
    assert_eq!(first["message"]["valid_for_ms"], 25000);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while subscription.lock().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    subscription
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .update(state());
    let renewal = receive(&mut host);
    assert_eq!(
        renewal["message"],
        serde_json::json!({"kind":"visual_renew","valid_for_ms":25000})
    );
    host.set_read_timeout(Some(Duration::from_millis(650)))
        .unwrap();
    let mut byte = [0];
    let error = host.read(&mut byte).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    host.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(plugin.join().unwrap().is_ok());
    assert!(!subscription.lock().unwrap().as_ref().unwrap().is_active());
}
