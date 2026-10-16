//! The `now_playing` visual: what the button shows, and that it follows the player on its own.
mod support;
use image::{Rgba, RgbaImage};
use rad_plugin_media::Timing;
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};
use support::*;

const BRAVE: &str = "org.mpris.MediaPlayer2.brave.instance4242";
const STRAWBERRY: &str = "org.mpris.MediaPlayer2.strawberry";
/// A change must reach the host within one heartbeat (the SDK's is at most 500 ms).
const HEARTBEAT: Duration = Duration::from_millis(500);

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// A solid-colour PNG in a fresh directory, named like the covers players leave about. The
/// directory goes away with the value, so it is kept for as long as the player may read it.
struct Art {
    dir: Scratch,
}

impl Art {
    fn new(colour: [u8; 4]) -> Self {
        let art = Self {
            dir: scratch_dir("art", false),
        };
        RgbaImage::from_pixel(200, 200, Rgba(colour))
            .save(art.path())
            .unwrap();
        art
    }

    fn path(&self) -> PathBuf {
        self.dir.join("cover.png")
    }

    fn url(&self) -> String {
        format!("file://{}", self.path().display())
    }
}

/// The centre pixel of the JPEG a visual carries.
fn centre(visual: &Visual) -> [u8; 3] {
    let image = image::load_from_memory_with_format(
        &visual.jpeg().expect("a cover"),
        image::ImageFormat::Jpeg,
    )
    .unwrap()
    .to_rgb8();
    assert_eq!((image.width(), image.height()), (96, 96));
    image.get_pixel(48, 48).0
}

#[test]
fn a_cover_is_shown_as_a_small_jpeg_of_the_right_size() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let art = Art::new([30, 90, 220, 255]);
    player
        .command(&format!("art {}", art.url()))
        .command("status Playing")
        .sync();
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("strawberry");
    let (_, visual) = host
        .wait_visual(subscription, |v| v.jpeg_hex.is_some(), secs(5))
        .expect("a cover");
    assert_eq!(visual.availability, "current");
    assert!(visual.jpeg_hex.as_ref().unwrap().len() <= 16384);
    let [red, green, blue] = centre(&visual);
    assert!(
        blue > 180 && red < 80 && green < 130,
        "{:?}",
        [red, green, blue]
    );
    assert_eq!(visual.text, None, "the cover is the whole picture");
    host.finish().unwrap();
}

#[test]
fn without_a_cover_the_state_is_shown_as_text_and_colour() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("strawberry");
    let (_, stopped) = host
        .wait_visual(subscription, |v| v.is("current", Some("Stopped")), secs(5))
        .unwrap();
    assert_eq!(stopped.jpeg_hex, None);
    let mut colours = vec![stopped.color.expect("a colour")];
    for state in ["Playing", "Paused"] {
        player.command(&format!("status {state}"));
        let (_, visual) = host
            .wait_visual(subscription, |v| v.is("current", Some(state)), HEARTBEAT)
            .unwrap_or_else(|seen| panic!("never {state}: {seen:?}"));
        colours.push(visual.color.expect("a colour"));
    }
    colours.sort();
    colours.dedup();
    assert_eq!(colours.len(), 3, "each state has its own colour");
    assert!(colours.iter().all(|c| c[3] == 255));
    host.finish().unwrap();
}

#[test]
fn a_cover_that_cannot_be_used_leaves_the_text() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let notes = scratch_dir("art", false);
    let not_a_picture = notes.join("notes.txt");
    fs::write(&not_a_picture, "not a picture").unwrap();
    let good = Art::new([200, 0, 0, 255]);
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("strawberry");
    host.wait_state(subscription, "current", Some("Stopped"), secs(5))
        .unwrap();
    for url in [
        "https://example.invalid/cover.jpg".to_owned(),
        "http://example.invalid/cover.jpg".to_owned(),
        "file://example.invalid/cover.jpg".to_owned(),
        "data:image/png;base64,AAAA".to_owned(),
        "file:///nowhere/cover.png".to_owned(),
        format!("file://{}", not_a_picture.display()),
    ] {
        // A good cover first, so that "no cover" has to be a change.
        player
            .command(&format!("art {}", good.url()))
            .command("status Paused");
        host.wait_visual(subscription, |v| v.jpeg_hex.is_some(), HEARTBEAT)
            .unwrap();
        player.command(&format!("art {url}"));
        let (_, visual) = host
            .wait_visual(subscription, |v| v.jpeg_hex.is_none(), HEARTBEAT)
            .unwrap_or_else(|seen| panic!("{url} still shows a cover: {seen:?}"));
        assert!(visual.is("current", Some("Paused")), "{url}: {visual:?}");
    }
    host.finish().unwrap();
}

#[test]
fn changes_made_outside_reach_the_host_within_one_heartbeat() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("strawberry");
    host.wait_state(subscription, "current", Some("Stopped"), secs(5))
        .unwrap();
    for state in ["Playing", "Paused", "Stopped", "Playing"] {
        host.discard_pending();
        player.command(&format!("status {state}"));
        let took = host
            .wait_state(subscription, "current", Some(state), HEARTBEAT)
            .unwrap_or_else(|seen| panic!("never {state}: {seen:?}"));
        assert!(took <= HEARTBEAT, "{took:?}");
    }
    // A new cover, and then no cover.
    let (first, second) = (Art::new([220, 20, 20, 255]), Art::new([20, 220, 20, 255]));
    host.discard_pending();
    player.command(&format!("art {}", first.url()));
    let (_, red) = host
        .wait_visual(subscription, |v| v.jpeg_hex.is_some(), HEARTBEAT)
        .unwrap();
    assert!(centre(&red)[0] > 150);
    host.discard_pending();
    player.command(&format!("art {}", second.url()));
    let (_, green) = host
        .wait_visual(
            subscription,
            |v| v.jpeg_hex.is_some() && v != &red,
            HEARTBEAT,
        )
        .unwrap();
    assert!(centre(&green)[1] > 150);
    host.discard_pending();
    player.command("art");
    host.wait_state(subscription, "current", Some("Playing"), HEARTBEAT)
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn a_cover_file_replaced_under_the_same_name_is_shown_again() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let art = Art::new([220, 20, 20, 255]);
    player.command(&format!("art {}", art.url())).sync();
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("strawberry");
    let (_, red) = host
        .wait_visual(subscription, |v| v.jpeg_hex.is_some(), secs(5))
        .unwrap();
    assert!(centre(&red)[0] > 150);
    // Same URL, new picture (a different size, so that the file certainly differs); the player
    // announces the track again.
    RgbaImage::from_pixel(300, 300, Rgba([20, 20, 220, 255]))
        .save(art.path())
        .unwrap();
    host.discard_pending();
    player.command("title next track");
    let (_, blue) = host
        .wait_visual(
            subscription,
            |v| v.jpeg_hex.is_some() && v != &red,
            HEARTBEAT,
        )
        .unwrap();
    assert!(centre(&blue)[2] > 150);
    host.finish().unwrap();
}

#[test]
fn a_player_that_starts_or_quits_is_followed() {
    let bus = PrivateBus::start();
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("brave");
    // Not running: unavailable, with no content.
    let (_, absent) = host.wait_visual(subscription, |_| true, secs(5)).unwrap();
    assert_eq!(absent.availability, "unavailable");
    assert_eq!((absent.text, absent.jpeg_hex), (None, None));

    host.discard_pending();
    let mut player = FakePlayer::start(&bus, BRAVE);
    player.command("status Playing").sync();
    let took = host
        .wait_state(subscription, "current", Some("Playing"), HEARTBEAT)
        .unwrap_or_else(|seen| panic!("{seen:?}"));
    assert!(took <= HEARTBEAT);

    host.discard_pending();
    player.quit();
    host.wait_state(subscription, "unavailable", None, HEARTBEAT)
        .unwrap();

    // And it can come back.
    host.discard_pending();
    let _again = FakePlayer::start(&bus, BRAVE);
    host.wait_state(subscription, "current", Some("Stopped"), HEARTBEAT)
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn only_the_chosen_application_is_shown() {
    // With another player running, a missing app is not replaced by it.
    let bus = PrivateBus::start();
    let mut strawberry = FakePlayer::start(&bus, STRAWBERRY);
    strawberry.command("status Playing").sync();
    let mut host = Host::start(Some(bus.address()), quick());
    let brave = host.subscribe("brave");
    let berry = host.subscribe("strawberry");
    host.wait_state(berry, "current", Some("Playing"), secs(5))
        .unwrap();
    host.wait_state(brave, "unavailable", None, secs(5))
        .unwrap();

    // The other way round too: a change in one never shows in the other.
    let mut brave_player = FakePlayer::start(&bus, BRAVE);
    brave_player.command("status Paused").sync();
    host.wait_state(brave, "current", Some("Paused"), HEARTBEAT)
        .unwrap();
    host.discard_pending();
    strawberry.command("status Stopped");
    host.wait_state(berry, "current", Some("Stopped"), HEARTBEAT)
        .unwrap();
    let deadline = Instant::now() + Duration::from_millis(700);
    while let Some(visual) =
        host.next_visual(brave, deadline.saturating_duration_since(Instant::now()))
    {
        assert!(visual.is("current", Some("Paused")), "{visual:?}");
    }
    host.finish().unwrap();
}

#[test]
fn with_several_instances_the_first_one_found_is_shown() {
    // Two matching players: the first by name is the one shown.
    let bus = PrivateBus::start();
    let mut later = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.brave.instance9");
    let mut first = FakePlayer::start(&bus, "org.mpris.MediaPlayer2.brave.instance10");
    later.command("status Paused").sync();
    first.command("status Playing").sync();
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("brave");
    host.wait_state(subscription, "current", Some("Playing"), secs(5))
        .unwrap();
    host.discard_pending();
    first.quit();
    host.wait_state(subscription, "current", Some("Paused"), HEARTBEAT)
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn two_buttons_for_the_same_application_show_the_same_thing() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let mut host = Host::start(Some(bus.address()), quick());
    let one = host.subscribe("strawberry");
    host.wait_state(one, "current", Some("Stopped"), secs(5))
        .unwrap();
    let two = host.subscribe("strawberry");
    host.wait_state(two, "current", Some("Stopped"), secs(5))
        .unwrap();
    host.discard_pending();
    player.command("status Playing");
    host.wait_state(one, "current", Some("Playing"), HEARTBEAT)
        .unwrap();
    host.wait_state(two, "current", Some("Playing"), HEARTBEAT)
        .unwrap();
    // Ending one leaves the other running.
    host.unsubscribe(one);
    host.discard_pending();
    player.command("status Paused");
    host.wait_state(two, "current", Some("Paused"), HEARTBEAT)
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn nothing_is_read_while_nothing_changes_and_the_last_state_keeps_being_repeated() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("strawberry");
    host.wait_state(subscription, "current", Some("Stopped"), secs(5))
        .unwrap();
    // Starting up looks twice (once when the button appears, once more when the listening is in
    // place); let that finish, and count from there.
    std::thread::sleep(Duration::from_millis(400));
    let reads = player.reads_of("PlaybackStatus");
    host.discard_pending();
    let mut repeats = 0;
    let end = Instant::now() + Duration::from_millis(1400);
    while let Some(visual) =
        host.next_visual(subscription, end.saturating_duration_since(Instant::now()))
    {
        assert!(visual.is("current", Some("Stopped")));
        repeats += 1;
    }
    assert!(
        repeats >= 2,
        "the heartbeat repeated the state {repeats} times"
    );
    assert_eq!(
        player.reads_of("PlaybackStatus"),
        reads,
        "the player was polled"
    );
    host.finish().unwrap();
}

#[test]
fn a_player_that_stops_answering_is_marked_stale_and_the_others_carry_on() {
    let bus = PrivateBus::start();
    let mut stuck = FakePlayer::start(&bus, BRAVE);
    let mut fine = FakePlayer::start(&bus, STRAWBERRY);
    stuck.command("status Playing").sync();
    let mut host = Host::start(Some(bus.address()), quick());
    let hung = host.subscribe("brave");
    let healthy = host.subscribe("strawberry");
    host.wait_state(hung, "current", Some("Playing"), secs(5))
        .unwrap();
    host.wait_state(healthy, "current", Some("Stopped"), secs(5))
        .unwrap();

    stuck.freeze();
    host.discard_pending();
    fine.command("status Playing"); // any change makes every visual look again
    host.wait_state(healthy, "current", Some("Playing"), HEARTBEAT)
        .expect("the healthy one is not held up");
    // What was known stays, but it is no longer presented as current.
    host.wait_state(hung, "stale", Some("Playing"), secs(3))
        .unwrap();

    stuck.thaw();
    host.discard_pending();
    fine.command("status Paused");
    host.wait_state(hung, "current", Some("Playing"), secs(3))
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn a_player_that_froze_without_a_word_is_marked_stale_by_the_slow_check() {
    // No signal ever reaches the plugin from a frozen player, and nothing else on the bus changes.
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    player.command("status Playing").sync();
    let timing = Timing {
        recheck: Duration::from_millis(400),
        ..quick()
    };
    let mut host = Host::start(Some(bus.address()), timing.clone());
    let subscription = host.subscribe("strawberry");
    host.wait_state(subscription, "current", Some("Playing"), secs(5))
        .unwrap();
    // Starting up looks twice (see below); freeze only once the player has been left alone.
    std::thread::sleep(Duration::from_millis(250));

    player.freeze();
    // The check comes, the player does not answer it in time: what was known stays, as stale.
    let within = timing.recheck + timing.refresh_deadline + Duration::from_millis(1500);
    host.wait_state(subscription, "stale", Some("Playing"), within)
        .unwrap_or_else(|seen| panic!("never stale: {seen:?}"));

    player.thaw();
    host.wait_state(subscription, "current", Some("Playing"), within)
        .unwrap_or_else(|seen| panic!("never current again: {seen:?}"));
    host.finish().unwrap();
}

#[test]
fn the_slow_check_leaves_a_healthy_player_as_it_is() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    let timing = Timing {
        recheck: Duration::from_millis(150),
        ..quick()
    };
    let mut host = Host::start(Some(bus.address()), timing);
    let subscription = host.subscribe("strawberry");
    host.wait_state(subscription, "current", Some("Stopped"), secs(5))
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let reads = player.reads_of("PlaybackStatus");
    host.discard_pending();
    let end = Instant::now() + Duration::from_millis(1200);
    while let Some(visual) =
        host.next_visual(subscription, end.saturating_duration_since(Instant::now()))
    {
        assert!(visual.is("current", Some("Stopped")), "{visual:?}");
    }
    // About eight checks fit; a few are enough to show that they happen.
    let checks = player.reads_of("PlaybackStatus") - reads;
    assert!((3..=12).contains(&checks), "{checks} checks in 1.2 s");
    host.finish().unwrap();
}

#[test]
fn a_player_that_dies_while_it_is_being_read_is_unavailable_and_never_stale() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    player.command("status Playing").sync();
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("strawberry");
    host.wait_state(subscription, "current", Some("Playing"), secs(5))
        .unwrap();
    // Starting up looks twice; let that finish.
    std::thread::sleep(Duration::from_millis(250));

    // The next change makes the plugin read the player, and the player ends while it is asked.
    // What the plugin shows for that read has to be that the player is gone, not that it is slow.
    player
        .command("die PlaybackStatus")
        .command("status Paused");
    let mut seen = Vec::new();
    let end = Instant::now() + secs(3);
    while let Some(visual) =
        host.next_visual(subscription, end.saturating_duration_since(Instant::now()))
    {
        let gone = visual.availability == "unavailable";
        seen.push(visual);
        if gone {
            break;
        }
    }
    assert!(
        seen.last().is_some_and(|v| v.availability == "unavailable"),
        "{seen:?}"
    );
    assert!(seen.iter().all(|v| v.availability != "stale"), "{seen:?}");
    host.finish().unwrap();
}

#[test]
fn a_player_that_never_answered_is_unavailable() {
    let bus = PrivateBus::start();
    let frozen = FakePlayer::start(&bus, BRAVE);
    frozen.freeze();
    let mut host = Host::start(Some(bus.address()), quick());
    let subscription = host.subscribe("brave");
    host.wait_state(subscription, "unavailable", None, secs(3))
        .unwrap();
    frozen.thaw();
    host.finish().unwrap();
}

#[test]
fn losing_the_bus_is_unavailable_and_a_bus_that_returns_is_followed_again() {
    let mut bus = PrivateBus::start_at_fixed_address();
    let mut host = Host::start(Some(bus.address()), quick());
    let before = FakePlayer::start(&bus, STRAWBERRY);
    let subscription = host.subscribe("strawberry");
    host.wait_state(subscription, "current", Some("Stopped"), secs(5))
        .unwrap();
    host.discard_pending();
    // The player is still there, as far as it knows: the bus is what is gone.
    bus.kill();
    host.wait_state(subscription, "unavailable", None, secs(3))
        .unwrap();
    drop(before);

    bus.restart();
    let mut player = FakePlayer::start(&bus, STRAWBERRY);
    player.command("status Playing").sync();
    host.wait_state(subscription, "current", Some("Playing"), secs(3))
        .unwrap();
    // The signals work again on the new connection.
    host.discard_pending();
    player.command("status Paused");
    host.wait_state(subscription, "current", Some("Paused"), HEARTBEAT)
        .unwrap();
    host.finish().unwrap();
}

#[test]
fn without_a_bus_the_visual_is_unavailable() {
    let mut dead = PrivateBus::start();
    let address = dead.address().to_owned();
    dead.kill();
    for address in [Some(address.as_str()), None] {
        let mut host = Host::start(address, quick());
        let subscription = host.subscribe("brave");
        host.wait_state(subscription, "unavailable", None, secs(3))
            .unwrap();
        host.finish().unwrap();
    }
}

#[test]
fn a_subscription_without_a_usable_player_is_unavailable() {
    let bus = PrivateBus::start();
    let _player = FakePlayer::start(&bus, STRAWBERRY);
    let mut host = Host::start(Some(bus.address()), quick());
    for values in [
        json!({}),
        json!({"player":""}),
        json!({"player":7}),
        json!({"player":"a\nb"}),
    ] {
        host.send(
            900,
            json!({"kind":"subscribe","source":"now_playing","parameters":{"schema_version":1,"values":values}}),
        );
        let frame = host
            .take(
                |f| f["correlation"]["request"] == 900 && f["message"]["kind"] == "visual",
                secs(3),
            )
            .expect("a visual");
        assert_eq!(
            frame["message"]["state"],
            json!({"availability":"unavailable"}),
            "{values}"
        );
        host.unsubscribe(900);
    }
    host.send(
        901,
        json!({"kind":"subscribe","source":"other","parameters":{"schema_version":1,"values":{"player":"strawberry"}}}),
    );
    let frame = host
        .take(
            |f| f["correlation"]["request"] == 901 && f["message"]["kind"] == "visual",
            secs(3),
        )
        .expect("a visual");
    assert_eq!(
        frame["message"]["state"],
        json!({"availability":"unavailable"})
    );
    host.finish().unwrap();
}
