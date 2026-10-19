use crate::polling::{Poller, Sample};
use rad_plugin_sdk::*;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub fn config() -> Config {
    Config::new(
        "org.rad.metrics",
        Version {
            major: 0,
            minor: 1,
            patch: 0,
        },
        2..=2,
    )
}
const SOURCES: [&str; 4] = ["cpu", "ram", "gpu", "vram"];
struct Shared {
    subscriptions: HashMap<SubscriptionId, Subscription>,
    states: [VisualState; 4],
}
pub struct MetricsPlugin {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}
impl MetricsPlugin {
    pub fn with_reader(read: impl FnMut() -> Sample + Send + 'static) -> std::io::Result<Self> {
        Self::with_poller(Poller::new(read)?)
    }
    pub(crate) fn with_poller(mut poller: Poller) -> std::io::Result<Self> {
        let shared = Arc::new(Mutex::new(Shared {
            subscriptions: HashMap::new(),
            states: std::array::from_fn(|_| VisualState::Unavailable),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let state = shared.clone();
        let stopped = stop.clone();
        std::thread::Builder::new()
            .name("metrics-publisher".into())
            .spawn(move || {
                let origin = Instant::now();
                while !stopped.load(Ordering::Acquire) {
                    let active = !state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .subscriptions
                        .is_empty();
                    let states = poller.tick(origin.elapsed(), active);
                    let updates = {
                        let mut shared = state.lock().unwrap_or_else(PoisonError::into_inner);
                        let mut updates = Vec::new();
                        for subscription in shared.subscriptions.values() {
                            if let Some(index) =
                                SOURCES.iter().position(|s| *s == subscription.source())
                                && states[index] != shared.states[index]
                            {
                                updates.push((subscription.clone(), states[index].clone()));
                            }
                        }
                        shared.states = states;
                        updates
                    };
                    for (subscription, state) in updates {
                        subscription.update(state);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })?;
        Ok(Self { shared, stop })
    }
}
impl Drop for MetricsPlugin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
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
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        // SDK also caps at 64; retain the same bound for direct trait callers.
        if shared.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return s.update(VisualState::Unavailable);
        }
        // Serialize this initial publication with publisher's state capture.
        s.update(shared.states[index].clone());
        shared.subscriptions.insert(s.id(), s);
    }
    fn unsubscribe(&self, id: SubscriptionId) {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .subscriptions
            .remove(&id);
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
