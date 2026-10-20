//! The plugin proper: the SDK's `Plugin` on top of actions, options and visual watching.
use crate::{
    Pactl, Timing, actions,
    devices::{Kind, options_for},
    server,
    watch::Watch,
};
use rad_plugin_sdk::{
    ChoiceList, Config, Invoke, OptionsQuery, Outcome, Plugin, Reply, Subscription, SubscriptionId,
    Version,
};
use std::{sync::Arc, time::Instant};

/// Identity of the plugin, as in `manifest.json`.
pub fn config() -> Config {
    Config::new(
        "org.rad.audio",
        Version {
            major: 0,
            minor: 2,
            patch: 0,
        },
        1..=1,
    )
    .capabilities(vec!["dynamic_options".into(), "visual_freshness".into()])
}

pub struct AudioPlugin {
    shared: Arc<Shared>,
}

struct Shared {
    /// `None` when `pactl` is not installed: everything reports unavailable.
    pactl: Option<Pactl>,
    timing: Timing,
    watch: Arc<Watch>,
}

impl AudioPlugin {
    pub fn new(pactl: Option<Pactl>, timing: Timing) -> Self {
        let watch = Watch::new(pactl.clone(), timing.clone());
        Self {
            shared: Arc::new(Shared {
                pactl,
                timing,
                watch,
            }),
        }
    }
}

/// Runs `job` on its own thread. If the thread cannot start, the job is dropped, which answers
/// whatever reply it holds with its failure.
fn spawn(job: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("audio-job".into())
        .spawn(job);
}

impl Shared {
    fn device_options(&self, query: &OptionsQuery) -> ChoiceList {
        let (Some(kind), "device", Some(pactl)) = (
            Kind::from_definition(&query.definition),
            query.field.as_str(),
            &self.pactl,
        ) else {
            return ChoiceList::Unavailable;
        };
        let deadline = Instant::now() + self.timing.options_deadline;
        match server::snapshot(pactl, deadline) {
            Ok(snapshot) => options_for(kind, &snapshot),
            Err(_) => ChoiceList::Unavailable,
        }
    }
}

impl Plugin for AudioPlugin {
    fn invoke(&self, call: Invoke, reply: Reply<Outcome>) {
        let shared = Arc::clone(&self.shared);
        spawn(move || {
            let deadline = Instant::now() + shared.timing.action_deadline;
            reply.send(actions::perform(shared.pactl.as_ref(), &call, deadline));
        });
    }

    fn options(&self, query: OptionsQuery, reply: Reply<ChoiceList>) {
        let shared = Arc::clone(&self.shared);
        spawn(move || reply.send(shared.device_options(&query)));
    }

    fn subscribe(&self, subscription: Subscription) {
        self.shared.watch.add(subscription);
    }

    fn unsubscribe(&self, subscription: SubscriptionId) {
        self.shared.watch.remove(subscription);
    }
}
