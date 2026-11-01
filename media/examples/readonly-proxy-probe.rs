//! Private fake-bus gate only: one connection must remain read-only after GetAll.
use gio::{Cancellable, DBusCallFlags, DBusConnection, DBusConnectionFlags, DBusError};
use glib::{Variant, VariantTy, prelude::ToVariant};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    process::ExitCode,
};

const NAME: &str = "org.mpris.MediaPlayer2.brave.instance1";
const OBJECT: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

fn gate_layout(permitted: &Path, root: &Path) -> bool {
    root.is_absolute()
        && root.file_name().is_some_and(|n| n == "validation")
        && root.parent().and_then(Path::parent) == Some(permitted)
}

fn private_address(root: &Path) -> Option<String> {
    if std::env::var("RAD_READONLY_PROXY_PRIVATE_GATE").as_deref() != Ok("1") {
        return None;
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .ok()?;
    let permitted = repo.join("target/performance").canonicalize().ok()?;
    if root.canonicalize().ok()?.as_path() != root || !gate_layout(&permitted, root) {
        return None;
    }
    // SAFETY: geteuid takes no pointers and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    for directory in [root.parent()?, root] {
        let metadata = fs::symlink_metadata(directory).ok()?;
        if !metadata.is_dir() || metadata.mode() & 0o077 != 0 || metadata.uid() != uid {
            return None;
        }
    }
    // Both sockets must be the ones the controller created in its new private gate.
    for name in ["bus", "filtered.sock"] {
        let metadata = fs::symlink_metadata(root.join(name)).ok()?;
        if !metadata.file_type().is_socket() || metadata.uid() != uid {
            return None;
        }
    }
    Some(format!(
        "unix:path={}",
        root.join("filtered.sock").to_str()?
    ))
}

fn call(
    connection: &DBusConnection,
    destination: &str,
    interface: &str,
    method: &str,
    parameters: Option<&Variant>,
    response: &str,
) -> Result<Variant, glib::Error> {
    connection.call_sync(
        Some(destination),
        if destination == "org.freedesktop.DBus" {
            "/org/freedesktop/DBus"
        } else {
            OBJECT
        },
        interface,
        method,
        parameters,
        Some(VariantTy::new(response).expect("fixed valid response type")),
        DBusCallFlags::NO_AUTO_START,
        1000,
        None::<&Cancellable>,
    )
}

fn get_all(connection: &DBusConnection, destination: &str) -> bool {
    call(
        connection,
        destination,
        PROPERTIES,
        "GetAll",
        Some(&(PLAYER,).to_variant()),
        "(a{sv})",
    )
    .ok()
    .and_then(|v| v.get::<(HashMap<String, Variant>,)>())
    .and_then(|(properties,)| properties.get("PlaybackStatus")?.get::<String>())
    .is_some_and(|status| matches!(status.as_str(), "Playing" | "Paused" | "Stopped"))
}

fn access_denied(result: Result<Variant, glib::Error>) -> bool {
    result.is_err_and(|error| error.matches(DBusError::AccessDenied))
}

fn run(root: &Path) -> Option<serde_json::Value> {
    let address = private_address(root)?;
    // This is the only connection constructed: every subsequent call reuses it.
    let connection = DBusConnection::for_address_sync(
        &address,
        DBusConnectionFlags::AUTHENTICATION_CLIENT | DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        None::<&Cancellable>,
    )
    .ok()?;
    let checked = (|| {
        if !get_all(&connection, NAME) {
            return None;
        }
        let (owner,) = call(
            &connection,
            "org.freedesktop.DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            Some(&(NAME,).to_variant()),
            "(s)",
        )
        .ok()?
        .get::<(String,)>()?;
        if !owner.starts_with(':')
            || !owner[1..].bytes().all(|v| v.is_ascii_digit() || v == b'.')
            || !get_all(&connection, &owner)
        {
            return None;
        }
        let set = (PLAYER, "Volume", 0.0_f64.to_variant()).to_variant();
        let mut denied = Vec::with_capacity(4);
        for destination in [NAME, owner.as_str()] {
            denied.push(access_denied(call(
                &connection,
                destination,
                PLAYER,
                "Play",
                None,
                "()",
            )));
            denied.push(access_denied(call(
                &connection,
                destination,
                PROPERTIES,
                "Set",
                Some(&set),
                "()",
            )));
        }
        if !denied.iter().all(|denied| *denied) {
            return None;
        }
        Some(
            serde_json::json!({"complete":true,"persistent_connection":true,
            "known_getall_allowed":true,"unique_getall_allowed":true,
            "known_play_denied":denied[0],"known_set_denied":denied[1],
            "unique_play_denied":denied[2],"unique_set_denied":denied[3],
            "helper_no_auto_start_flag":true}),
        )
    })();
    if connection.close_sync(None::<&Cancellable>).is_err() {
        return None;
    }
    checked
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args.len() == 1 {
        run(&PathBuf::from(&args[0]))
    } else {
        None
    };
    if let Some(result) = result {
        println!("{result}");
        ExitCode::SUCCESS
    } else {
        println!("{{\"complete\":false,\"persistent_connection\":false}}");
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_direct_private_validation_layout_is_accepted() {
        let base = Path::new("/owned/performance");
        assert!(gate_layout(
            base,
            Path::new("/owned/performance/run/validation")
        ));
        for bad in [
            "/personal/bus",
            "/owned/performance/validation",
            "/owned/performance/run/nested/validation",
        ] {
            assert!(!gate_layout(base, Path::new(bad)));
        }
    }

    #[test]
    fn only_access_denied_counts_as_a_rejected_mutation() {
        assert!(access_denied(Err(glib::Error::new(
            DBusError::AccessDenied,
            "synthetic"
        ))));
        assert!(!access_denied(Err(glib::Error::new(
            DBusError::UnknownMethod,
            "synthetic"
        ))));
        assert!(!access_denied(Ok(().to_variant())));
    }
}
