//! Which session bus the plugin is pointed at.
use std::{
    fmt::Write,
    fs,
    os::unix::{ffi::OsStrExt, fs::FileTypeExt},
    path::{Path, PathBuf},
};

/// The address of the session bus: `declared` (`DBUS_SESSION_BUS_ADDRESS`) when there is one,
/// otherwise the standard socket `bus` in the user's `runtime_dir` (`XDG_RUNTIME_DIR`), when it
/// exists. `None` when there is no bus to point at.
///
/// A declared address is not checked: a bad one is for the connection to refuse. Nothing is ever
/// started to make a bus appear.
pub fn session_bus_address(
    declared: Option<String>,
    runtime_dir: Option<PathBuf>,
) -> Option<String> {
    declared
        .filter(|address| !address.is_empty())
        .or_else(|| standard_address(&runtime_dir?))
}

fn standard_address(runtime_dir: &Path) -> Option<String> {
    let socket = runtime_dir.join("bus");
    let is_socket = runtime_dir.is_absolute()
        && fs::metadata(&socket).is_ok_and(|found| found.file_type().is_socket());
    is_socket.then(|| format!("unix:path={}", escaped(&socket)))
}

/// `path` as the value of a bus address key: every byte that is not a letter, a digit or one of
/// `-_/.\*` is written as `%` and two hex digits.
fn escaped(path: &Path) -> String {
    path.as_os_str()
        .as_bytes()
        .iter()
        .fold(String::new(), |mut value, &byte| {
            if byte.is_ascii_alphanumeric() || b"-_/.\\*".contains(&byte) {
                value.push(char::from(byte));
            } else {
                let _ = write!(value, "%{byte:02x}");
            }
            value
        })
}
