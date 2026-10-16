//! Application identity of MPRIS bus names and the choice among several instances.
use rad_plugin_media::names::{app_identity, first_instance};

const BRAVE: &str = "org.mpris.MediaPlayer2.brave.instance4242";
const STRAWBERRY: &str = "org.mpris.MediaPlayer2.strawberry";

#[test]
fn the_identity_is_the_name_after_the_mpris_prefix_without_an_instance_suffix() {
    assert_eq!(app_identity(BRAVE), Some("brave"));
    assert_eq!(app_identity(STRAWBERRY), Some("strawberry"));
    assert_eq!(app_identity("org.mpris.MediaPlayer2.vlc"), Some("vlc"));
    assert_eq!(
        app_identity("org.mpris.MediaPlayer2.org_kde.player"),
        Some("org_kde.player"),
        "dots inside the name stay"
    );
}

#[test]
fn only_a_dot_instance_and_digits_suffix_is_removed() {
    let identity =
        |rest: &str| app_identity(&format!("org.mpris.MediaPlayer2.{rest}")).map(str::to_owned);
    assert_eq!(identity("brave.instance7"), Some("brave".into()));
    for kept in [
        "brave.instance",
        "brave.instance12a",
        "brave.instance_1_2",
        "brave.instances3",
        "braveinstance3",
        "brave.instance3.extra",
        "instance5",
    ] {
        assert_eq!(identity(kept), Some(kept.into()), "{kept}");
    }
}

#[test]
fn names_that_are_not_mpris_players_have_no_identity() {
    for other in [
        "",
        "org.mpris.MediaPlayer2",
        "org.mpris.MediaPlayer2.",
        ":1.69",
        "org.freedesktop.DBus",
        "org.mpris.MediaPlayer3.brave",
        "org.mpris.mediaplayer2.brave",
    ] {
        assert_eq!(app_identity(other), None, "{other:?}");
    }
}

#[test]
fn the_first_instance_is_the_first_name_in_lexicographic_order() {
    let names = [
        "org.mpris.MediaPlayer2.brave.instance2",
        "org.mpris.MediaPlayer2.brave.instance10",
        "org.mpris.MediaPlayer2.strawberry",
        "org.mpris.MediaPlayer2.brave.instance4242",
    ];
    // Text order, not numeric: "instance10" < "instance2" < "instance4242".
    assert_eq!(
        first_instance(names, "brave"),
        Some("org.mpris.MediaPlayer2.brave.instance10")
    );
    // The same answer whatever order the bus listed them in.
    let mut reversed = names;
    reversed.reverse();
    assert_eq!(
        first_instance(reversed, "brave"),
        first_instance(names, "brave")
    );
    assert_eq!(first_instance(names, "strawberry"), Some(STRAWBERRY));
}

#[test]
fn a_plain_name_comes_before_its_numbered_instances() {
    let names = [
        "org.mpris.MediaPlayer2.brave.instance1",
        "org.mpris.MediaPlayer2.brave",
    ];
    assert_eq!(
        first_instance(names, "brave"),
        Some("org.mpris.MediaPlayer2.brave")
    );
}

#[test]
fn only_names_of_exactly_that_application_match() {
    let names = [
        "org.mpris.MediaPlayer2.bravest",
        "org.mpris.MediaPlayer2.brave2",
        "org.mpris.MediaPlayer2.brave.instance1.extra",
        "org.mpris.MediaPlayer2.Brave",
        ":1.5",
        "org.freedesktop.DBus",
    ];
    assert_eq!(first_instance(names, "brave"), None);
    assert_eq!(first_instance(names, ""), None);
    assert_eq!(first_instance([], "brave"), None);
}
