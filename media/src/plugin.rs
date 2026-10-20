//! The plugin proper: the SDK's `Plugin` on top of options, actions and visual watching.
use crate::{Timing, actions, bus::Bus, options, watch::Watch};
use rad_plugin_sdk::{
    ChoiceList, Config, Invoke, OptionsQuery, Outcome, Plugin, Reply, Subscription, SubscriptionId,
    Version,
};
use std::{sync::Arc, time::Instant};

/// Identity of the plugin, as in `manifest.json`.
pub fn config() -> Config {
    Config::new(
        "org.rad.media",
        Version {
            major: 0,
            minor: 1,
            patch: 1,
        },
        2..=2,
    )
}

pub struct MediaPlugin {
    shared: Arc<Shared>,
}

struct Shared {
    bus: Arc<Bus>,
    timing: Timing,
    watch: Arc<Watch>,
}

impl MediaPlugin {
    /// `bus_address` is the session bus to use; `None` when there is none.
    pub fn new(bus_address: Option<String>, timing: Timing) -> Self {
        let bus = Arc::new(Bus::new(bus_address));
        let watch = Watch::new(Arc::clone(&bus), timing.clone());
        Self {
            shared: Arc::new(Shared { bus, timing, watch }),
        }
    }
}

/// Runs `job` on its own thread. If the thread cannot start, the job is dropped, which answers
/// whatever reply it holds with its failure.
fn spawn(job: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("media-job".into())
        .spawn(job);
}

/// Both the action and the visual choose a player; no other field has options.
fn asks_for_player(query: &OptionsQuery) -> bool {
    matches!(query.definition.as_str(), "control" | "now_playing") && query.field == "player"
}

impl Plugin for MediaPlugin {
    fn invoke(&self, call: Invoke, reply: Reply<Outcome>) {
        let shared = Arc::clone(&self.shared);
        spawn(move || {
            let deadline = Instant::now() + shared.timing.action_deadline;
            reply.send(actions::perform(&shared.bus, &call, deadline));
        });
    }

    fn options(&self, query: OptionsQuery, reply: Reply<ChoiceList>) {
        if !asks_for_player(&query) {
            return reply.send(ChoiceList::Unavailable);
        }
        let shared = Arc::clone(&self.shared);
        spawn(move || {
            let deadline = Instant::now() + shared.timing.options_deadline;
            reply.send(options::player_options(
                &shared.bus,
                &shared.timing,
                deadline,
            ));
        });
    }

    fn subscribe(&self, subscription: Subscription) {
        self.shared.watch.add(subscription);
    }

    fn unsubscribe(&self, subscription: SubscriptionId) {
        self.shared.watch.remove(subscription);
    }
}
