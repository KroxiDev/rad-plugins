//! Source-observation freshness, on a private bus with a synthetic player only.
mod support;
use std::time::Duration;
use support::*;

const PLAYER: &str = "org.mpris.MediaPlayer2.strawberry";

#[test]
fn an_unchanged_healthy_source_renews_without_retransmitting_the_cover() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, PLAYER);
    let art = scratch_dir("freshness-art", false);
    let file = art.join("cover.png");
    image::RgbaImage::from_pixel(32, 32, image::Rgba([20, 100, 200, 255]))
        .save(&file)
        .unwrap();
    player
        .command(&format!("art file://{}", file.display()))
        .command("status Paused")
        .sync();
    let mut timing = quick();
    timing.recheck = Duration::from_millis(150);
    let mut host = Host::start(Some(bus.address()), timing);
    let subscription = host.subscribe("strawberry");
    host.wait_visual(
        subscription,
        |v| v.jpeg_hex.is_some(),
        Duration::from_secs(5),
    )
    .unwrap();
    host.discard_pending();
    let before = player.reads_of("PlaybackStatus");
    let renewed = host
        .take(
            |v| {
                v["correlation"]["request"] == subscription
                    && v["message"]["kind"] == "visual_renew"
            },
            Duration::from_secs(3),
        )
        .expect("a fresh observation renews unchanged state");
    assert_eq!(renewed["message"]["valid_for_ms"], 25_000);
    assert_eq!(
        renewed["message"].as_object().unwrap().len(),
        2,
        "renewal carries only its kind and validity, never image/state"
    );
    assert!(
        player.reads_of("PlaybackStatus") > before,
        "renewal must follow an actual source read"
    );
    host.finish().unwrap();
}

#[test]
fn a_blocked_source_reader_cannot_renew_cached_current_content() {
    let bus = PrivateBus::start();
    let mut player = FakePlayer::start(&bus, PLAYER);
    player.command("status Paused").sync();
    let mut timing = quick();
    timing.recheck = Duration::from_millis(50);
    // Deliberately exceed the observation window: no independent SDK timer may
    // pretend that this blocked read is evidence of freshness. Host expiry itself
    // is exercised by rad-plugin-host/tests/freshness_v1.rs with its own clock.
    timing.refresh_deadline = Duration::from_secs(60);
    let mut host = Host::start(Some(bus.address()), timing);
    let subscription = host.subscribe("strawberry");
    host.wait_state(
        subscription,
        "current",
        Some("Paused"),
        Duration::from_secs(5),
    )
    .unwrap();
    player.freeze();
    host.discard_pending();
    let blind = host.take(
        |v| {
            v["correlation"]["request"] == subscription
                && matches!(
                    v["message"]["kind"].as_str(),
                    Some("visual" | "visual_renew")
                )
        },
        Duration::from_millis(1400),
    );
    player.thaw();
    host.finish().unwrap();
    assert!(
        blind.is_none(),
        "a blocked observer published/renewed cached state: {blind:?}"
    );
}
