//! Visual subscriptions: which devices are watched, what each shows, and the monitor that keeps
//! it current.
use crate::{Pactl, Selector, Snapshot, Timing, devices::Kind, monitor::Monitor, server::Failure};
use rad_plugin_sdk::{Subscription, SubscriptionId, VisualContent, VisualState};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

const ON_COLOUR: [u8; 4] = [40, 167, 69, 255];
const MUTED_COLOUR: [u8; 4] = [220, 53, 69, 255];

enum Cache {
    /// No picture yet.
    Loading,
    Ready(Snapshot),
    /// The server cannot be reached.
    Down,
}

struct Entry {
    subscription: Subscription,
    kind: Kind,
    selector: Selector,
}

struct State {
    entries: Vec<Entry>,
    /// What the monitor last saw; meaningless while no monitor runs.
    cache: Cache,
    /// Whether the monitor is wanted: there is at least one entry.
    watching: bool,
    /// The epoch the monitor was last started in. A picture from an earlier one is dropped.
    epoch: u64,
}

impl State {
    /// Publishes a newly observed picture; the SDK renews unchanged states without payload.
    fn show(&mut self) {
        for entry in &mut self.entries {
            let Some(state) = visual_state(entry.kind, &entry.selector, &self.cache) else {
                continue;
            };
            entry.subscription.update(state);
        }
    }
}

fn visual_state(kind: Kind, selector: &Selector, cache: &Cache) -> Option<VisualState> {
    match cache {
        Cache::Loading => None,
        Cache::Down => Some(VisualState::Unavailable),
        Cache::Ready(snapshot) => Some(match snapshot.resolve(kind, selector) {
            Some(device) => {
                let (text, color) = if device.muted {
                    ("Muted", MUTED_COLOUR)
                } else {
                    ("On", ON_COLOUR)
                };
                VisualState::Current {
                    content: VisualContent {
                        text: Some(text.into()),
                        color: Some(color),
                        ..VisualContent::default()
                    },
                }
            }
            None => VisualState::Unavailable,
        }),
    }
}

/// The visual subscriptions of one plugin, and the monitor that is wanted only while there are any.
pub(crate) struct Watch {
    /// `None` when `pactl` is not installed.
    monitor: Option<Monitor>,
    state: Mutex<State>,
}

impl Watch {
    pub(crate) fn new(pactl: Option<Pactl>, timing: Timing) -> Arc<Self> {
        Arc::new_cyclic(|this: &Weak<Watch>| {
            let this = this.clone();
            let monitor = pactl.map(|pactl| {
                let publish = Box::new(move |epoch, picture| {
                    if let Some(watch) = this.upgrade() {
                        watch.apply(epoch, picture);
                    }
                });
                Monitor::new(pactl, timing, publish)
            });
            Self {
                monitor,
                state: Mutex::new(State {
                    entries: Vec::new(),
                    cache: Cache::Loading,
                    watching: false,
                    epoch: 0,
                }),
            }
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the monitor's latest picture, unless it began before the monitor was last started.
    fn apply(&self, epoch: u64, picture: Result<Snapshot, Failure>) {
        let mut state = self.lock();
        if !state.watching || state.epoch != epoch {
            return;
        }
        state.cache = picture.map_or(Cache::Down, Cache::Ready);
        state.show();
    }

    /// Starts showing `subscription`; the first one starts the monitor. A subscription that
    /// cannot work is shown unavailable and not tracked.
    pub(crate) fn add(&self, subscription: Subscription) {
        let (Some(kind), Some(selector), Some(monitor)) = (
            Kind::from_definition(subscription.source()),
            subscription
                .parameters()
                .text("device")
                .and_then(Selector::from_value),
            &self.monitor,
        ) else {
            subscription.update(VisualState::Unavailable);
            return;
        };
        let mut state = self.lock();
        state.entries.push(Entry {
            subscription: subscription.clone(),
            kind,
            selector,
        });
        if state.watching {
            // An add is not an observation: only the reader may assert freshness.
            monitor.refresh();
            return;
        }
        match monitor.start() {
            Some(epoch) => {
                state.watching = true;
                state.epoch = epoch;
            }
            None => {
                state.entries.pop();
                subscription.update(VisualState::Unavailable);
            }
        }
    }

    /// Stops showing a subscription; the last one stops the monitor.
    pub(crate) fn remove(&self, id: SubscriptionId) {
        let mut state = self.lock();
        state.entries.retain(|entry| entry.subscription.id() != id);
        if state.entries.is_empty() && state.watching {
            if let Some(monitor) = &self.monitor {
                monitor.stop();
            }
            state.watching = false;
            // The next time starts from nothing, not from a picture that went stale.
            state.cache = Cache::Loading;
        }
    }
}
