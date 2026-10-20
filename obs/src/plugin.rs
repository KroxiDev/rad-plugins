//! SDK adapter and one bounded worker. No OBS I/O occurs on the SDK thread.
use crate::{protocol, transport::Transport};
use rad_plugin_sdk::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};
pub fn config() -> Config {
    Config::new(
        "org.rad.obs",
        Version {
            major: 0,
            minor: 2,
            patch: 0,
        },
        1..=1,
    )
    .capabilities(vec![
        "credentials".into(),
        "settings".into(),
        "dynamic_options".into(),
        "response_timeout".into(),
        "visual_freshness".into(),
    ])
    .credential_slots(vec!["password".into()])
}
struct Wait {
    credentials: Option<Credentials>,
    reply: Reply<Outcome>,
    deadline: Instant,
    flow: Option<crate::workflow::Workflow>,
    sent: Option<String>,
    outcome: Option<Outcome>,
    next: Instant,
}
struct Query {
    credentials: Option<Credentials>,
    reply: Reply<ChoiceList>,
    flow: crate::catalog::Catalog,
    sent: Option<String>,
    deadline: Instant,
    result: Option<ChoiceList>,
}
struct Watch {
    sub: Subscription,
    flow: Option<crate::workflow::Workflow>,
    sent: Option<String>,
    deadline: Instant,
    next: Instant,
    last: VisualState,
}
struct Session {
    transport: Option<Transport>,
    phase: u8,
    attempt: Instant,
    retry: Instant,
    waits: Vec<Wait>,
    queries: Vec<Query>,
    watches: Vec<Watch>,
    serial: u64,
    blocked: bool,
    capabilities: Vec<Value>,
    discarded: Option<(String, Instant)>,
    pending_text: Option<String>,
    backoff: Duration,
}
struct Inner {
    sessions: BTreeMap<(u64, SocketAddrV4), Session>,
    stop: bool,
}
struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
    endpoint: SocketAddrV4,
}
impl Shared {
    fn endpoint_for(&self, settings: Option<&Parameters>) -> Result<SocketAddrV4, FailureCode> {
        let port = if let Some(settings) = settings {
            if settings.schema_version != 1 || settings.values.keys().any(|key| key != "port") {
                return Err(FailureCode::InvalidParameters);
            }
            settings
                .values
                .get("port")
                .map_or(Some(u64::from(self.endpoint.port())), Value::as_u64)
                .filter(|port| (1..=65535).contains(port))
                .ok_or(FailureCode::InvalidParameters)? as u16
        } else {
            self.endpoint.port()
        };
        // A test endpoint is never allowed to fall back to personal OBS, even through settings.
        if self.endpoint.port() != 4455 && port == 4455 {
            return Err(FailureCode::InvalidParameters);
        }
        Ok(SocketAddrV4::new(*self.endpoint.ip(), port))
    }
}
pub struct ObsPlugin {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl ObsPlugin {
    pub fn new() -> Self {
        Self::start(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4455)).expect("worker creation")
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(endpoint: SocketAddrV4) -> Result<Self, FailureCode> {
        if endpoint.port() == 4455 {
            return Err(FailureCode::InvalidParameters);
        }
        Self::start(endpoint)
    }
    fn start(endpoint: SocketAddrV4) -> Result<Self, FailureCode> {
        if !endpoint.ip().is_loopback() || endpoint.port() == 0 {
            return Err(FailureCode::InvalidParameters);
        }
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                sessions: BTreeMap::new(),
                stop: false,
            }),
            wake: Condvar::new(),
            endpoint,
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("obs-io".into())
            .spawn(move || run_worker(worker))
            .map_err(|_| FailureCode::Unavailable)?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
}
impl Default for ObsPlugin {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for ObsPlugin {
    fn drop(&mut self) {
        lock(&self.shared.inner).stop = true;
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
fn action_timeout(action: &str, parameters: &Parameters) -> Result<u64, FailureCode> {
    if action != "wait" {
        return Ok(5);
    }
    let seconds = parameters
        .values
        .get("timeout_seconds")
        .map_or(Some(30), Value::as_u64);
    seconds
        .filter(|n| (1..=120).contains(n))
        .ok_or(FailureCode::InvalidParameters)
}
impl Plugin for ObsPlugin {
    fn invoke(&self, call: Invoke, reply: Reply<Outcome>) {
        let Some(credentials) = call.credentials else {
            reply.send(Outcome::Failure {
                code: FailureCode::CredentialsUnavailable,
            });
            return;
        };
        let seconds = match action_timeout(&call.action, &call.parameters) {
            Ok(seconds) => seconds,
            Err(code) => {
                drop(credentials);
                reply.send(Outcome::Failure { code });
                return;
            }
        };
        let flow = if call.action == "wait" {
            None
        } else {
            match crate::workflow::Workflow::action(&call.action, &call.parameters) {
                Ok(f) => Some(f),
                Err(code) => {
                    drop(credentials);
                    reply.send(Outcome::Failure { code });
                    return;
                }
            }
        };
        let endpoint = match self.shared.endpoint_for(call.settings.as_ref()) {
            Ok(endpoint) => endpoint,
            Err(code) => {
                drop(credentials);
                reply.send(Outcome::Failure { code });
                return;
            }
        };
        let revision = (credentials.revision(), endpoint);
        let mut inner = lock(&self.shared.inner);
        if inner
            .sessions
            .values()
            .map(|s| s.waits.len())
            .sum::<usize>()
            >= 16
            || (!inner.sessions.contains_key(&revision) && inner.sessions.len() >= 67)
        {
            drop(inner);
            drop(credentials);
            reply.send(Outcome::Failure {
                code: FailureCode::Busy,
            });
            return;
        }
        let now = Instant::now();
        let session = inner.sessions.entry(revision).or_insert_with(|| Session {
            transport: None,
            phase: 0,
            attempt: now,
            retry: now,
            waits: vec![],
            queries: vec![],
            watches: vec![],
            serial: 0,
            blocked: false,
            capabilities: vec![],
            discarded: None,
            pending_text: None,
            backoff: Duration::from_millis(250),
        });
        if flow.is_some()
            && (session.blocked || (session.transport.is_none() && now < session.retry))
        {
            drop(inner);
            drop(credentials);
            reply.send(Outcome::Failure {
                code: FailureCode::Unavailable,
            });
            return;
        }
        session.waits.push(Wait {
            credentials: Some(credentials),
            reply,
            deadline: now + Duration::from_secs(seconds),
            flow,
            sent: None,
            outcome: None,
            next: now,
        });
        drop(inner);
        self.shared.wake.notify_one();
    }
    fn options(&self, query: OptionsQuery, reply: Reply<ChoiceList>) {
        let Some(credentials) = query.credentials else {
            reply.send(ChoiceList::Unavailable);
            return;
        };
        let Ok(flow) = crate::catalog::Catalog::new(&query.field, &query.context) else {
            drop(credentials);
            reply.send(ChoiceList::Unavailable);
            return;
        };
        let Ok(endpoint) = self.shared.endpoint_for(query.settings.as_ref()) else {
            drop(credentials);
            reply.send(ChoiceList::Unavailable);
            return;
        };
        let revision = (credentials.revision(), endpoint);
        let mut inner = lock(&self.shared.inner);
        if inner
            .sessions
            .values()
            .map(|s| s.queries.len())
            .sum::<usize>()
            >= 2
            || (!inner.sessions.contains_key(&revision) && inner.sessions.len() >= 67)
        {
            drop(inner);
            drop(credentials);
            reply.send(ChoiceList::Unavailable);
            return;
        }
        let now = Instant::now();
        let session = inner.sessions.entry(revision).or_insert_with(|| Session {
            transport: None,
            phase: 0,
            attempt: now,
            retry: now,
            waits: vec![],
            queries: vec![],
            watches: vec![],
            serial: 0,
            blocked: false,
            capabilities: vec![],
            discarded: None,
            pending_text: None,
            backoff: Duration::from_millis(250),
        });
        session.queries.push(Query {
            credentials: Some(credentials),
            reply,
            flow,
            sent: None,
            deadline: now + Duration::from_secs(2),
            result: None,
        });
        drop(inner);
        self.shared.wake.notify_one();
    }
    fn subscribe(&self, sub: Subscription) {
        sub.update(VisualState::Unavailable);
        let Some(credentials) = sub.credentials() else {
            return;
        };
        let Ok(endpoint) = self.shared.endpoint_for(sub.settings()) else {
            return;
        };
        let revision = (credentials.revision(), endpoint);
        if crate::workflow::Workflow::visual(sub.source(), sub.parameters()).is_err() {
            return;
        }
        let mut inner = lock(&self.shared.inner);
        if inner
            .sessions
            .values()
            .map(|s| s.watches.len())
            .sum::<usize>()
            >= 32
            || (!inner.sessions.contains_key(&revision) && inner.sessions.len() >= 67)
        {
            return;
        }
        let now = Instant::now();
        let session = inner.sessions.entry(revision).or_insert_with(|| Session {
            transport: None,
            phase: 0,
            attempt: now,
            retry: now,
            waits: vec![],
            queries: vec![],
            watches: vec![],
            serial: 0,
            blocked: false,
            capabilities: vec![],
            discarded: None,
            pending_text: None,
            backoff: Duration::from_millis(250),
        });
        session.watches.push(Watch {
            sub,
            flow: None,
            sent: None,
            deadline: now,
            next: now,
            last: VisualState::Unavailable,
        });
        drop(inner);
        self.shared.wake.notify_one();
    }
    fn unsubscribe(&self, id: SubscriptionId) {
        let mut inner = lock(&self.shared.inner);
        for session in inner.sessions.values_mut() {
            for w in &mut session.watches {
                if w.sub.id() == id
                    && let Some(sent) = w.sent.take()
                {
                    session.discarded = Some((sent, w.deadline));
                }
            }
            session.watches.retain(|w| w.sub.id() != id);
        }
        inner.sessions.retain(|_, session| session.has_demand());
    }
    fn credentials_released(&self, revision: u64) {
        lock(&self.shared.inner)
            .sessions
            .retain(|(r, _), _| *r != revision);
    }
}
fn run_worker(shared: Arc<Shared>) {
    while worker_turn(&shared, Instant::now()) {
        let inner = lock(&shared.inner);
        if inner.stop {
            continue;
        }
        if inner.sessions.is_empty() {
            // No sockets or deadlines remain: only new demand (or shutdown) can wake us.
            let _guard = shared
                .wake
                .wait_while(inner, |inner| !inner.stop && inner.sessions.is_empty());
        } else {
            let _ = shared.wake.wait_timeout(inner, Duration::from_millis(20));
        }
    }
}
// Same turn for production monotonic time and isolated deadline tests.
fn worker_turn(shared: &Shared, now: Instant) -> bool {
    let mut inner = lock(&shared.inner);
    if inner.stop {
        inner.sessions.clear();
        return false;
    }
    let mut replies = vec![];
    let mut choices = vec![];
    for ((_, endpoint), session) in &mut inner.sessions {
        let result = session.step(*endpoint, now);
        let ready = session.ready();
        let mut keep = vec![];
        for mut wait in session.waits.drain(..) {
            let outcome = if now >= wait.deadline {
                Some(Outcome::Failure {
                    code: FailureCode::TimedOut,
                })
            } else if wait.outcome.is_some() {
                wait.outcome
            } else if ready && wait.flow.is_none() {
                Some(if protocol::supports(&session.capabilities) {
                    Outcome::Success
                } else {
                    Outcome::Failure {
                        code: FailureCode::Incompatible,
                    }
                })
            } else if let Err(code) = result {
                if wait.flow.is_some()
                    || matches!(
                        code,
                        FailureCode::CredentialsUnavailable | FailureCode::Incompatible
                    )
                {
                    Some(Outcome::Failure { code })
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(outcome) = outcome {
                wait.credentials.take();
                replies.push((wait.reply, outcome));
            } else {
                keep.push(wait);
            }
        }
        session.waits = keep;
        let mut keep = vec![];
        for mut q in session.queries.drain(..) {
            if now >= q.deadline || result.is_err() {
                q.result = Some(ChoiceList::Unavailable);
            }
            if let Some(result) = q.result.take() {
                q.credentials.take();
                choices.push((q.reply, result));
            } else {
                keep.push(q);
            }
        }
        session.queries = keep;
        for w in &mut session.watches {
            if result.is_err() || (w.flow.is_some() && now >= w.deadline) {
                w.flow = None;
                w.sent = None;
                w.next = now + Duration::from_millis(250);
                w.last = VisualState::Unavailable;
                w.sub.update(w.last.clone());
            }
        }
    }
    // Retire the transport before answering the final consumer, not only when credentials
    // are replaced. A later consumer starts a fresh session with no old actions or state.
    inner.sessions.retain(|_, session| session.has_demand());
    drop(inner);
    for (reply, outcome) in replies {
        reply.send(outcome);
    }
    for (reply, list) in choices {
        reply.send(list);
    }
    true
}

impl Session {
    fn has_demand(&self) -> bool {
        !self.waits.is_empty() || !self.queries.is_empty() || !self.watches.is_empty()
    }
    fn event(&mut self, event: &str, now: Instant) {
        if event == "CurrentSceneCollectionChanging" {
            self.blocked = true;
            for w in &mut self.waits {
                if w.flow.is_some() {
                    w.outcome = Some(Outcome::Failure {
                        code: FailureCode::Unavailable,
                    });
                    if let Some(sent) = w.sent.take() {
                        self.discarded = Some((sent, w.deadline));
                    }
                }
            }
        }
        if event == "CurrentSceneCollectionChanged" {
            self.blocked = false;
        }
        let relevant = event.starts_with("Scene")
            || event.starts_with("Input")
            || event.starts_with("SourceFilter")
            || event.starts_with("Current")
            || matches!(event, "RecordStateChanged" | "StreamStateChanged");
        if relevant {
            for q in &mut self.queries {
                q.result = Some(ChoiceList::Unavailable);
                if let Some(sent) = q.sent.take() {
                    self.discarded = Some((sent, q.deadline));
                }
            }
            for w in &mut self.watches {
                if let Some(sent) = w.sent.take() {
                    self.discarded = Some((sent, w.deadline));
                }
                w.flow = None;
                w.next = now;
                w.last = VisualState::Unavailable;
                w.sub.update(w.last.clone());
            }
        }
    }
    fn ready(&self) -> bool {
        self.phase == 3 && !self.blocked && self.pending_text.is_none()
    }

    fn step(&mut self, endpoint: SocketAddrV4, now: Instant) -> Result<(), FailureCode> {
        if self.waits.is_empty()
            && self.queries.is_empty()
            && self.watches.is_empty()
            && self.transport.is_none()
        {
            return Ok(());
        }
        if self.transport.is_none() {
            if now < self.retry {
                return Ok(());
            }
            self.transport = match Transport::connect(endpoint) {
                Ok(transport) => Some(transport),
                Err(code) => {
                    self.failed_attempt(now);
                    return Err(code);
                }
            };
            self.phase = 0;
            self.blocked = false;
            self.attempt = now;
        }
        let result = self.receive(now);
        if result.is_err() {
            self.failed_attempt(now);
        }
        result
    }
    fn failed_attempt(&mut self, now: Instant) {
        self.transport = None;
        self.discarded = None;
        self.pending_text = None;
        self.phase = 0;
        self.retry = now + self.backoff;
        self.backoff = (self.backoff * 2).min(Duration::from_secs(2));
    }
    fn receive(&mut self, now: Instant) -> Result<(), FailureCode> {
        if self.phase < 3 && now.duration_since(self.attempt) >= Duration::from_secs(2) {
            return Err(FailureCode::TimedOut);
        }
        if !self.blocked
            && (self.discarded.as_ref().is_some_and(|(_, d)| now >= *d)
                || self
                    .waits
                    .iter()
                    .any(|w| w.sent.is_some() && now >= w.deadline)
                || self
                    .queries
                    .iter()
                    .any(|q| q.sent.is_some() && now >= q.deadline)
                || self
                    .watches
                    .iter()
                    .any(|w| w.sent.is_some() && now >= w.deadline))
        {
            return Err(FailureCode::TimedOut);
        }
        let mut payload_bytes = 0;
        for _ in 0..8 {
            let text = match self.pending_text.take() {
                Some(text) => Some(text),
                None => self.transport.as_mut().unwrap().receive()?,
            };
            let Some(text) = text else {
                break;
            };
            if text.len() > 65536 {
                return Err(FailureCode::Failed);
            }
            if payload_bytes + text.len() > 65536 {
                self.pending_text = Some(text);
                break;
            }
            payload_bytes += text.len();
            let message: Value = protocol::decode(&text)?;
            if message["op"] == 5 {
                let event = message["d"]["eventType"]
                    .as_str()
                    .ok_or(FailureCode::Failed)?;
                if event == "ExitStarted" {
                    return Err(FailureCode::Unavailable);
                }
                self.event(event, now);
                continue;
            }
            match self.phase {
                0 => {
                    let password = self
                        .waits
                        .first()
                        .and_then(|w| w.credentials.as_ref())
                        .or_else(|| self.queries.first().and_then(|q| q.credentials.as_ref()))
                        .or_else(|| self.watches.first().and_then(|w| w.sub.credentials()))
                        .and_then(|c| c.get("password"));
                    let identify = protocol::identify(&message, password)?;
                    self.transport.as_mut().unwrap().send(identify)?;
                    self.phase = 1;
                }
                1 => {
                    if message["op"] != 2 || message["d"]["negotiatedRpcVersion"] != 1 {
                        return Err(FailureCode::Incompatible);
                    }
                    self.transport.as_mut().unwrap().send(
                        json!({"op":6,"d":{"requestType":"GetVersion","requestId":"version"}})
                            .to_string(),
                    )?;
                    self.phase = 2;
                }
                2 => {
                    if message["op"] == 5 {
                        continue;
                    }
                    if message["op"] != 7
                        || message["d"]["requestId"] != "version"
                        || message["d"]["requestStatus"]["result"] != true
                    {
                        return Err(FailureCode::Incompatible);
                    }
                    let requests = message["d"]["responseData"]["availableRequests"]
                        .as_array()
                        .ok_or(FailureCode::Incompatible)?;
                    if requests.len() > 512 || requests.iter().any(|v| v.as_str().is_none()) {
                        return Err(FailureCode::Incompatible);
                    }
                    self.capabilities = requests.clone();
                    self.phase = 3;
                    self.backoff = Duration::from_millis(250);
                }
                _ => {
                    if message["op"] == 7 {
                        if self
                            .discarded
                            .as_ref()
                            .is_some_and(|(id, _)| protocol::matches_response(Some(id), &message))
                        {
                            self.discarded = None;
                            continue;
                        }
                        if let Some(w) = self
                            .watches
                            .iter_mut()
                            .find(|w| protocol::matches_response(w.sent.as_deref(), &message))
                        {
                            w.sent = None;
                            let result = if message["d"]["requestStatus"]["result"] != true
                                || w.flow
                                    .as_ref()
                                    .and_then(|f| f.request())
                                    .is_none_or(|(r, _)| message["d"]["requestType"] != r)
                            {
                                Err(FailureCode::Unavailable)
                            } else {
                                w.flow
                                    .as_mut()
                                    .ok_or(FailureCode::Failed)?
                                    .accept(message["d"]["responseData"].clone())
                            };
                            if result.is_err() {
                                w.last = VisualState::Unavailable;
                                w.flow = None;
                            } else if w.flow.as_ref().is_some_and(|f| f.request().is_none()) {
                                w.last = visual_state(
                                    w.sub.source(),
                                    w.sub.parameters(),
                                    &w.flow.as_ref().unwrap().result,
                                );
                                w.flow = None;
                            }
                            if w.flow.is_none() {
                                w.next = now + Duration::from_secs(2);
                                w.sub.update(w.last.clone());
                            }
                        }
                        if let Some(q) = self
                            .queries
                            .iter_mut()
                            .find(|q| protocol::matches_response(q.sent.as_deref(), &message))
                        {
                            q.sent = None;
                            if message["d"]["requestStatus"]["result"] != true
                                || q.flow
                                    .request()
                                    .is_none_or(|(r, _)| message["d"]["requestType"] != r)
                                || q.flow.accept(message["d"]["responseData"].clone()).is_err()
                            {
                                q.result = Some(ChoiceList::Unavailable);
                            } else if q.flow.request().is_none() {
                                q.result = Some(q.flow.result());
                            }
                        }
                        if let Some(job) = self
                            .waits
                            .iter_mut()
                            .find(|w| protocol::matches_response(w.sent.as_deref(), &message))
                        {
                            job.sent = None;
                            job.next = now + Duration::from_millis(100);
                            let result = if message["d"]["requestStatus"]["result"] != true
                                || job
                                    .flow
                                    .as_ref()
                                    .and_then(|f| f.request())
                                    .is_none_or(|(r, _)| message["d"]["requestType"] != r)
                            {
                                Err(FailureCode::Failed)
                            } else {
                                job.flow
                                    .as_mut()
                                    .ok_or(FailureCode::Failed)?
                                    .accept(message["d"]["responseData"].clone())
                            };
                            job.outcome = match result {
                                Err(code) => Some(Outcome::Failure { code }),
                                Ok(())
                                    if job.flow.as_ref().is_some_and(|f| f.request().is_none()) =>
                                {
                                    Some(Outcome::Success)
                                }
                                _ => None,
                            };
                        }
                    }
                }
            }
        }
        if self.ready()
            && self.discarded.is_none()
            && !self.waits.iter().any(|w| w.sent.is_some())
            && !self.queries.iter().any(|q| q.sent.is_some())
            && !self.watches.iter().any(|w| w.sent.is_some())
            && let Some(job) = self.waits.iter_mut().find(|w| {
                w.outcome.is_none() && w.flow.is_some() && now < w.deadline && now >= w.next
            })
            && let Some((request, data)) = job.flow.as_ref().unwrap().request()
        {
            if !self.capabilities.iter().any(|v| v == request) {
                job.outcome = Some(Outcome::Failure {
                    code: FailureCode::Incompatible,
                });
                return Ok(());
            }
            self.serial = self.serial.checked_add(1).ok_or(FailureCode::Failed)?;
            let id = self.serial.to_string();
            self.transport.as_mut().unwrap().send(
                json!({"op":6,"d":{"requestType":request,"requestId":id,"requestData":data}})
                    .to_string(),
            )?;
            job.sent = Some(id);
        }
        if self.ready()
            && self.discarded.is_none()
            && !self.waits.iter().any(|w| w.sent.is_some())
            && !self.queries.iter().any(|q| q.sent.is_some())
            && !self.watches.iter().any(|w| w.sent.is_some())
            && let Some(q) = self
                .queries
                .iter_mut()
                .find(|q| q.result.is_none() && now < q.deadline)
            && let Some((request, data)) = q.flow.request()
        {
            if !self.capabilities.iter().any(|v| v == request) {
                q.result = Some(ChoiceList::Unavailable);
                return Ok(());
            }
            self.serial = self.serial.checked_add(1).ok_or(FailureCode::Failed)?;
            let id = self.serial.to_string();
            self.transport.as_mut().unwrap().send(
                json!({"op":6,"d":{"requestType":request,"requestId":id,"requestData":data}})
                    .to_string(),
            )?;
            q.sent = Some(id);
        }
        if self.ready()
            && self.discarded.is_none()
            && !self.waits.iter().any(|w| w.sent.is_some())
            && !self.queries.iter().any(|q| q.sent.is_some())
            && !self.watches.iter().any(|w| w.sent.is_some())
            && let Some(w) = self
                .watches
                .iter_mut()
                .find(|w| w.flow.is_some() || now >= w.next)
        {
            if w.flow.is_none() {
                w.flow = Some(crate::workflow::Workflow::visual(
                    w.sub.source(),
                    w.sub.parameters(),
                )?);
                w.deadline = now + Duration::from_secs(2);
            }
            if let Some((request, data)) = w.flow.as_ref().unwrap().request() {
                if !self.capabilities.iter().any(|v| v == request) {
                    w.flow = None;
                    w.last = VisualState::Unavailable;
                    w.next = now + Duration::from_secs(2);
                    w.sub.update(w.last.clone());
                    return Ok(());
                }
                self.serial = self.serial.checked_add(1).ok_or(FailureCode::Failed)?;
                let id = self.serial.to_string();
                self.transport.as_mut().unwrap().send(
                    json!({"op":6,"d":{"requestType":request,"requestId":id,"requestData":data}})
                        .to_string(),
                )?;
                w.sent = Some(id);
            }
        }
        Ok(())
    }
}
fn visual_state(source: &str, params: &Parameters, data: &Value) -> VisualState {
    let text = match source {
        "record" => match (
            data["outputActive"].as_bool(),
            data["outputPaused"].as_bool(),
        ) {
            (Some(true), Some(true)) => "Paused",
            (Some(true), Some(false)) => "Recording",
            (Some(false), _) => "Stopped",
            _ => return VisualState::Unavailable,
        },
        "stream" => match data["outputActive"].as_bool() {
            Some(true) => "Streaming",
            Some(false) => "Stopped",
            _ => return VisualState::Unavailable,
        },
        "filter" => match data["filterEnabled"].as_bool() {
            Some(true) => "Enabled",
            Some(false) => "Disabled",
            _ => return VisualState::Unavailable,
        },
        "item" => match data["sceneItemEnabled"].as_bool() {
            Some(true) => "Visible",
            Some(false) => "Hidden",
            _ => return VisualState::Unavailable,
        },
        "scene" => {
            let Some(name) = params
                .text("scene")
                .and_then(|v| crate::workflow::target(v).ok())
                .map(|t| t.name)
            else {
                return VisualState::Unavailable;
            };
            if data["currentProgramSceneName"] == name {
                "Program"
            } else {
                "Inactive"
            }
        }
        _ => return VisualState::Unavailable,
    };
    VisualState::Current {
        content: VisualContent {
            text: Some(text.into()),
            ..VisualContent::default()
        },
    }
}
pub(crate) const REQUIRED: &[&str] = &[
    "GetVersion",
    "GetSceneCollectionList",
    "GetSceneList",
    "GetGroupList",
    "GetInputList",
    "GetCurrentProgramScene",
    "SetCurrentProgramScene",
    "GetSceneItemList",
    "GetGroupSceneItemList",
    "GetSceneItemEnabled",
    "SetSceneItemEnabled",
    "GetSourceFilterList",
    "GetSourceFilter",
    "SetSourceFilterEnabled",
    "GetRecordStatus",
    "StartRecord",
    "StopRecord",
    "PauseRecord",
    "ResumeRecord",
    "GetStreamStatus",
    "StartStream",
    "StopStream",
];

#[cfg(test)]
mod tests;
