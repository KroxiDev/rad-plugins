//! Isolated conformance entrypoint, never packaged as the public OBS executable.
use std::{
    fs::OpenOptions,
    io::Read,
    net::{Ipv4Addr, SocketAddrV4},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};
fn port() -> Option<u16> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 1 {
        return args[0].parse().ok().filter(|p| *p != 0 && *p != 4455);
    }
    if !args.is_empty() {
        return None;
    }
    let root = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    if !root.is_absolute() {
        return None;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join("rad-obs-fixture.port"))
        .ok()?;
    let meta = file.metadata().ok()?;
    // SAFETY: geteuid has no pointer arguments or preconditions.
    if !meta.is_file()
        || meta.mode() & 0o777 != 0o600
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.len() > 8
    {
        return None;
    }
    let mut value = String::new();
    file.by_ref().take(9).read_to_string(&mut value).ok()?;
    if value.len() > 8 {
        return None;
    }
    value.trim().parse().ok().filter(|p| *p != 0 && *p != 4455)
}
fn main() {
    if let Some(exit) = rad_plugin_sdk::licenses_exit() {
        std::process::exit(if exit == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    let Some(port) = port() else {
        std::process::exit(2);
    };
    let Ok(plugin) =
        rad_plugin_obs::ObsPlugin::for_test(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
    else {
        std::process::exit(2);
    };
    if rad_plugin_obs::run_host(plugin).is_err() {
        std::process::exit(1);
    }
}
