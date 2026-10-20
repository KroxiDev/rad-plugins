//! A private D-Bus daemon: the stand-in for the session bus.
use super::{Scratch, scratch_dir};
use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::{
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

pub struct PrivateBus {
    child: Child,
    address: String,
    dir: Scratch,
    /// The socket the daemon listens on, when its address is meant to survive a restart.
    fixed: Option<PathBuf>,
}

/// A session-type bus listening on `listen`. No `standard_session_servicedirs`, so nothing of the
/// machine can be started by activation; only the services in `services`, if given. Every client
/// may do anything.
fn configuration(listen: &str, services: Option<&Path>) -> String {
    let services = services
        .map(|dir| format!("<servicedir>{}</servicedir>", dir.display()))
        .unwrap_or_default();
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>{listen}</listen>
  {services}
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#
    )
}

fn spawn_daemon(dir: &Path, listen: &str) -> (Child, String) {
    let config = dir.join("bus.conf");
    let services = dir.join("services");
    fs::write(
        &config,
        configuration(listen, services.is_dir().then_some(services.as_path())),
    )
    .unwrap();
    let mut command = Command::new("dbus-daemon");
    command
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: only async-signal-safe `prctl` runs between fork and exec. The daemon dies with
    // the thread that started it, so a test that panics or is killed leaves nothing behind.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .expect("dbus-daemon is needed to run these tests");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let address = line.trim().to_owned();
    assert!(address.starts_with("unix:"), "bus address: {address:?}");
    (child, address)
}

impl PrivateBus {
    pub fn start() -> Self {
        let dir = scratch_dir("rm", true);
        let (child, address) = spawn_daemon(&dir, &format!("unix:tmpdir={}", dir.display()));
        Self {
            child,
            address,
            dir,
            fixed: None,
        }
    }

    /// A bus whose address stays the same when it is killed and `restart`ed.
    pub fn start_at_fixed_address() -> Self {
        let dir = scratch_dir("rm", true);
        let socket = dir.join("bus");
        let (child, _) = spawn_daemon(&dir, &format!("unix:path={}", socket.display()));
        Self {
            child,
            // Without the daemon's `guid`, which changes with every start.
            address: format!("unix:path={}", socket.display()),
            dir,
            fixed: Some(socket),
        }
    }

    /// Starts the daemon again on the same address, after `kill`.
    pub fn restart(&mut self) {
        let socket = self.fixed.clone().expect("a bus with a fixed address");
        let _ = fs::remove_file(&socket);
        let (child, _) = spawn_daemon(&self.dir, &format!("unix:path={}", socket.display()));
        self.child = child;
    }

    /// A bus on which each of `names` can be started by activation. Starting one runs nothing
    /// but `touch` on a marker file, so whether it was started shows in [`Self::started`].
    pub fn start_activatable(names: &[&str]) -> Self {
        let dir = scratch_dir("rm", true);
        let (services, markers) = (dir.join("services"), dir.join("markers"));
        fs::create_dir_all(&services).unwrap();
        fs::create_dir_all(&markers).unwrap();
        let touch = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .map(|dir| dir.join("touch"))
            .find(|touch| touch.is_file())
            .expect("`touch` is needed to run these tests");
        for name in names {
            fs::write(
                services.join(format!("{name}.service")),
                format!(
                    "[D-BUS Service]\nName={name}\nExec={} {}\n",
                    touch.display(),
                    markers.join(name).display()
                ),
            )
            .unwrap();
        }
        let (child, address) = spawn_daemon(&dir, &format!("unix:tmpdir={}", dir.display()));
        Self {
            child,
            address,
            dir,
            fixed: None,
        }
    }

    /// Whether the service `name` of [`Self::start_activatable`] was started. A service that
    /// was just asked for may need a moment to leave its mark.
    pub fn started(&self, name: &str) -> bool {
        self.dir.join("markers").join(name).exists()
    }

    /// Asks the bus to start the service `name`, as any client that does not forbid it would.
    pub fn activate(&self, name: &str) {
        use gio::prelude::*;
        let connection = gio::DBusConnection::for_address_sync(
            self.address(),
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            None::<&gio::Cancellable>,
        )
        .expect("connect to the private bus");
        // The service is a `touch` that never takes the name, so the bus does not answer (or
        // answers with an error); what counts is that it ran the service, so no waiting for it.
        let _ = connection.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "StartServiceByName",
            Some(&(name, 0u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            200,
            None::<&gio::Cancellable>,
        );
    }

    /// Connected clients, excluding this short-lived observer and the daemon.
    pub fn client_count(&self) -> usize {
        let connection = gio::DBusConnection::for_address_sync(
            self.address(),
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            None::<&gio::Cancellable>,
        )
        .unwrap();
        let reply = connection
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ListNames",
                None,
                None,
                gio::DBusCallFlags::NONE,
                1000,
                None::<&gio::Cancellable>,
            )
            .unwrap();
        let (names,) = reply.get::<(Vec<String>,)>().unwrap();
        let count = names.iter().filter(|name| name.starts_with(':')).count() - 1;
        connection.close_sync(None::<&gio::Cancellable>).unwrap();
        count
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// The directory a bus with a fixed address has its socket `bus` in: what a user's
    /// `XDG_RUNTIME_DIR` is for the standard session bus.
    pub fn runtime_dir(&self) -> &Path {
        &self.dir
    }

    /// Kills the daemon, as if the bus went away.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A bus that is there and never says a word: it accepts connections and keeps them open, so a
/// client that connects waits for an authentication that never comes.
pub struct MuteBus {
    path: PathBuf,
    _dir: Scratch,
    stop: Arc<AtomicBool>,
    accepted: Arc<AtomicUsize>,
    acceptor: Option<JoinHandle<()>>,
}

impl MuteBus {
    pub fn start() -> Self {
        let dir = scratch_dir("rm", true);
        let path = dir.join("mute");
        let listener = UnixListener::bind(&path).unwrap();
        let (stop, accepted) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(0)),
        );
        let (stopping, counting) = (Arc::clone(&stop), Arc::clone(&accepted));
        let acceptor = std::thread::spawn(move || {
            // Kept open until the bus is dropped: the clients are left waiting.
            let mut held = Vec::new();
            while let Ok((client, _)) = listener.accept() {
                if stopping.load(Ordering::Acquire) {
                    return;
                }
                counting.fetch_add(1, Ordering::AcqRel);
                held.push(client);
            }
        });
        Self {
            path,
            _dir: dir,
            stop,
            accepted,
            acceptor: Some(acceptor),
        }
    }

    pub fn address(&self) -> String {
        format!("unix:path={}", self.path.display())
    }

    /// How many connections were made to it so far.
    pub fn connections(&self) -> usize {
        self.accepted.load(Ordering::Acquire)
    }
}

impl Drop for MuteBus {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wakes the acceptor, which is blocked in `accept`.
        let _ = UnixStream::connect(&self.path);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}
