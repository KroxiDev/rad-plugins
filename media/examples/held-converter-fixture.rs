//! Explicit private control socket + bus, never a desktop-bus fallback.
use rad_plugin_media::{MediaPlugin, Timing, config};
use std::{
    io::Read,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    time::Duration,
};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Only a package installed by the private test Store is runnable. Do not use
    // inherited desktop environment variables to select a gate or a bus.
    let executable = std::env::current_exe()?.canonicalize()?;
    let plugins = executable
        .ancestors()
        .find(|path| {
            path.file_name().is_some_and(|v| v == "plugins")
                && path
                    .parent()
                    .is_some_and(|p| p.file_name().is_some_and(|v| v == "store"))
        })
        .ok_or("private fixture Store executable required")?;
    let root: PathBuf = plugins
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or("private fixture Store required")?
        .join("runtime");
    let metadata = std::fs::symlink_metadata(&root)?;
    if !root.is_absolute() || !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err("private runtime required".into());
    }
    let mut gate = UnixStream::connect(root.join("cover.fixture"))?;
    gate.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut length = [0; 4];
    gate.read_exact(&mut length)?;
    let size = u32::from_be_bytes(length) as usize;
    if !(1..=4096).contains(&size) {
        return Err("invalid private bus address".into());
    }
    let mut address = vec![0; size];
    gate.read_exact(&mut address)?;
    let address = String::from_utf8(address)?;
    let expected = format!("unix:path={}", root.join("private-bus").display());
    if address != expected && !address.starts_with(&(expected + ",")) {
        return Err("private bus outside fixture runtime".into());
    }
    let plugin = MediaPlugin::with_fixture_converter(address, Timing::default(), gate);
    rad_plugin_sdk::run_stdio(config(), plugin)?;
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("held converter fixture: {error}");
        std::process::exit(1);
    }
}
