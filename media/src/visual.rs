//! What the `now_playing` visual shows for one application, from what its player says.
use crate::{
    Timing,
    bus::{Bus, Failure},
    cover,
    names::first_instance,
    player::{self, Status},
};
use rad_plugin_sdk::{VisualContent, VisualState};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Instant, SystemTime},
};

const PLAYING: [u8; 4] = [40, 167, 69, 255];
const PAUSED: [u8; 4] = [255, 193, 7, 255];
const STOPPED: [u8; 4] = [108, 117, 125, 255];

/// A cover is made again only if its file changed: the URL, the size and the time of change.
#[derive(PartialEq)]
struct CoverKey {
    path: PathBuf,
    length: u64,
    modified: Option<SystemTime>,
}

/// Reads one application's player and works out what to show. It remembers the cover it made,
/// so that an unrelated change does not decode the picture again.
pub(crate) struct Reader {
    app: String,
    converter: Arc<cover::Converter>,
    cover: Option<(CoverKey, Option<String>)>,
}

impl Reader {
    pub(crate) fn new(app: String, converter: Arc<cover::Converter>) -> Self {
        Self {
            app,
            converter,
            cover: None,
        }
    }

    /// The state to show. `shown` is what is displayed now: if the player cannot be asked, it is
    /// kept but marked stale instead of being passed off as current.
    pub(crate) fn read(
        &mut self,
        bus: &Bus,
        timing: &Timing,
        shown: Option<&VisualState>,
    ) -> VisualState {
        let deadline = Instant::now() + timing.refresh_deadline;
        let stale = || match shown {
            Some(VisualState::Current { content } | VisualState::Stale { content }) => {
                VisualState::Stale {
                    content: content.clone(),
                }
            }
            _ => VisualState::Unavailable,
        };
        let _connection = match bus.connection(deadline) {
            Ok(connection) => connection,
            Err(Failure::Unavailable) => return VisualState::Unavailable,
            Err(_) => return stale(),
        };
        let names = match bus.player_names(deadline) {
            Ok(names) => names,
            Err(Failure::Unavailable) => return VisualState::Unavailable,
            Err(_) => return stale(),
        };
        let Some(name) = first_instance(names.iter().map(String::as_str), &self.app) else {
            return VisualState::Unavailable;
        };
        match player::state(bus, name, deadline) {
            Ok(state) => VisualState::Current {
                content: self.content(&state),
            },
            // It left between the list and the question.
            Err(Failure::Unavailable) => VisualState::Unavailable,
            Err(_) => stale(),
        }
    }

    fn content(&mut self, state: &player::State) -> VisualContent {
        if let Some(jpeg_hex) = self.cover(state.art_url.as_deref()) {
            return VisualContent {
                jpeg_hex: Some(jpeg_hex),
                ..VisualContent::default()
            };
        }
        let (text, color) = match state.status {
            Status::Playing => ("Playing", PLAYING),
            Status::Paused => ("Paused", PAUSED),
            Status::Stopped => ("Stopped", STOPPED),
        };
        VisualContent {
            text: Some(text.into()),
            color: Some(color),
            ..VisualContent::default()
        }
    }

    /// The cover as hex of a JPEG, if the player offers a local picture that can be used.
    fn cover(&mut self, art_url: Option<&str>) -> Option<String> {
        let path = cover::path_of(art_url?).ok()?;
        let metadata = fs::metadata(&path).ok()?;
        let key = CoverKey {
            path,
            length: metadata.len(),
            modified: metadata.modified().ok(),
        };
        if let Some((known, made)) = &self.cover
            && *known == key
        {
            return made.clone();
        }
        let made = self.converter.jpeg_hex(&key.path).ok();
        self.cover = Some((key, made.clone()));
        made
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn a_reader_without_cover_does_not_wait_for_busy_conversion() {
        // The receiver stays alive but never accepts a conversion: sending to it would block.
        let (converter, stalled) = cover::Converter::stalled();
        let (done, result) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut reader = Reader::new("text-player".into(), Arc::new(converter));
            done.send(reader.content(&player::State {
                status: Status::Playing,
                art_url: None,
            }))
            .unwrap();
        });
        let result = result.recv_timeout(Duration::from_secs(2));
        drop(stalled); // Release a regressed sender before joining, even when the assertion fails.
        handle.join().unwrap();
        assert_eq!(
            result
                .expect("text reader waited for cover conversion")
                .text
                .as_deref(),
            Some("Playing")
        );
    }
}
