//! Mute, unmute and toggle of the output and the microphone, against the fake sound server.
mod support;
use rad_plugin_audio::{AudioPlugin, Kind};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::*;

fn success() -> Value {
    json!({"status":"success"})
}
fn failure(code: &str) -> Value {
    json!({"status":"failure","code":code})
}

#[test]
fn toggle_mute_and_unmute_act_on_the_default_device_of_each_kind() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    for (action, kind, default) in [
        ("output", Kind::Output, "sink-a"),
        ("microphone", Kind::Microphone, "mic-a"),
    ] {
        assert!(!server.is_muted(kind, default));
        assert_eq!(host.invoke(action, "toggle", "@default"), success());
        assert!(server.is_muted(kind, default), "{action}: toggled on");
        assert_eq!(host.invoke(action, "toggle", "@default"), success());
        assert!(!server.is_muted(kind, default), "{action}: toggled off");
        assert_eq!(host.invoke(action, "mute", "@default"), success());
        assert_eq!(host.invoke(action, "mute", "@default"), success());
        assert!(
            server.is_muted(kind, default),
            "{action}: muting twice leaves it muted"
        );
        assert_eq!(host.invoke(action, "unmute", "@default"), success());
        assert_eq!(host.invoke(action, "unmute", "@default"), success());
        assert!(
            !server.is_muted(kind, default),
            "{action}: unmuting twice leaves it unmuted"
        );
    }
    host.finish().unwrap();
}

#[test]
fn the_default_alias_is_resolved_by_the_server_so_it_follows_a_change_of_default() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    assert_eq!(host.invoke("output", "mute", "@default"), success());
    server.set_default(Kind::Output, "sink-b");
    assert_eq!(host.invoke("output", "mute", "@default"), success());
    assert!(server.is_muted(Kind::Output, "sink-a") && server.is_muted(Kind::Output, "sink-b"));
    assert_eq!(host.invoke("output", "unmute", "@default"), success());
    assert!(
        server.is_muted(Kind::Output, "sink-a"),
        "the former default is left alone"
    );
    assert!(!server.is_muted(Kind::Output, "sink-b"));
    assert!(
        server
            .calls()
            .iter()
            .any(|c| c == "set-sink-mute @DEFAULT_SINK@ 1"),
        "{:?}",
        server.calls()
    );
    host.finish().unwrap();
}

#[test]
fn a_chosen_device_is_muted_whatever_the_default_is() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    assert_eq!(host.invoke("output", "mute", "sink-b"), success());
    assert_eq!(host.invoke("microphone", "toggle", "mic-b"), success());
    assert!(server.is_muted(Kind::Output, "sink-b") && server.is_muted(Kind::Microphone, "mic-b"));
    assert!(
        !server.is_muted(Kind::Output, "sink-a") && !server.is_muted(Kind::Microphone, "mic-a")
    );
    assert_eq!(host.invoke("output", "toggle", "sink-b"), success());
    assert!(!server.is_muted(Kind::Output, "sink-b"));
    host.finish().unwrap();
}

#[test]
fn a_chosen_device_that_is_missing_is_unavailable_and_nothing_else_changes() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    let before = server.state();
    // Missing, a monitor (not a microphone), a sink named as a source and the other way round.
    for (action, device) in [
        ("output", "sink-gone"),
        ("microphone", "mic-gone"),
        ("microphone", "sink-a.monitor"),
        ("microphone", "sink-a"),
        ("output", "mic-a"),
    ] {
        for operation in ["toggle", "mute", "unmute"] {
            assert_eq!(
                host.invoke(action, operation, device),
                failure("unavailable"),
                "{action} {operation} {device}"
            );
        }
    }
    assert_eq!(
        server.mutating_calls(),
        Vec::<String>::new(),
        "no command that changes the server ran"
    );
    assert_eq!(server.state().sinks, before.sinks);
    assert_eq!(server.state().sources, before.sources);
    // A device that was there and then went away behaves the same.
    assert_eq!(host.invoke("output", "mute", "sink-b"), success());
    server.remove_device(Kind::Output, "sink-b");
    assert_eq!(
        host.invoke("output", "unmute", "sink-b"),
        failure("unavailable")
    );
    assert!(
        server.state().sinks.iter().all(|d| d.name != "sink-b"),
        "it was not recreated"
    );
    host.finish().unwrap();
}

#[test]
fn a_device_named_like_an_option_is_refused_without_running_anything() {
    let server = FakeServer::new();
    server.add_device(Kind::Output, "-n", "Strange");
    let mut host = Host::start(&server, quick());
    assert_eq!(
        host.invoke("output", "mute", "-n"),
        failure("invalid_parameters")
    );
    assert_eq!(server.mutating_calls(), Vec::<String>::new());
    host.finish().unwrap();
}

#[test]
fn an_unreachable_server_is_unavailable_for_the_default_and_for_a_chosen_device() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    server.down();
    assert_eq!(
        host.invoke("output", "mute", "@default"),
        failure("unavailable")
    );
    assert_eq!(
        host.invoke("microphone", "toggle", "mic-a"),
        failure("unavailable")
    );
    server.up();
    assert_eq!(host.invoke("output", "mute", "@default"), success());
    host.finish().unwrap();
    // No pactl installed at all.
    let mut host = Host::with_plugin(AudioPlugin::new(None, quick()));
    assert_eq!(
        host.invoke("output", "mute", "@default"),
        failure("unavailable")
    );
    host.finish().unwrap();
}

#[test]
fn a_failure_with_the_server_up_is_failed_not_unavailable() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    server.fail_mutations(true);
    assert_eq!(host.invoke("output", "mute", "sink-a"), failure("failed"));
    assert_eq!(
        host.invoke("microphone", "mute", "@default"),
        failure("failed")
    );
    // Garbage where a listing should be is a failure too, not a missing device.
    server.fail_mutations(false);
    server.garbage(true);
    assert_eq!(host.invoke("output", "mute", "sink-a"), failure("failed"));
    host.finish().unwrap();
}

#[test]
fn the_default_alias_with_no_default_device_is_unavailable() {
    let server = FakeServer::new();
    server.set_default(Kind::Output, "");
    let mut host = Host::start(&server, quick());
    assert_eq!(
        host.invoke("output", "mute", "@default"),
        failure("unavailable")
    );
    host.finish().unwrap();
}

#[test]
fn a_hung_pactl_times_out_without_blocking_other_actions() {
    let server = FakeServer::new();
    server.hang(&["set-sink-mute sink-a"]);
    let mut host = Host::start(&server, quick());
    let started = Instant::now();
    let hung = host.ask_invoke("output", "mute", "sink-a");
    // While that one hangs, another action completes at once and the loop keeps answering.
    assert_eq!(host.invoke("microphone", "mute", "@default"), success());
    assert_eq!(host.invoke("output", "mute", "sink-b"), success());
    assert_eq!(
        host.options("action", "output", "device")["availability"],
        "available"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "others waited for the hung one: {:?}",
        started.elapsed()
    );
    assert_eq!(
        host.outcome_of(hung, Duration::from_secs(5)),
        Some(failure("timed_out"))
    );
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "the deadline is the configured one"
    );
    assert!(
        started.elapsed() < Duration::from_millis(1800),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(server.hangs_started(), 1);
    assert!(
        eventually(Duration::from_secs(2), || server.live_hung() == 0),
        "the hung child was killed"
    );
    assert!(
        !server.is_muted(Kind::Output, "sink-a"),
        "the hung command never took effect"
    );
    host.finish().unwrap();
}

#[test]
fn parameters_the_host_should_have_rejected_are_invalid_parameters() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    assert_eq!(
        host.invoke("output", "explode", "sink-a"),
        failure("invalid_parameters")
    );
    assert_eq!(
        host.invoke("output", "mute", ""),
        failure("invalid_parameters")
    );
    assert_eq!(
        host.invoke("speakers", "mute", "sink-a"),
        failure("invalid_parameters")
    );
    host.send(
        90,
        json!({"kind":"invoke","action":"output","parameters":{"schema_version":1,"values":{"operation":"mute"}}}),
    );
    let reply = host
        .take(
            |f| f["correlation"]["request"] == 90,
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply["message"]["outcome"], failure("invalid_parameters"));
    assert_eq!(server.mutating_calls(), Vec::<String>::new());
    host.finish().unwrap();
}

#[test]
fn children_get_a_minimal_environment_and_nothing_else() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    assert_eq!(host.invoke("output", "mute", "sink-a"), success());
    let subscription = host.subscribe("output", "@default");
    host.wait_visual(
        subscription,
        "current",
        Some("Muted"),
        Duration::from_secs(5),
    )
    .unwrap();
    let runs = server.environments();
    assert!(
        runs.len() >= 5,
        "listings, a command and the monitor all ran: {runs:?}"
    );
    // The test process has a full environment; the plugin hands on only the runtime directory.
    assert!(
        runs.iter().all(|names| names == "XDG_RUNTIME_DIR"),
        "{runs:?}"
    );
    host.finish().unwrap();
}

#[test]
fn output_beyond_the_cap_is_refused_and_the_child_ended() {
    let server = FakeServer::new();
    server.flood(true);
    let mut host = Host::start(&server, quick());
    let started = Instant::now();
    assert_eq!(host.invoke("output", "mute", "sink-a"), failure("failed"));
    assert_eq!(
        host.options("action", "output", "device"),
        json!({"availability":"unavailable"})
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    assert!(server.mutating_calls().is_empty());
    host.finish().unwrap();
}

#[test]
fn a_microphone_default_that_is_a_monitor_or_missing_is_unavailable_and_nothing_is_muted() {
    let server = FakeServer::new();
    let mut host = Host::start(&server, quick());
    // The default source points at what a sink plays: not a microphone, and the visual says so.
    server.set_default(Kind::Microphone, "sink-a.monitor");
    for operation in ["toggle", "mute", "unmute"] {
        assert_eq!(
            host.invoke("microphone", operation, "@default"),
            failure("unavailable"),
            "{operation}"
        );
    }
    assert_eq!(
        server.mutating_calls(),
        Vec::<String>::new(),
        "nothing was written"
    );
    assert!(
        !server.is_muted(Kind::Output, "sink-a"),
        "the sink behind the monitor is untouched"
    );
    // No default source at all.
    server.set_default(Kind::Microphone, "");
    assert_eq!(
        host.invoke("microphone", "mute", "@default"),
        failure("unavailable")
    );
    assert_eq!(server.mutating_calls(), Vec::<String>::new());
    host.finish().unwrap();
}

#[test]
fn a_microphone_default_is_resolved_to_its_name_before_acting() {
    let server = FakeServer::new();
    server.set_default(Kind::Microphone, "mic-b");
    let mut host = Host::start(&server, quick());
    assert_eq!(host.invoke("microphone", "mute", "@default"), success());
    assert!(server.is_muted(Kind::Microphone, "mic-b"));
    assert!(!server.is_muted(Kind::Microphone, "mic-a"));
    assert_eq!(server.mutating_calls(), ["set-source-mute mic-b 1"]);
    // The output keeps asking the server for its default, which cannot be a monitor.
    assert_eq!(host.invoke("output", "mute", "@default"), success());
    assert_eq!(
        server.mutating_calls().last().unwrap(),
        "set-sink-mute @DEFAULT_SINK@ 1"
    );
    host.finish().unwrap();
}
