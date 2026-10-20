//! The plugin as a process, speaking the protocol over real pipes with a minimal environment.
//!
//! The real binary runs inside a private filesystem and network namespace. This requires
//! bubblewrap and enabled Linux namespaces; setup failures fail the test, never skip it.
mod support;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::Duration,
};
use support::*;

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
            .stderr(Stdio::inherit())
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
                   "plugin_id":"org.rad.audio","plugin_version":{"major":0,"minor":1,"patch":0}}),
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

    fn close_input(&mut self) {
        self.stdin = None;
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// All executables in the no-server tests go through this boundary.
fn isolated_command(binary: &Path) -> Command {
    let mut command = Command::new("/usr/bin/bwrap");
    command.env_clear().args([
        "--die-with-parent",
        "--unshare-all",
        "--new-session",
        "--tmpfs",
        "/",
        "--ro-bind",
        "/usr",
        "/usr",
    ]);
    // Both merged-/usr and traditional Linux library layouts are supported.
    for path in ["/lib", "/lib64"] {
        if Path::new(path).exists() {
            command.args(["--ro-bind", path, path]);
        }
    }
    command
        .args([
            "--symlink",
            "usr/bin",
            "/bin",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--dir",
            "/home/test",
            "--dir",
            "/run/user/test",
            "--dir",
            "/tmp",
            "--setenv",
            "HOME",
            "/home/test",
            "--setenv",
            "XDG_RUNTIME_DIR",
            "/run/user/test",
            "--chdir",
            "/home/test",
            "--ro-bind",
        ])
        .arg(binary)
        .arg("/test-program")
        .arg("--")
        .arg("/test-program");
    command
}

#[test]
fn no_server_boundary_hides_host_paths_and_network() {
    use std::os::unix::net::UnixListener;
    let dir =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("isolation-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("guard.sock");
    let _guard = UnixListener::bind(&socket).unwrap();
    let network = fs::read_link("/proc/self/ns/net").unwrap();
    let output = isolated_command(Path::new("/usr/bin/sh"))
        .args([
            "-c",
            r#"
            test ! -e "$1" || exit 41
            network=$(/usr/bin/readlink /proc/self/ns/net) || exit 42
            test "$network" != "$2" || exit 42
            test "$HOME" = /home/test || exit 43
            test "$XDG_RUNTIME_DIR" = /run/user/test || exit 44
            test ! -e /dev/snd || exit 45
            test ! -e /run/pulse || exit 46
            test ! -e /etc/pulse || exit 47
        "#,
            "guard",
        ])
        .arg(&socket)
        .arg(network)
        .output()
        .expect("run isolated guard (requires bwrap and Linux namespaces)");
    fs::remove_dir_all(dir).unwrap();
    assert!(
        output.status.success(),
        "isolation guard: {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn real_plugin() -> Process {
    Process::spawn(isolated_command(Path::new(env!(
        "CARGO_BIN_EXE_rad-plugin-audio"
    ))))
}

#[test]
fn the_real_binary_handshakes_and_reports_everything_unavailable_without_a_server() {
    let mut plugin = real_plugin();
    plugin.hello(2);
    let ready = plugin.recv().expect("ready");
    assert_eq!(ready["correlation"], json!({"generation":3,"request":1}));
    assert_eq!(
        ready["message"],
        json!({"kind":"ready","plugin_id":"org.rad.audio","plugin_version":{"major":0,"minor":1,"patch":0}})
    );

    plugin.send(
        2,
        json!({"kind":"options","scope":"action","definition":"output","field":"device",
               "context":{"schema_version":1,"values":{}}}),
    );
    assert_eq!(
        plugin.recv().unwrap()["message"]["list"],
        json!({"availability":"unavailable"})
    );

    // A named device that cannot exist: unavailable, and nothing could have been touched.
    plugin.send(
        3,
        json!({"kind":"invoke","action":"output","parameters":{"schema_version":1,
               "values":{"operation":"mute","device":"no-such-device"}}}),
    );
    let result = plugin.recv().unwrap();
    assert_eq!(result["correlation"]["request"], 3);
    assert_eq!(
        result["message"]["outcome"],
        json!({"status":"failure","code":"unavailable"})
    );

    plugin.send(
        4,
        json!({"kind":"subscribe","source":"microphone","parameters":{"schema_version":1,"values":{"device":"@default"}}}),
    );
    let visual = plugin.recv().unwrap();
    assert_eq!(visual["correlation"]["request"], 4);
    assert_eq!(
        visual["message"]["state"],
        json!({"availability":"unavailable"})
    );

    // The host leaving ends the plugin cleanly.
    plugin.close_input();
    let status = plugin.child.wait().unwrap();
    assert!(status.success(), "{status:?}");
}

#[test]
fn the_real_binary_refuses_a_host_api_it_does_not_support() {
    let mut plugin = real_plugin();
    plugin.hello(1);
    assert!(
        plugin.recv().is_none(),
        "no ready for an unsupported host API"
    );
    let status = plugin.child.wait().unwrap();
    assert!(!status.success(), "{status:?}");
}

#[test]
fn the_event_child_dies_with_the_plugin_even_if_the_plugin_is_killed() {
    let server = FakeServer::new();
    let mut command = Command::new(fake::example("plugin_with_fake"));
    command
        .arg(fake::fake_pactl_path())
        .env_clear()
        .env("XDG_RUNTIME_DIR", server.dir());
    let mut plugin = Process::spawn(command);
    plugin.hello(2);
    assert!(plugin.recv().is_some());
    plugin.send(
        2,
        json!({"kind":"subscribe","source":"output","parameters":{"schema_version":1,"values":{"device":"@default"}}}),
    );
    let visual = plugin.recv().unwrap();
    assert_eq!(visual["message"]["state"]["content"]["text"], "On");
    assert_eq!(
        server.live_subscribers(),
        1,
        "the monitor's pactl is running"
    );

    // SIGKILL: the plugin gets no chance to clean up after itself.
    plugin.child.kill().unwrap();
    plugin.child.wait().unwrap();
    assert!(
        eventually(Duration::from_secs(3), || server.live_subscribers() == 0),
        "the pactl subscribe child outlived the plugin"
    );
}
