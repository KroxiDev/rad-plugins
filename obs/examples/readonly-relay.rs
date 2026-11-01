//! Experimental read-only OBS guard. Real operation belongs to the explicit controller.
use serde_json::{Value, json};

const READS: [&str; 3] = ["GetVersion", "GetRecordStatus", "GetStreamStatus"];
fn keys(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|map| {
        required.iter().all(|key| map.contains_key(*key))
            && map
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
    })
}
fn allowed_request(frame: &Value) -> Option<(&str, &str)> {
    if !keys(frame, &["op", "d"], &[])
        || frame["op"] != 6
        || !keys(&frame["d"], &["requestType", "requestId"], &["requestData"])
    {
        return None;
    }
    let data = &frame["d"];
    if data
        .get("requestData")
        .is_some_and(|v| v.as_object().is_none_or(|v| !v.is_empty()))
    {
        return None;
    }
    let name = data["requestType"].as_str()?;
    let id = data["requestId"].as_str()?;
    (READS.contains(&name)
        && !id.is_empty()
        && id.len() <= 256
        && !id.chars().any(char::is_control))
    .then_some((name, id))
}

use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::{MetadataExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};
use zeroize::{Zeroize, Zeroizing};

type Result<T> = std::result::Result<T, &'static str>;
const MAX_FRAME: usize = 65536;
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_COUNT: u64 = 1_000_000;
fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(8192)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME * 2)
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME))
}
fn add(value: &mut u64) -> Result<()> {
    *value = value
        .checked_add(1)
        .filter(|v| *v <= MAX_COUNT)
        .ok_or("counter_limit")?;
    Ok(())
}
#[derive(Default, Clone)]
struct Counts {
    reads: [u64; 3],
    denied_frames: u64,
    upstream_connections_opened: u64,
    authenticated_upstreams: u64,
    front_auth_failures: u64,
    events_forwarded: u64,
    events_dropped: u64,
    upstream_version: Option<Value>,
}
impl Counts {
    fn snapshot(&self, event: &str, active: bool) -> Value {
        json!({"event":event,"reads":{"GetVersion":self.reads[0],"GetRecordStatus":self.reads[1],"GetStreamStatus":self.reads[2]},
            "denied_frames":self.denied_frames,"upstream_connections_opened":self.upstream_connections_opened,
            "authenticated_upstreams":self.authenticated_upstreams,"active_connections":usize::from(active),
            "front_auth_failures":self.front_auth_failures,"events_forwarded":self.events_forwarded,"events_dropped":self.events_dropped,"upstream_version":self.upstream_version})
    }
}
fn random() -> Result<String> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut *bytes))
        .map_err(|_| "random_failed")?;
    Ok(STANDARD.encode(*bytes))
}
fn authentication(password: &str, salt: &str, challenge: &str) -> Zeroizing<String> {
    let mut first_hash = Sha256::new();
    first_hash.update(password.as_bytes());
    first_hash.update(salt.as_bytes());
    let first = Zeroizing::new(first_hash.finalize().to_vec());
    let secret = Zeroizing::new(STANDARD.encode(&*first));
    // Do not concatenate the real password into any ordinary temporary buffer.
    let mut hash = Sha256::new();
    hash.update(secret.as_bytes());
    hash.update(challenge.as_bytes());
    Zeroizing::new(STANDARD.encode(hash.finalize()))
}
fn text(ws: &mut WebSocket<TcpStream>, value: Value) -> Result<()> {
    // Fail closed on backpressure; never retry an accepted frame or replay a request.
    ws.send(Message::Text(value.to_string().into()))
        .map_err(|_| "socket_write")
}
fn decode(message: Message) -> Result<Value> {
    match message {
        Message::Text(s) if s.len() <= MAX_FRAME => {
            serde_json::from_str(&s).map_err(|_| "invalid_frame")
        }
        _ => Err("invalid_frame"),
    }
}
fn read(ws: &mut WebSocket<TcpStream>) -> Result<Option<Value>> {
    match ws.read() {
        Ok(Message::Ping(_) | Message::Pong(_)) => {
            ws.flush().map_err(|_| "socket_write")?;
            Ok(None)
        }
        Ok(message) => decode(message).map(Some),
        Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(_) => Err("socket_read"),
    }
}
fn one(ws: &mut WebSocket<TcpStream>) -> Result<Value> {
    decode(ws.read().map_err(|_| "handshake_read")?)
}
fn sockets(stream: &TcpStream, timeout: Duration) -> Result<()> {
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| "socket_option")?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| "socket_option")?;
    Ok(())
}
fn front_auth(frame: &Value, token: &str, salt: &str, challenge: &str) -> bool {
    if !keys(frame, &["op", "d"], &[])
        || frame["op"] != 1
        || !keys(
            &frame["d"],
            &["rpcVersion", "authentication"],
            &["eventSubscriptions"],
        )
        || frame["d"]["rpcVersion"] != 1
        || frame["d"]
            .get("eventSubscriptions")
            .is_some_and(|v| v.as_u64().is_none())
    {
        return false;
    }
    let Some(got) = frame["d"]["authentication"]
        .as_str()
        .filter(|s| s.len() <= 256)
    else {
        return false;
    };
    let wanted = authentication(token, salt, challenge);
    let a = Sha256::digest(got.as_bytes());
    let b = Sha256::digest(wanted.as_bytes());
    a.iter()
        .zip(b)
        .fold(0u8, |different, (a, b)| different | (*a ^ b))
        == 0
}
fn upstream(port: u16, password: &str, counts: &mut Counts) -> Result<WebSocket<TcpStream>> {
    let address: SocketAddr = (Ipv4Addr::LOCALHOST, port).into();
    let stream =
        TcpStream::connect_timeout(&address, IO_TIMEOUT).map_err(|_| "upstream_unavailable")?;
    add(&mut counts.upstream_connections_opened)?;
    sockets(&stream, IO_TIMEOUT)?;
    let (mut socket, _) = tungstenite::client::client_with_config(
        format!("ws://127.0.0.1:{port}/"),
        stream,
        Some(config()),
    )
    .map_err(|_| "upstream_handshake")?;
    let hello = one(&mut socket)?;
    if hello["op"] != 0 || hello["d"]["rpcVersion"].as_u64().is_none_or(|v| v < 1) {
        return Err("upstream_hello");
    }
    let auth = &hello["d"]["authentication"];
    let salt = auth["salt"]
        .as_str()
        .filter(|s| s.len() <= 1024)
        .ok_or("upstream_auth_required")?;
    let challenge = auth["challenge"]
        .as_str()
        .filter(|s| s.len() <= 1024)
        .ok_or("upstream_auth_required")?;
    let auth = authentication(password, salt, challenge);
    let mut identify =
        json!({"op":1,"d":{"rpcVersion":1,"eventSubscriptions":64,"authentication":auth.as_str()}});
    let result = text(&mut socket, identify.clone());
    if let Some(Value::String(v)) = identify["d"].get_mut("authentication") {
        v.zeroize();
    }
    result?;
    let identified = one(&mut socket)?;
    if identified["op"] != 2 || identified["d"]["negotiatedRpcVersion"] != 1 {
        return Err("upstream_auth_failed");
    }
    add(&mut counts.authenticated_upstreams)?;
    socket
        .get_mut()
        .set_nonblocking(true)
        .map_err(|_| "socket_option")?;
    Ok(socket)
}
struct Pending {
    id: String,
    kind: String,
}
struct Session {
    front: WebSocket<TcpStream>,
    upstream: WebSocket<TcpStream>,
    pending: BTreeMap<String, Pending>,
    serial: u64,
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.front.get_mut().shutdown(Shutdown::Both);
        let _ = self.upstream.get_mut().shutdown(Shutdown::Both);
    }
}
impl Session {
    fn accept(
        stream: TcpStream,
        token: &str,
        port: u16,
        password: &str,
        counts: &mut Counts,
    ) -> Result<Self> {
        sockets(&stream, IO_TIMEOUT)?;
        let mut front = tungstenite::accept_with_config(stream, Some(config()))
            .map_err(|_| "front_handshake")?;
        let salt = random()?;
        let challenge = random()?;
        text(
            &mut front,
            json!({"op":0,"d":{"obsWebSocketVersion":"5.0.0","rpcVersion":1,"authentication":{"salt":salt,"challenge":challenge}}}),
        )?;
        let frame = one(&mut front)?;
        if !front_auth(&frame, token, &salt, &challenge) {
            add(&mut counts.front_auth_failures)?;
            return Err("front_auth_failed");
        }
        // No connection to the protected endpoint exists before downstream authentication.
        let upstream = upstream(port, password, counts)?;
        text(&mut front, json!({"op":2,"d":{"negotiatedRpcVersion":1}}))?;
        front
            .get_mut()
            .set_nonblocking(true)
            .map_err(|_| "socket_option")?;
        Ok(Self {
            front,
            upstream,
            pending: BTreeMap::new(),
            serial: 0,
        })
    }
    fn deny(&mut self, frame: &Value, counts: &mut Counts) -> Result<()> {
        add(&mut counts.denied_frames)?;
        let id = frame["d"]["requestId"]
            .as_str()
            .filter(|s| s.len() <= 256)
            .unwrap_or("denied");
        let kind = frame["d"]["requestType"]
            .as_str()
            .filter(|s| s.len() <= 128)
            .unwrap_or("Denied");
        text(
            &mut self.front,
            json!({"op":7,"d":{"requestId":id,"requestType":kind,"requestStatus":{"result":false,"code":600,"comment":"Read-only guard denied request"},"responseData":{}}}),
        )
    }
    fn pump(&mut self, counts: &mut Counts) -> Result<()> {
        for _ in 0..8 {
            let Some(frame) = read(&mut self.front)? else {
                break;
            };
            let Some((kind, id)) = allowed_request(&frame) else {
                self.deny(&frame, counts)?;
                continue;
            };
            if self.pending.len() >= 64 || self.pending.values().any(|p| p.id == id) {
                self.deny(&frame, counts)?;
                continue;
            }
            self.serial = self.serial.checked_add(1).ok_or("request_limit")?;
            let upstream_id = format!("r{}", self.serial);
            text(
                &mut self.upstream,
                json!({"op":6,"d":{"requestType":kind,"requestId":upstream_id,"requestData":{}}}),
            )?;
            add(&mut counts.reads[READS.iter().position(|k| *k == kind).unwrap()])?;
            self.pending.insert(
                upstream_id,
                Pending {
                    id: id.into(),
                    kind: kind.into(),
                },
            );
        }
        for _ in 0..8 {
            let Some(frame) = read(&mut self.upstream)? else {
                break;
            };
            if frame["op"] == 5 {
                let kind = frame["d"]["eventType"].as_str().unwrap_or("");
                if ["RecordStateChanged", "StreamStateChanged"].contains(&kind) {
                    text(
                        &mut self.front,
                        json!({"op":5,"d":{"eventType":kind,"eventIntent":64,"eventData":{}}}),
                    )?;
                    add(&mut counts.events_forwarded)?;
                } else {
                    add(&mut counts.events_dropped)?;
                }
            } else {
                if frame["op"] != 7 {
                    return Err("upstream_frame");
                }
                let id = frame["d"]["requestId"]
                    .as_str()
                    .ok_or("upstream_response")?;
                let pending = self.pending.remove(id).ok_or("upstream_response")?;
                if frame["d"]["requestType"] != pending.kind {
                    return Err("upstream_response");
                }
                let result = frame["d"]["requestStatus"]["result"] == true;
                let data = if result {
                    response_data(&pending.kind, &frame["d"]["responseData"])?
                } else {
                    json!({})
                };
                if result && pending.kind == "GetVersion" {
                    let public_version = |key: &str| {
                        data[key].as_str().filter(|v| {
                            !v.is_empty()
                                && v.len() <= 64
                                && v.bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                        })
                    };
                    counts.upstream_version = Some(
                        json!({"obsVersion":public_version("obsVersion"),"obsWebSocketVersion":public_version("obsWebSocketVersion")}),
                    );
                }
                text(
                    &mut self.front,
                    json!({"op":7,"d":{"requestType":pending.kind,"requestId":pending.id,
                    "requestStatus":{"result":result,"code":if result {100}else{600}},"responseData":data}}),
                )?;
            }
        }
        Ok(())
    }
}
fn response_data(kind: &str, data: &Value) -> Result<Value> {
    match kind {
        "GetVersion" => {
            let available = data["availableRequests"]
                .as_array()
                .filter(|v| {
                    v.len() <= 512
                        && v.iter().all(|v| {
                            v.as_str()
                                .is_some_and(|s| s.len() <= 128 && !s.chars().any(char::is_control))
                        })
                })
                .ok_or("upstream_version")?;
            let mut result = json!({"availableRequests":available});
            for name in ["obsVersion", "obsWebSocketVersion"] {
                if let Some(value) = data[name]
                    .as_str()
                    .filter(|s| s.len() <= 128 && !s.chars().any(char::is_control))
                {
                    result[name] = json!(value);
                }
            }
            if let Some(version) = data["rpcVersion"].as_u64() {
                result["rpcVersion"] = json!(version);
            }
            Ok(result)
        }
        "GetRecordStatus" => Ok(
            json!({"outputActive":data["outputActive"].as_bool().ok_or("upstream_status")?,"outputPaused":data["outputPaused"].as_bool().ok_or("upstream_status")?}),
        ),
        "GetStreamStatus" => {
            Ok(json!({"outputActive":data["outputActive"].as_bool().ok_or("upstream_status")?}))
        }
        _ => Err("upstream_response"),
    }
}
#[derive(PartialEq)]
enum ControlCommand {
    Snapshot,
    Finish,
}
struct Control {
    fd: RawFd,
    buffered: Vec<u8>,
    ended: bool,
}
impl Control {
    fn new(fd: RawFd) -> Self {
        Self {
            fd,
            buffered: Vec::new(),
            ended: false,
        }
    }
    fn commands(&mut self) -> Result<Vec<ControlCommand>> {
        let mut commands = Vec::new();
        if self.ended {
            return Ok(vec![ControlCommand::Finish]);
        }
        let mut poll = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: borrowed live controller fd, initialized pollfd, no pointers escape.
        if unsafe { libc::poll(&mut poll, 1, 0) } < 0 {
            return Err("control_poll");
        }
        if poll.revents != 0 {
            let mut bytes = [0u8; 128];
            // SAFETY: readiness was observed; bytes is writable and bounded.
            let n = unsafe { libc::read(self.fd, bytes.as_mut_ptr().cast(), bytes.len()) };
            if n < 0 {
                return Err("control_read");
            }
            if n == 0 {
                self.ended = true;
                return Ok(vec![ControlCommand::Finish]);
            }
            self.buffered.extend_from_slice(&bytes[..n as usize]);
            if self.buffered.len() > 256 {
                return Err("control_limit");
            }
            while let Some(i) = self.buffered.iter().position(|b| *b == b'\n') {
                let line: Vec<_> = self.buffered.drain(..=i).collect();
                commands.push(match line.as_slice() {
                    b"snapshot\n" => ControlCommand::Snapshot,
                    b"finish\n" => ControlCommand::Finish,
                    _ => return Err("invalid_control"),
                });
            }
        }
        Ok(commands)
    }
}
fn wait(
    control: &Control,
    listener: Option<&TcpListener>,
    session: Option<&Session>,
    timeout: Duration,
) -> Result<()> {
    let mut fds = vec![libc::pollfd {
        fd: control.fd,
        events: libc::POLLIN,
        revents: 0,
    }];
    if let Some(listener) = listener {
        fds.push(libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
    }
    if let Some(session) = session {
        for fd in [
            session.front.get_ref().as_raw_fd(),
            session.upstream.get_ref().as_raw_fd(),
        ] {
            fds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
    }
    // SAFETY: all descriptors remain borrowed/alive throughout the bounded poll.
    if unsafe {
        libc::poll(
            fds.as_mut_ptr(),
            fds.len() as _,
            timeout.as_millis().min(100) as _,
        )
    } < 0
    {
        return Err("socket_poll");
    }
    Ok(())
}
fn relay(
    port: u16,
    password: Zeroizing<String>,
    control: &mut Control,
    duration: Duration,
    emit: &mut dyn FnMut(Value) -> Result<()>,
) -> Result<Counts> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut *bytes))
        .map_err(|_| "random_failed")?;
    let token = Zeroizing::new(bytes.iter().map(|v| format!("{v:02x}")).collect::<String>());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| "listen_failed")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "socket_option")?;
    emit(
        json!({"event":"ready","port":listener.local_addr().map_err(|_| "listen_failed")?.port(),"credential":token.as_str()}),
    )?;
    let deadline = Instant::now() + duration;
    let mut counts = Counts::default();
    let mut session = None;
    'serving: while Instant::now() < deadline {
        for command in control.commands()? {
            match command {
                ControlCommand::Finish => break 'serving,
                ControlCommand::Snapshot => emit(counts.snapshot("snapshot", session.is_some()))?,
            }
        }
        match listener.accept() {
            Ok((stream, address)) => {
                if session.is_none() && address.ip() == Ipv4Addr::LOCALHOST {
                    // Per-step500ms limits bound handshake/controller cancellation latency.
                    if deadline.saturating_duration_since(Instant::now()) >= Duration::from_secs(5)
                    {
                        let before = counts.upstream_connections_opened;
                        match Session::accept(stream, &token, port, &password, &mut counts) {
                            Ok(accepted) => session = Some(accepted),
                            Err(
                                "front_handshake" | "handshake_read" | "invalid_frame"
                                | "front_auth_failed",
                            ) if counts.upstream_connections_opened == before => {}
                            // Includes auth rejection/closure during upstream Identify: never retry a real password.
                            Err(_) => return Err("upstream_terminal"),
                        }
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => return Err("listen_failed"),
        }
        if session
            .as_mut()
            .is_some_and(|s| s.pump(&mut counts).is_err())
        {
            session = None;
        }
        wait(
            control,
            Some(&listener),
            session.as_ref(),
            deadline.saturating_duration_since(Instant::now()),
        )?;
    }
    let timed_out = Instant::now() >= deadline;
    drop(session);
    drop(listener);
    drop(token);
    drop(password);
    if timed_out {
        return Err("deadline");
    }
    let mut final_state = counts.snapshot("finished", false);
    final_state["owned_fds_closed"] = json!(true);
    final_state["ui_child_reaped"] = json!(true);
    emit(final_state)?;
    Ok(counts)
}
struct Prompt {
    child: Child,
}
impl Drop for Prompt {
    fn drop(&mut self) {
        // Child remembers a collected status, so kill() cannot signal a reused PID.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn capture_password(
    mut command: Command,
    control: &mut Control,
    timeout: Duration,
) -> Result<Zeroizing<String>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let parent = std::process::id();
    // SAFETY: only async-signal-safe libc calls run after fork, before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0
                || libc::getppid() as u32 != parent
            {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    let prompt = Prompt {
        child: command.spawn().map_err(|_| "prompt_failed")?,
    };
    capture_prompt(prompt, control, timeout)
}
fn capture_prompt(
    mut prompt: Prompt,
    control: &mut Control,
    timeout: Duration,
) -> Result<Zeroizing<String>> {
    let mut stdout = prompt.child.stdout.take().ok_or("prompt_failed")?;
    let fd = stdout.as_raw_fd();
    // SAFETY: fcntl operates on the owned stdout pipe; no flags of controller are changed.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err("prompt_failed");
    }
    let deadline = Instant::now() + timeout;
    let mut secret = Zeroizing::new(Vec::<u8>::new());
    loop {
        if Instant::now() >= deadline || control.commands()?.contains(&ControlCommand::Finish) {
            return Err("prompt_cancelled");
        }
        let mut bytes = Zeroizing::new([0u8; 512]);
        match stdout.read(&mut *bytes) {
            Ok(n) => {
                if secret.len() + n > 4096 {
                    return Err("prompt_limit");
                }
                secret.extend_from_slice(&bytes[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => return Err("prompt_failed"),
        }
        if let Some(status) = prompt.child.try_wait().map_err(|_| "prompt_failed")? {
            // Drain a final already-written pipe fragment before examining the password.
            loop {
                let mut bytes = Zeroizing::new([0u8; 512]);
                match stdout.read(&mut *bytes) {
                    Ok(0) => break,
                    Ok(n) if secret.len() + n <= 4096 => secret.extend_from_slice(&bytes[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    _ => return Err("prompt_failed"),
                }
            }
            if !status.success() {
                return Err("prompt_cancelled");
            }
            while secret.last().is_some_and(|b| b"\n\r".contains(b)) {
                secret.pop();
            }
            let password = std::str::from_utf8(&secret).map_err(|_| "prompt_failed")?;
            if password.is_empty() {
                return Err("prompt_cancelled");
            }
            return Ok(Zeroizing::new(password.to_owned()));
        }
        wait(control, None, None, Duration::from_millis(20))?;
    }
}
fn private_root(path: &Path) -> Result<()> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("target/performance")
        .canonicalize()
        .map_err(|_| "invalid_root")?;
    let canonical = path.canonicalize().map_err(|_| "invalid_root")?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "invalid_root")?;
    if !path.is_absolute()
        || canonical != path
        || !canonical.starts_with(base)
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err("invalid_root");
    }
    Ok(())
}
fn harden(parent: u32) -> Result<()> {
    if parent == 0 || unsafe { libc::getppid() } as u32 != parent {
        return Err("controller_identity");
    }
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: process-local hardening and a valid initialized limit structure.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } < 0
        || unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } < 0
        || unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } < 0
        || unsafe { libc::getppid() } as u32 != parent
    {
        return Err("hardening_failed");
    }
    // stdout is an owned controller pipe, never a destination for personal frames.
    let flags = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err("hardening_failed");
    }
    Ok(())
}
fn output(value: Value) -> Result<()> {
    let bytes = format!("{value}\n");
    if bytes.len() > 4096 {
        return Err("output_limit");
    }
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut sent = 0;
    while sent < bytes.len() {
        if Instant::now() >= deadline {
            return Err("output_closed");
        }
        let mut ready = libc::pollfd {
            fd: libc::STDOUT_FILENO,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: borrowed stdout, initialized bounded poll and live byte slice.
        if unsafe { libc::poll(&mut ready, 1, 20) } < 0 {
            return Err("output_closed");
        }
        if ready.revents == 0 {
            continue;
        }
        let n = unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                bytes[sent..].as_ptr().cast(),
                bytes.len() - sent,
            )
        };
        if n <= 0 {
            return Err("output_closed");
        }
        sent += n as usize;
    }
    Ok(())
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mode = args
        .first()
        .map(String::as_str)
        .ok_or("invalid_arguments")?;
    match (mode, args.len()) {
        ("--validate", 2) => {
            private_root(&PathBuf::from(&args[1]))?;
            harden(unsafe { libc::getppid() } as u32)?;
            output(validate()?)
        }
        ("--real", 3) | ("--synthetic", 4) => {
            private_root(&PathBuf::from(&args[1]))?;
            let parent: u32 = args
                .last()
                .unwrap()
                .parse()
                .map_err(|_| "invalid_arguments")?;
            harden(parent)?;
            let mut control = Control::new(io::stdin().as_raw_fd());
            let (port, password) = if mode == "--real" {
                let mut prompt = Command::new("/usr/bin/zenity");
                prompt.args(["--password", "--title", "RAD · medición OBS (solo lectura)"]);
                (
                    4455,
                    capture_password(prompt, &mut control, Duration::from_secs(120))?,
                )
            } else {
                let port: u16 = args[2].parse().map_err(|_| "invalid_arguments")?;
                if port == 0 || port == 4455 {
                    return Err("invalid_arguments");
                }
                (port, Zeroizing::new("correct".to_owned()))
            };
            relay(
                port,
                password,
                &mut control,
                Duration::from_secs(300),
                &mut output,
            )?;
            Ok(())
        }
        _ => Err("invalid_arguments"),
    }
}
fn main() {
    if run().is_err() {
        let _ = output(json!({"event":"error","code":"relay_incomplete"}));
        std::process::exit(1);
    }
}

// The standalone gate exercises the same relay on an owned ephemeral fake only.
mod gate {
    use super::*;
    use std::{
        os::unix::net::UnixStream,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread::{self, JoinHandle},
    };
    #[derive(Default, Clone)]
    struct Observed {
        requests: Vec<String>,
        effects: usize,
        connections: usize,
        outputs_only: bool,
        closed: usize,
    }
    struct Fake {
        port: u16,
        stop: Arc<AtomicBool>,
        observed: Arc<Mutex<Observed>>,
        worker: Option<JoinHandle<()>>,
    }
    impl Fake {
        fn start() -> Result<Self> {
            let listener =
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| "gate_listen")?;
            let port = listener.local_addr().map_err(|_| "gate_listen")?.port();
            if port == 4455 {
                return Err("gate_port");
            }
            listener.set_nonblocking(true).map_err(|_| "gate_listen")?;
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let observed = Arc::new(Mutex::new(Observed {
                outputs_only: true,
                ..Observed::default()
            }));
            let observations = observed.clone();
            let worker = thread::spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            observations.lock().unwrap().connections += 1;
                            let _ = sockets(&stream, Duration::from_millis(100));
                            if let Ok(mut ws) =
                                tungstenite::accept_with_config(stream, Some(config()))
                            {
                                let result = (|| -> Result<()> {
                                    text(
                                        &mut ws,
                                        json!({"op":0,"d":{"rpcVersion":1,"authentication":{"salt":"fixture-salt","challenge":"fixture-challenge"}}}),
                                    )?;
                                    let frame = one(&mut ws)?;
                                    if !front_auth(
                                        &frame,
                                        "correct",
                                        "fixture-salt",
                                        "fixture-challenge",
                                    ) || frame["d"]["eventSubscriptions"] != 64
                                    {
                                        observations.lock().unwrap().outputs_only = false;
                                        return Err("gate_auth");
                                    }
                                    text(&mut ws, json!({"op":2,"d":{"negotiatedRpcVersion":1}}))?;
                                    while !flag.load(Ordering::Acquire) {
                                        let frame = match read(&mut ws) {
                                            Ok(Some(v)) => v,
                                            Ok(None) => continue,
                                            Err(_) => break,
                                        };
                                        let kind =
                                            frame["d"]["requestType"].as_str().unwrap_or("invalid");
                                        {
                                            let mut o = observations.lock().unwrap();
                                            if o.requests.len() >= 128 {
                                                return Err("gate_limit");
                                            }
                                            o.requests.push(kind.into());
                                            if frame["op"] != 6
                                                || !READS.contains(&kind)
                                                || frame["d"]["requestData"] != json!({})
                                            {
                                                o.effects += 1;
                                            }
                                        }
                                        if kind == "GetRecordStatus" {
                                            for event in [
                                                "RecordStateChanged",
                                                "StreamStateChanged",
                                                "InputVolumeChanged",
                                            ] {
                                                text(
                                                    &mut ws,
                                                    json!({"op":5,"d":{"eventType":event,"eventIntent":64,"eventData":{"outputPath":"/private/path.mkv","inputName":"private-source","outputState":"private-state"}}}),
                                                )?;
                                            }
                                        }
                                        let data = match kind {
                                            "GetVersion" => {
                                                json!({"availableRequests":["GetVersion","GetRecordStatus","GetStreamStatus","StartRecord"],"obsVersion":"fixture-version","obsWebSocketVersion":"5.0.0","rpcVersion":1,"platformDescription":"private-host"})
                                            }
                                            "GetRecordStatus" => {
                                                json!({"outputActive":false,"outputPaused":false,"outputPath":"/private/path.mkv"})
                                            }
                                            _ => {
                                                json!({"outputActive":false,"outputName":"private-channel"})
                                            }
                                        };
                                        text(
                                            &mut ws,
                                            json!({"op":7,"d":{"requestType":kind,"requestId":frame["d"]["requestId"],"requestStatus":{"result":true,"code":100,"comment":"private diagnostic"},"responseData":data}}),
                                        )?;
                                    }
                                    Ok(())
                                })();
                                let _ = result;
                            }
                            observations.lock().unwrap().closed += 1;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(_) => break,
                    }
                }
            });
            Ok(Self {
                port,
                stop,
                observed,
                worker: Some(worker),
            })
        }
        fn state(&self) -> Observed {
            self.observed.lock().unwrap().clone()
        }
        fn finish(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.finish();
        }
    }
    struct Relay {
        control: UnixStream,
        events: mpsc::Receiver<Value>,
        worker: Option<JoinHandle<Result<Counts>>>,
        port: u16,
        token: Zeroizing<String>,
    }
    impl Relay {
        fn start(upstream: u16) -> Result<Self> {
            Self::start_password(upstream, "correct")
        }
        fn start_password(upstream: u16, password: &str) -> Result<Self> {
            let password = Zeroizing::new(password.to_owned());
            let (control, reader) = UnixStream::pair().map_err(|_| "gate_control")?;
            let (emit, events) = mpsc::sync_channel(32);
            let worker = thread::spawn(move || {
                relay(
                    upstream,
                    password,
                    &mut Control::new(reader.as_raw_fd()),
                    Duration::from_secs(10),
                    &mut |v| emit.try_send(v).map_err(|_| "gate_output"),
                )
            });
            let ready = events
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "gate_ready")?;
            let port = ready["port"]
                .as_u64()
                .filter(|v| *v > 0 && *v <= 65535 && *v != 4455)
                .ok_or("gate_ready")? as u16;
            let token =
                Zeroizing::new(ready["credential"].as_str().ok_or("gate_ready")?.to_owned());
            if token.len() != 64
                || !token
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("gate_token");
            }
            Ok(Self {
                control,
                events,
                worker: Some(worker),
                port,
                token,
            })
        }
        fn snapshot(&mut self) -> Result<Value> {
            self.control
                .write_all(b"snapshot\n")
                .map_err(|_| "gate_control")?;
            self.events
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "gate_snapshot")
        }
        fn finish(&mut self, eof: bool) -> Result<Value> {
            if eof {
                self.control
                    .shutdown(Shutdown::Write)
                    .map_err(|_| "gate_control")?;
            } else {
                self.control
                    .write_all(b"finish\n")
                    .map_err(|_| "gate_control")?;
            }
            let finished = self
                .events
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "gate_finish")?;
            self.worker
                .take()
                .ok_or("gate_join")?
                .join()
                .map_err(|_| "gate_join")??;
            Ok(finished)
        }
    }
    impl Drop for Relay {
        fn drop(&mut self) {
            let _ = self.control.shutdown(Shutdown::Write);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
    fn client(port: u16, password: &str) -> Result<WebSocket<TcpStream>> {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).map_err(|_| "gate_connect")?;
        sockets(&stream, Duration::from_secs(2))?;
        let (mut ws, _) = tungstenite::client::client_with_config(
            format!("ws://127.0.0.1:{port}/"),
            stream,
            Some(config()),
        )
        .map_err(|_| "gate_handshake")?;
        let hello = one(&mut ws)?;
        let salt = hello["d"]["authentication"]["salt"]
            .as_str()
            .ok_or("gate_hello")?;
        let challenge = hello["d"]["authentication"]["challenge"]
            .as_str()
            .ok_or("gate_hello")?;
        text(
            &mut ws,
            json!({"op":1,"d":{"rpcVersion":1,"eventSubscriptions":239,"authentication":authentication(password,salt,challenge).as_str()}}),
        )?;
        if one(&mut ws)?["op"] != 2 {
            return Err("gate_identified");
        }
        Ok(ws)
    }
    fn request(
        ws: &mut WebSocket<TcpStream>,
        frame: Value,
        events: &mut Vec<Value>,
    ) -> Result<Value> {
        text(ws, frame)?;
        for _ in 0..8 {
            let value = one(ws)?;
            if value["op"] == 5 {
                events.push(value);
            } else {
                return Ok(value);
            }
        }
        Err("gate_frames")
    }
    #[cfg(test)]
    pub(super) fn terminal_auth_failure() -> Result<bool> {
        let mut fake = Fake::start()?;
        let mut service = Relay::start_password(fake.port, "wrong-upstream-password")?;
        let rejected = client(service.port, &service.token).is_err();
        let end = Instant::now() + Duration::from_millis(150);
        while Instant::now() < end && !service.worker.as_ref().unwrap().is_finished() {
            thread::sleep(Duration::from_millis(2));
        }
        let terminal = service.worker.as_ref().unwrap().is_finished();
        let terminal_error = if terminal {
            service
                .worker
                .take()
                .unwrap()
                .join()
                .map_err(|_| "gate_join")?
                .is_err()
        } else {
            service.finish(false)?;
            false
        };
        let listener_closed = TcpStream::connect((Ipv4Addr::LOCALHOST, service.port)).is_err();
        fake.finish();
        Ok(rejected && terminal_error && listener_closed && fake.state().connections == 1)
    }
    pub(super) fn validate() -> Result<Value> {
        let mut fake = Fake::start()?;
        let mut service = Relay::start(fake.port)?;
        let wrong = client(service.port, "not-the-token").is_err();
        let first = service.snapshot()?;
        let wrong_before_upstream = wrong
            && first["front_auth_failures"] == 1
            && first["upstream_connections_opened"] == 0
            && first["upstream_version"].is_null()
            && fake.state().connections == 0;
        let mut front = client(service.port, &service.token)?;
        let mut events = Vec::new();
        let mut reads_ok = true;
        let mut payloads_private = true;
        let mut version_actual = false;
        for (i, kind) in READS.iter().enumerate() {
            let response = request(
                &mut front,
                json!({"op":6,"d":{"requestType":kind,"requestId":format!("read-{i}"),"requestData":{}}}),
                &mut events,
            )?;
            reads_ok &= response["d"]["requestStatus"]["result"] == true;
            payloads_private &= !response.to_string().contains("private");
            if *kind == "GetVersion" {
                version_actual = response["d"]["responseData"]["obsVersion"] == "fixture-version"
                    && response["d"]["responseData"]["availableRequests"]
                        .as_array()
                        .is_some_and(|a| a.contains(&json!("StartRecord")));
            }
        }
        let mut denied = Vec::new();
        for kind in [
            "StartRecord",
            "StopRecord",
            "PauseRecord",
            "ResumeRecord",
            "StartStream",
            "StopStream",
            "SetCurrentProgramScene",
            "SetSceneItemEnabled",
            "CallVendorRequest",
            "GetUnknown",
        ] {
            denied.push(
                json!({"op":6,"d":{"requestType":kind,"requestId":"denial","requestData":{}}}),
            );
        }
        denied.push(
            json!({"op":8,"d":{"requestId":"batch","requests":[{"requestType":"StartRecord"}]}}),
        );
        denied.push(json!({"op":99,"d":{}}));
        for kind in READS {
            denied.push(json!({"op":6,"d":{"requestType":kind,"requestId":"args","requestData":{"unexpected":true}}}));
        }
        denied.push(
            json!({"op":6,"d":{"requestType":"GetVersion","requestId":"extra","unexpected":true}}),
        );
        let expected_denied = denied.len();
        let mut denied_ok = true;
        for frame in denied {
            denied_ok &=
                request(&mut front, frame, &mut events)?["d"]["requestStatus"]["result"] == false;
        }
        let snapshot = service.snapshot()?;
        let state = fake.state();
        let no_upstream_mutations = state.effects == 0 && state.requests == READS;
        let events_stripped = events.len() == 2
            && events.iter().all(|v| {
                ["RecordStateChanged", "StreamStateChanged"]
                    .contains(&v["d"]["eventType"].as_str().unwrap_or(""))
                    && v["d"]["eventData"] == json!({})
            });
        let snapshot_exact = snapshot["reads"]
            == json!({"GetVersion":1,"GetRecordStatus":1,"GetStreamStatus":1})
            && snapshot["denied_frames"] == expected_denied
            && snapshot["events_forwarded"] == 2
            && snapshot["events_dropped"] == 1
            && snapshot["upstream_version"]
                == json!({"obsVersion":"fixture-version","obsWebSocketVersion":"5.0.0"});
        let finished = service.finish(false)?;
        drop(front);
        fake.finish();
        let state = fake.state();
        let cleanup = finished["active_connections"] == 0
            && finished["owned_fds_closed"] == true
            && finished["ui_child_reaped"] == true
            && state.connections == state.closed;
        // A separate EOF path proves the controller lifeline without any endpoint connection.
        let mut eof_service = Relay::start(fake.port)?;
        let eof = eof_service.finish(true)?;
        let synthetic_only = fake.port != 4455 && service.port != 4455 && eof_service.port != 4455;
        let complete = reads_ok
            && denied_ok
            && wrong_before_upstream
            && no_upstream_mutations
            && state.outputs_only
            && events_stripped
            && payloads_private
            && version_actual
            && snapshot_exact
            && cleanup
            && eof["owned_fds_closed"] == true;
        Ok(
            json!({"event":"validation","complete":complete,"synthetic_only":synthetic_only,"three_reads_permitted":reads_ok,"writes_denied_post_auth":denied_ok,
            "wrong_auth_before_upstream":wrong_before_upstream,"zero_upstream_effects":no_upstream_mutations,
            "upstream_outputs_only":state.outputs_only,"events_filtered_and_stripped":events_stripped,
            "private_response_fields_stripped":payloads_private,"version_is_upstream_not_synthetic":version_actual,
            "snapshot_exact":snapshot_exact,"finish_closes_owned_fds":cleanup,"eof_closes_owned_fds":eof["owned_fds_closed"] == true}),
        )
    }
}
fn validate() -> Result<Value> {
    gate::validate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    #[test]
    fn request_policy_denies_mutations_batches_unknowns_and_arguments() {
        for frame in [
            json!({"op":6,"d":{"requestType":"StartRecord","requestId":"x","requestData":{}}}),
            json!({"op":8,"d":{"requestType":"GetVersion","requestId":"x"}}),
            json!({"op":6,"d":{"requestType":"Unknown","requestId":"x"}}),
            json!({"op":6,"d":{"requestType":"GetVersion","requestId":"x","requestData":{"unexpected":true}}}),
            json!({"op":6,"d":{"requestType":"GetVersion","requestId":"x".repeat(257)}}),
            json!({"op":6,"d":{"requestType":"GetVersion","requestId":"x"},"extra":true}),
        ] {
            assert!(allowed_request(&frame).is_none());
        }
        for kind in READS {
            assert!(
                allowed_request(&json!({"op":6,"d":{"requestType":kind,"requestId":"ok"}}))
                    .is_some()
            );
        }
    }
    #[test]
    fn actual_authenticated_relay_blocks_effects_and_cleans_its_lifelines() {
        let result = validate().unwrap();
        for (key, value) in result.as_object().unwrap() {
            if key != "event" {
                assert_eq!(*value, true, "{key}");
            }
        }
    }
    #[test]
    fn upstream_authentication_failure_is_terminal_without_password_retry() {
        assert!(gate::terminal_auth_failure().unwrap());
    }
    #[test]
    fn independent_authentication_vector_and_upstream_privacy_filter() {
        assert_eq!(
            authentication(
                "supersecretpassword",
                "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=",
                "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY="
            )
            .as_str(),
            "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4="
        );
        assert_eq!(
            response_data(
                "GetRecordStatus",
                &json!({"outputActive":true,"outputPaused":false,"outputPath":"PRIVATE"})
            )
            .unwrap(),
            json!({"outputActive":true,"outputPaused":false})
        );
        assert!(
            response_data(
                "GetRecordStatus",
                &json!({"outputActive":"true","outputPaused":false})
            )
            .is_err()
        );
    }
    #[test]
    fn mock_prompt_secret_is_returned_only_in_memory_and_cancel_is_reaped() {
        let (writer, reader) = UnixStream::pair().unwrap();
        let mut command = Command::new("/usr/bin/printf");
        command.arg("fixture-password\n");
        let password = capture_password(
            command,
            &mut Control::new(reader.as_raw_fd()),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(password.as_str(), "fixture-password");
        drop(password);
        let child = Command::new("/usr/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let id = child.id();
        // Closing only this owned controller pipe must cancel and reap the owned mock UI.
        drop(writer);
        assert!(
            capture_prompt(
                Prompt { child },
                &mut Control::new(reader.as_raw_fd()),
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert_eq!(unsafe { libc::kill(id as i32, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }
    #[test]
    fn prompt_timeout_and_user_cancel_are_bounded_and_never_a_password() {
        let (_writer, reader) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let mut command = Command::new("/usr/bin/sleep");
        command.arg("30");
        assert!(
            capture_password(
                command,
                &mut Control::new(reader.as_raw_fd()),
                Duration::from_millis(25)
            )
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(
            capture_password(
                Command::new("/usr/bin/false"),
                &mut Control::new(reader.as_raw_fd()),
                Duration::from_secs(1)
            )
            .is_err()
        );
    }
}
