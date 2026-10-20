use crate::polling::{Demand, Poller, Sample};
use rad_plugin_sdk::*;
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, PoisonError},
    time::Instant,
};

pub fn config() -> Config {
    Config::new(
        "org.rad.metrics",
        Version {
            major: 0,
            minor: 1,
            patch: 1,
        },
        2..=2,
    )
}
const SOURCES: [&str; 4] = ["cpu", "ram", "gpu", "vram"];
struct Shared {
    subscriptions: HashMap<SubscriptionId, Subscription>,
    states: [VisualState; 4],
    states_epoch: u64,
    demand: Demand,
    stopped: bool,
}
impl Shared {
    fn refresh_demand(&mut self) {
        let sources = std::array::from_fn(|i| {
            self.subscriptions
                .values()
                .any(|s| s.source() == SOURCES[i])
        });
        self.demand.set(sources);
    }
}
pub struct MetricsPlugin {
    shared: Arc<(Mutex<Shared>, Condvar)>,
}
impl MetricsPlugin {
    pub fn with_reader(read: impl FnMut() -> Sample + Send + 'static) -> std::io::Result<Self> {
        Self::with_poller(Poller::new(read)?)
    }
    pub(crate) fn with_poller(mut poller: Poller) -> std::io::Result<Self> {
        let shared = Arc::new((
            Mutex::new(Shared {
                subscriptions: HashMap::new(),
                states: std::array::from_fn(|_| VisualState::Unavailable),
                states_epoch: 0,
                demand: Demand::default(),
                stopped: false,
            }),
            Condvar::new(),
        ));
        let state = shared.clone();
        std::thread::Builder::new()
            .name("metrics-publisher".into())
            .spawn(move || {
                let origin = Instant::now();
                let (lock, wake) = &*state;
                let mut shared = lock.lock().unwrap_or_else(PoisonError::into_inner);
                while !shared.stopped {
                    let now = origin.elapsed();
                    let states = poller.tick_demand(now, shared.demand);
                    // Serialize publication and subscription changes: no old epoch can overtake
                    // a removal/resubscription. The poller never performs I/O here.
                    for subscription in shared.subscriptions.values() {
                        if let Some(index) =
                            SOURCES.iter().position(|s| *s == subscription.source())
                            && states[index] != shared.states[index]
                        {
                            subscription.update(states[index].clone());
                        }
                    }
                    shared.states = states;
                    shared.states_epoch = shared.demand.epoch;
                    shared = match poller.wake_after(now) {
                        Some(wait) => {
                            wake.wait_timeout(shared, wait)
                                .unwrap_or_else(PoisonError::into_inner)
                                .0
                        }
                        None => wake.wait(shared).unwrap_or_else(PoisonError::into_inner),
                    };
                }
            })?;
        Ok(Self { shared })
    }
}
impl Drop for MetricsPlugin {
    fn drop(&mut self) {
        self.shared
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stopped = true;
        self.shared.1.notify_one();
    }
}
impl Plugin for MetricsPlugin {
    fn subscribe(&self, s: Subscription) {
        let Some(index) = SOURCES.iter().position(|source| *source == s.source()) else {
            return s.update(VisualState::Unavailable);
        };
        if s.parameters().schema_version != 1 || !s.parameters().values.is_empty() {
            return s.update(VisualState::Unavailable);
        }
        let mut shared = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
        // SDK also caps at 64; retain the same bound for direct trait callers.
        if shared.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return s.update(VisualState::Unavailable);
        }
        shared.subscriptions.insert(s.id(), s.clone());
        shared.refresh_demand();
        // A new demand epoch cannot reuse the previous mask's cached Current state.
        let initial = if shared.states_epoch == shared.demand.epoch {
            shared.states[index].clone()
        } else {
            VisualState::Unavailable
        };
        s.update(initial);
        self.shared.1.notify_one();
    }
    fn unsubscribe(&self, id: SubscriptionId) {
        let mut shared = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
        shared.subscriptions.remove(&id);
        shared.refresh_demand();
        self.shared.1.notify_one();
    }
    fn invoke(&self, _: Invoke, r: Reply<Outcome>) {
        r.send(Outcome::Failure {
            code: FailureCode::Unavailable,
        });
    }
    fn options(&self, _: OptionsQuery, r: Reply<ChoiceList>) {
        r.send(ChoiceList::Unavailable);
    }
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod tests;
