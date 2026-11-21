use crate::{
    polling::{Demand, Observation, Poller, Sample},
    presentation::{Presentation, Source},
};
use rad_plugin_sdk::*;
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, PoisonError},
    time::{Duration, Instant},
};

pub fn config() -> Config {
    Config::new(
        "org.rad.metrics",
        Version {
            major: 0,
            minor: 4,
            patch: 0,
        },
        1..=1,
    )
    .capabilities(vec!["labels".into(), "visual_freshness".into()])
}
struct VisualSubscription {
    subscription: Subscription,
    source: Source,
    presentation: Presentation,
    last: VisualState,
}
impl VisualSubscription {
    fn publish(&mut self, observation: Option<Observation<'_>>, new_sample: bool) {
        let state = match observation {
            Some(sample) if new_sample => sample.state(self.source, &self.presentation),
            Some(sample) if sample.stale => {
                let VisualState::Current { content } = &self.last else {
                    return;
                };
                VisualState::Stale {
                    content: content.clone(),
                }
            }
            None if self.last != VisualState::Unavailable => VisualState::Unavailable,
            _ => return,
        };
        self.subscription.update(state.clone());
        self.last = state;
    }
}

struct Shared {
    subscriptions: HashMap<SubscriptionId, VisualSubscription>,
    poller: Poller,
    origin: Instant,
    demand: Demand,
    stopped: bool,
}
impl Shared {
    fn refresh_demand(&mut self) {
        let sources =
            Source::ALL.map(|source| self.subscriptions.values().any(|s| s.source == source));
        self.demand.set(sources);
    }
    fn publish(&mut self, now: Duration) {
        let previous = self
            .poller
            .observation(now, self.demand)
            .map(|sample| sample.started);
        let observation = self.poller.tick_demand(now, self.demand);
        let new_sample = observation.is_some_and(|sample| Some(sample.started) != previous);
        for subscription in self.subscriptions.values_mut() {
            subscription.publish(observation, new_sample);
        }
    }
}
pub struct MetricsPlugin {
    shared: Arc<(Mutex<Shared>, Condvar)>,
}
impl MetricsPlugin {
    pub fn with_reader(read: impl FnMut() -> Sample + Send + 'static) -> std::io::Result<Self> {
        Self::with_poller(Poller::new(read)?)
    }
    pub(crate) fn with_poller(poller: Poller) -> std::io::Result<Self> {
        let shared = Arc::new((
            Mutex::new(Shared {
                subscriptions: HashMap::new(),
                poller,
                origin: Instant::now(),
                demand: Demand::default(),
                stopped: false,
            }),
            Condvar::new(),
        ));
        let state = shared.clone();
        std::thread::Builder::new()
            .name("metrics-publisher".into())
            .spawn(move || {
                let (lock, wake) = &*state;
                let mut shared = lock.lock().unwrap_or_else(PoisonError::into_inner);
                while !shared.stopped {
                    // Serialize publication with demand changes so a released epoch cannot overtake a new one.
                    let now = shared.origin.elapsed();
                    shared.publish(now);
                    shared = match shared.poller.wake_after(now) {
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
        let Some(source) = Source::from_visual(s.source()) else {
            return s.update(VisualState::Unavailable);
        };
        let Some(presentation) = Presentation::parse(source, s.parameters()) else {
            return s.update(VisualState::Unavailable);
        };
        let mut shared = self.shared.0.lock().unwrap_or_else(PoisonError::into_inner);
        // SDK also caps at 64; retain the same bound for direct trait callers.
        if shared.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return s.update(VisualState::Unavailable);
        }
        // Adding a source starts a new demand epoch, so only an existing source can share
        // a cached observation. The poller also checks for releases it has not seen yet.
        let initial = if shared.demand.sources[source as usize] {
            shared
                .poller
                .observation(shared.origin.elapsed(), shared.demand)
                .map_or(VisualState::Unavailable, |sample| {
                    sample.state(source, &presentation)
                })
        } else {
            VisualState::Unavailable
        };
        s.update(initial.clone());
        shared.subscriptions.insert(
            s.id(),
            VisualSubscription {
                subscription: s,
                source,
                presentation,
                last: initial,
            },
        );
        shared.refresh_demand();
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
