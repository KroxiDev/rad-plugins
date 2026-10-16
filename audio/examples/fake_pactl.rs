//! A stand-in for `pactl` that speaks the same commands against a directory (named by
//! `XDG_RUNTIME_DIR`) instead of a sound server. It exists only for the plugin's tests; it never
//! touches the real audio of the machine.
//!
//! Besides the commands it implements (`-f json list sinks|sources`, `-f json info`,
//! `get-default-sink|source`, `set-sink-mute`, `set-source-mute`, `subscribe`) it reads marker
//! files from the directory to inject faults: `down` (connection refused), `hang` (lines of
//! command prefixes that never return), `fail` (mute commands exit 1), `garbage` (listings are
//! not JSON) and `flood` (listings never stop). Every run appends its arguments and the names of
//! its environment variables to `calls.log`.
#[path = "../tests/support/fake_state.rs"]
mod fake_state;

use fake_state::{Dev, State, append, file, read, update};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::exit,
    thread::sleep,
    time::Duration,
};

fn dir() -> PathBuf {
    PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR names the fake server"),
    )
}

fn marker(dir: &Path, name: &str) -> bool {
    file(dir, name).exists()
}

fn refuse() -> ! {
    eprintln!("Connection failure: Connection refused");
    exit(1);
}

fn sink_json(index: usize, dev: &Dev) -> Value {
    json!({
        "index": index, "state": "SUSPENDED", "name": dev.name, "description": dev.description,
        "driver": "fake", "sample_specification": "s16le 2ch 44100Hz", "owner_module": 4294967295u32,
        "mute": dev.mute, "monitor_source": format!("{}.monitor", dev.name),
        "flags": ["HARDWARE"], "properties": {"device.description": dev.description}, "ports": [],
        "active_port": "analog-output", "formats": ["pcm"],
    })
}

/// A source as the real listing shows it: monitors carry the monitored sink in `monitor_source`,
/// plain sources an empty string.
fn source_json(index: usize, name: &str, description: &str, mute: bool, monitored: &str) -> Value {
    json!({
        "index": index, "state": "SUSPENDED", "name": name, "description": description,
        "driver": "fake", "mute": mute, "monitor_source": monitored,
        "flags": ["HARDWARE"], "properties": {"media.class": "Audio/Source"}, "ports": [],
    })
}

fn listing(state: &State, what: &str) -> Value {
    match what {
        "sinks" => state
            .sinks
            .iter()
            .enumerate()
            .map(|(i, d)| sink_json(i, d))
            .collect(),
        "sources" => {
            let monitors = state.sinks.iter().map(|d| {
                (
                    format!("{}.monitor", d.name),
                    format!("Monitor of {}", d.description),
                    d.mute,
                    d.name.clone(),
                )
            });
            let plain = state
                .sources
                .iter()
                .map(|d| (d.name.clone(), d.description.clone(), d.mute, String::new()));
            monitors
                .chain(plain)
                .enumerate()
                .map(|(i, (name, desc, mute, monitored))| {
                    source_json(i, &name, &desc, mute, &monitored)
                })
                .collect()
        }
        _ => exit(1),
    }
}

fn mutate(dir: &Path, kind: &str, target: &str, value: &str) {
    if marker(dir, "fail") {
        exit(1);
    }
    let found = update(dir, |state| {
        let default = if kind == "sink" {
            state.default_sink.clone()
        } else {
            state.default_source.clone()
        };
        let alias = if kind == "sink" {
            "@DEFAULT_SINK@"
        } else {
            "@DEFAULT_SOURCE@"
        };
        let name = if target == alias {
            default.as_str()
        } else {
            target
        };
        // Like the real server, a monitor source can be muted by name: it is the sink's mute.
        let monitored = name.strip_suffix(".monitor").filter(|_| kind == "source");
        let dev = match monitored {
            Some(sink) => state.sinks.iter_mut().find(|d| d.name == sink)?,
            None => {
                let devices = if kind == "sink" {
                    &mut state.sinks
                } else {
                    &mut state.sources
                };
                devices.iter_mut().find(|d| d.name == name)?
            }
        };
        dev.mute = match value {
            "1" => true,
            "0" => false,
            "toggle" => !dev.mute,
            _ => return None,
        };
        Some(())
    });
    if found.is_none() {
        eprintln!("Failure: No such entity");
        exit(1);
    }
    append(dir, "events.log", &format!("Event 'change' on {kind} #1"));
}

fn subscribe(dir: &Path) -> ! {
    // Only events after this point are reported, like the real command; the log line below is
    // written afterwards so a test that has seen it knows the starting point is fixed.
    let mut seen = fs::metadata(file(dir, "events.log"))
        .map(|m| m.len())
        .unwrap_or(0);
    append(
        dir,
        "calls.log",
        &format!("subscribe-start {}", std::process::id()),
    );
    let mut out = std::io::stdout();
    loop {
        if marker(dir, "down") {
            refuse();
        }
        let bytes = fs::read(file(dir, "events.log")).unwrap_or_default();
        if bytes.len() as u64 > seen {
            out.write_all(&bytes[seen as usize..]).expect("stdout");
            out.flush().expect("flush");
            seen = bytes.len() as u64;
        }
        sleep(Duration::from_millis(5));
    }
}

fn main() {
    let dir = dir();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    append(&dir, "calls.log", &format!("argv {}", args.join(" ")));
    let mut names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    append(&dir, "calls.log", &format!("env {}", names.join(",")));
    if marker(&dir, "down") {
        refuse();
    }
    if let Ok(hang) = fs::read_to_string(file(&dir, "hang")) {
        let joined = args.join(" ");
        if hang.lines().any(|prefix| joined.starts_with(prefix)) {
            append(&dir, "calls.log", &format!("hang {}", std::process::id()));
            sleep(Duration::from_secs(60));
            exit(1);
        }
    }
    if args.first().map(String::as_str) == Some("-f") {
        args.drain(..2);
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let state = read(&dir);
    match words.as_slice() {
        ["list", _] if marker(&dir, "garbage") => println!("this is not json"),
        ["list", _] if marker(&dir, "flood") => {
            let block = [b'x'; 65536];
            let mut out = std::io::stdout();
            while out.write_all(&block).is_ok() {}
        }
        ["list", what] => println!("{}", listing(&state, what)),
        ["info"] => println!(
            "{}",
            json!({"server_name": "fake", "default_sink_name": state.default_sink,
                   "default_source_name": state.default_source})
        ),
        ["get-default-sink"] => println!("{}", state.default_sink),
        ["get-default-source"] => println!("{}", state.default_source),
        ["set-sink-mute", target, value] => mutate(&dir, "sink", target, value),
        ["set-source-mute", target, value] => mutate(&dir, "source", target, value),
        ["subscribe"] => subscribe(&dir),
        _ => {
            eprintln!("fake pactl: unsupported command");
            exit(1);
        }
    }
}
