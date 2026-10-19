//! Nonblocking loopback-only WebSocket; all peer errors become closed codes.
use rad_plugin_sdk::FailureCode;
use std::{
    io,
    net::{SocketAddrV4, TcpStream},
    os::fd::FromRawFd,
};
use tungstenite::{
    HandshakeError, Message, WebSocket,
    handshake::{MidHandshake, client::ClientHandshake},
    protocol::WebSocketConfig,
};

enum State {
    Tcp(TcpStream),
    Handshake(MidHandshake<ClientHandshake<TcpStream>>),
    Open(WebSocket<TcpStream>),
    Broken,
}
pub(crate) struct Transport {
    state: State,
    endpoint: SocketAddrV4,
}
fn blocked(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(e) if e.kind() == io::ErrorKind::WouldBlock)
}
impl Transport {
    pub fn connect(endpoint: SocketAddrV4) -> Result<Self, FailureCode> {
        if !endpoint.ip().is_loopback() || endpoint.port() == 0 {
            return Err(FailureCode::InvalidParameters);
        }
        // SAFETY: fresh owned fd, initialized sockaddr, correct length; ownership transfers once.
        let stream = unsafe {
            let fd = libc::socket(
                libc::AF_INET,
                libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            );
            if fd < 0 {
                return Err(FailureCode::Unavailable);
            }
            let stream = TcpStream::from_raw_fd(fd);
            let address = libc::sockaddr_in {
                sin_family: libc::AF_INET as _,
                sin_port: endpoint.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(endpoint.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            if libc::connect(
                fd,
                (&address as *const libc::sockaddr_in).cast(),
                size_of::<libc::sockaddr_in>() as _,
            ) < 0
                && io::Error::last_os_error().raw_os_error() != Some(libc::EINPROGRESS)
            {
                return Err(FailureCode::Unavailable);
            }
            stream
        };
        Ok(Self {
            state: State::Tcp(stream),
            endpoint,
        })
    }
    fn upgrade(&mut self) -> Result<(), FailureCode> {
        let old = std::mem::replace(&mut self.state, State::Broken);
        let handshake = match old {
            State::Tcp(stream) => {
                if stream
                    .take_error()
                    .map_err(|_| FailureCode::Unavailable)?
                    .is_some()
                {
                    return Err(FailureCode::Unavailable);
                }
                if stream.peer_addr().is_err() {
                    self.state = State::Tcp(stream);
                    return Ok(());
                }
                let config = WebSocketConfig::default()
                    .read_buffer_size(8192)
                    .write_buffer_size(8192)
                    .max_write_buffer_size(65536)
                    .max_message_size(Some(65536))
                    .max_frame_size(Some(65536));
                tungstenite::client::client_with_config(
                    format!("ws://{}/", self.endpoint),
                    stream,
                    Some(config),
                )
            }
            State::Handshake(mid) => mid.handshake(),
            other => {
                self.state = other;
                return Ok(());
            }
        };
        self.state = match handshake {
            Ok((socket, _)) => State::Open(socket),
            Err(HandshakeError::Interrupted(mid)) => State::Handshake(mid),
            Err(HandshakeError::Failure(_)) => return Err(FailureCode::Unavailable),
        };
        Ok(())
    }
    pub fn send(&mut self, text: String) -> Result<(), FailureCode> {
        if text.len() > 32768 {
            return Err(FailureCode::Failed);
        }
        let State::Open(socket) = &mut self.state else {
            return Err(FailureCode::Unavailable);
        };
        match socket.send(Message::Text(text.into())) {
            Ok(()) => Ok(()),
            Err(e) if blocked(&e) => Ok(()), // tungstenite already queued this frame; never resend it.
            Err(_) => Err(FailureCode::Unavailable),
        }
    }
    pub fn receive(&mut self) -> Result<Option<String>, FailureCode> {
        self.upgrade()?;
        let State::Open(socket) = &mut self.state else {
            return Ok(None);
        };
        if let Err(e) = socket.flush()
            && !blocked(&e)
        {
            return Err(FailureCode::Unavailable);
        }
        match socket.read() {
            Ok(Message::Text(text)) => Ok(Some(text.to_string())),
            Ok(Message::Ping(_) | Message::Pong(_)) => Ok(None),
            Ok(Message::Close(Some(frame))) if u16::from(frame.code) == 4009 => {
                Err(FailureCode::CredentialsUnavailable)
            }
            Ok(Message::Close(Some(frame))) if u16::from(frame.code) == 4010 => {
                Err(FailureCode::Incompatible)
            }
            Ok(_) => Err(FailureCode::Unavailable),
            Err(e) if blocked(&e) => Ok(None),
            Err(_) => Err(FailureCode::Unavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::TcpListener,
        time::{Duration, Instant},
    };
    #[test]
    fn websocket_payload_boundary_is_64k_without_truncation() {
        for size in [65536, 65537] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = match listener.local_addr().unwrap() {
                std::net::SocketAddr::V4(v) => v,
                _ => unreachable!(),
            };
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                let _ = ws.send(Message::Text("x".repeat(size).into()));
                std::thread::sleep(Duration::from_millis(100));
            });
            let mut client = Transport::connect(endpoint).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match client.receive() {
                    Ok(None) => {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(Some(text)) => {
                        assert_eq!(size, 65536, "oversized message accepted");
                        assert_eq!(text.len(), size);
                        break;
                    }
                    Err(_) => {
                        assert_eq!(size, 65537, "boundary message rejected");
                        break;
                    }
                }
            }
            server.join().unwrap();
        }
    }
    #[test]
    fn backpressure_flushes_accepted_frames_once_without_resending() {
        use std::os::fd::AsRawFd;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = match listener.local_addr().unwrap() {
            std::net::SocketAddr::V4(v) => v,
            _ => unreachable!(),
        };
        let (go, receive_count) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(Message::Text("ready".into())).unwrap();
            let count = receive_count.recv().unwrap();
            for i in 0..count {
                let msg = ws.read().unwrap();
                let text = msg.to_text().unwrap();
                assert_eq!(text.split_once(':').unwrap().0, i.to_string());
            }
            done_tx.send(()).unwrap();
        });
        let mut client = Transport::connect(endpoint).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while client.receive().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        if let State::Open(ws) = &client.state {
            let size: libc::c_int = 1024;
            // SAFETY: live TCP fd and correctly sized initialized socket option.
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        ws.get_ref().as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_SNDBUF,
                        (&size as *const libc::c_int).cast(),
                        size_of::<libc::c_int>() as _,
                    )
                },
                0
            );
        } else {
            panic!("upgrade missing");
        }
        let mut count = 0;
        for i in 0..1024 {
            if client.send(format!("{i}:{}", "x".repeat(32000))).is_err() {
                break;
            }
            count += 1;
        }
        assert!(
            count > 0 && count < 1024,
            "bounded buffer must exert backpressure"
        );
        go.send(count).unwrap();
        while done_rx.try_recv().is_err() {
            let _ = client.receive();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        server.join().unwrap();
    }
    #[test]
    fn authentication_close_is_terminal_credential_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = match listener.local_addr().unwrap() {
            std::net::SocketAddr::V4(v) => v,
            _ => unreachable!(),
        };
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.close(Some(tungstenite::protocol::CloseFrame {
                code: 4009.into(),
                reason: "synthetic".into(),
            }))
            .unwrap();
        });
        let mut c = Transport::connect(endpoint).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match c.receive() {
                Err(code) => {
                    assert_eq!(code, FailureCode::CredentialsUnavailable);
                    break;
                }
                Ok(_) => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        server.join().unwrap();
    }
    #[test]
    fn nonblocking_transport_upgrades_only_injected_loopback_and_reads_text() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = match listener.local_addr().unwrap() {
            std::net::SocketAddr::V4(v) => v,
            _ => unreachable!(),
        };
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.send(tungstenite::Message::Text("hello".into())).unwrap();
        });
        let mut client = Transport::connect(endpoint).expect("loopback connects lazily");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(text) = client.receive().unwrap() {
                assert_eq!(text, "hello");
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        server.join().unwrap();
        assert!(
            Transport::connect(SocketAddrV4::new(
                std::net::Ipv4Addr::new(192, 0, 2, 1),
                4455
            ))
            .is_err()
        );
    }
}
