//! Which session bus the plugin is pointed at: the one it was told about, otherwise the standard
//! socket of the user's runtime directory.
mod support;
use rad_plugin_media::session_bus_address;
use std::{fs, os::unix::net::UnixListener, path::Path};
use support::*;

fn address(declared: Option<&str>, runtime: Option<&Path>) -> Option<String> {
    session_bus_address(declared.map(str::to_owned), runtime.map(Path::to_path_buf))
}

#[test]
fn a_declared_address_is_used_as_it_is() {
    let runtime = scratch_dir("rm", true);
    let _bus = UnixListener::bind(runtime.join("bus")).unwrap();
    assert_eq!(
        address(Some("unix:path=/somewhere/else"), Some(&runtime)),
        Some("unix:path=/somewhere/else".to_owned()),
        "the declared bus wins over the standard one"
    );
    assert_eq!(
        address(Some("not an address"), None),
        Some("not an address".to_owned()),
        "a bad address is for the connection to refuse, not for this to second-guess"
    );
}

#[test]
fn without_an_address_the_standard_socket_of_the_runtime_directory_is_used() {
    let runtime = scratch_dir("rm", true);
    let _bus = UnixListener::bind(runtime.join("bus")).unwrap();
    let expected = Some(format!("unix:path={}/bus", runtime.display()));
    assert_eq!(address(None, Some(&runtime)), expected);
    assert_eq!(
        address(Some(""), Some(&runtime)),
        expected,
        "empty is missing"
    );
}

#[test]
fn there_is_no_default_when_there_is_no_bus_socket_to_point_at() {
    let runtime = scratch_dir("rm", true);
    assert_eq!(address(None, Some(&runtime)), None, "nothing there");
    fs::write(runtime.join("bus"), "not a socket").unwrap();
    assert_eq!(address(None, Some(&runtime)), None, "a file is no bus");
    assert_eq!(address(None, None), None, "no runtime directory");
}

#[test]
fn a_relative_runtime_directory_is_not_followed() {
    // A runtime directory is an absolute path; a relative one would mean a different bus for
    // every working directory. Here the relative path does lead to a socket called `bus`.
    let runtime = scratch_dir("rm", true);
    let _bus = UnixListener::bind(runtime.join("bus")).unwrap();
    let up = std::env::current_dir().unwrap().ancestors().count() - 1;
    let relative = Path::new(&"../".repeat(up)).join(runtime.strip_prefix("/").unwrap());
    assert!(relative.join("bus").exists(), "{relative:?}");
    assert_eq!(address(None, Some(&relative)), None);
}

#[test]
fn a_path_is_escaped_the_way_bus_addresses_require() {
    let runtime = scratch_dir("rm a,b=c;d", true);
    let _bus = UnixListener::bind(runtime.join("bus")).unwrap();
    let escaped = format!(
        "unix:path={}/bus",
        runtime
            .display()
            .to_string()
            .replace(' ', "%20")
            .replace(',', "%2c")
            .replace('=', "%3d")
            .replace(';', "%3b")
    );
    assert_eq!(address(None, Some(&runtime)), Some(escaped));
}
