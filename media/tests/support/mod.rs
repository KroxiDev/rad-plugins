//! Shared test scaffolding: a private D-Bus daemon, fake MPRIS players, and a scripted host that
//! speaks the plugin protocol with hand-written JSON and framing.
//!
//! Nothing here reaches the session bus of the machine: the daemon is started from a minimal
//! configuration of its own, with no service activation, and the plugin is given only its address.
#![allow(dead_code)]
mod bus;
mod host;
mod player;

#[allow(unused_imports)]
pub use bus::{MuteBus, PrivateBus};
#[allow(unused_imports)]
pub use host::{Host, Visual, quick};
#[allow(unused_imports)]
pub use player::FakePlayer;

use std::path::{Path, PathBuf};

/// A built example of this package: `target/<profile>/examples/<name>`, found from the running
/// test binary. `cargo test` builds them; with a filter such as `--test x` they need
/// `cargo build --examples` first.
pub fn example(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>/deps");
    let path = profile_dir.join("examples").join(name);
    assert!(
        path.exists(),
        "{} is missing: run `cargo test` or `cargo build --examples`",
        path.display()
    );
    path
}

/// A fresh empty directory that is removed again when it is dropped, even if the test panics.
pub struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh empty directory. `short` puts it in the system temporary directory, where the path
/// stays short enough for a socket (108 bytes at most); otherwise it is under the target
/// directory of the tests.
pub fn scratch_dir(label: &str, short: bool) -> Scratch {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let base = if short {
        std::env::temp_dir()
    } else {
        PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
    };
    let dir = base.join(format!(
        "{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}
