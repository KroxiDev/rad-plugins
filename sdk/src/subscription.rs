//! Visual subscriptions, the table that tracks them and the heartbeat that keeps them fresh.
use crate::{Correlation, Message, Parameters, VisualState, writer::Writer};
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
    time::Duration,
};

/// Identifies a subscription: the correlation of the `subscribe` that created it.
pub type SubscriptionId = Correlation;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct State {
    active: bool,
    last: Option<VisualState>,
}
struct Inner {
    id: SubscriptionId,
    source: String,
    parameters: Parameters,
    credentials: Option<crate::Credentials>,
    writer: Writer,
    state: Mutex<State>,
}

/// A live visual subscription. Clones share it, so it can be kept by the plugin and updated from
/// any thread. The SDK repeats the last state every heartbeat until the host unsubscribes.
#[derive(Clone)]
pub struct Subscription(Arc<Inner>);

impl Subscription {
    pub(crate) fn new(
        writer: Writer,
        id: SubscriptionId,
        source: String,
        parameters: Parameters,
        credentials: Option<crate::Credentials>,
    ) -> Self {
        Self(Arc::new(Inner {
            id,
            source,
            parameters,
            credentials,
            writer,
            state: Mutex::new(State {
                active: true,
                last: None,
            }),
        }))
    }

    pub fn id(&self) -> SubscriptionId {
        self.0.id
    }

    /// The visual source the host subscribed to.
    pub fn source(&self) -> &str {
        &self.0.source
    }

    pub fn credentials(&self) -> Option<&crate::Credentials> {
        self.0.credentials.as_ref()
    }

    pub fn parameters(&self) -> &Parameters {
        &self.0.parameters
    }

    /// False once the host has unsubscribed or the session has ended.
    pub fn is_active(&self) -> bool {
        locked(&self.0.state).active
    }

    /// Publishes `state` now and makes it the one the heartbeat repeats. A state the host would
    /// reject is published as `unavailable` instead. Does nothing once the subscription ended.
    pub fn update(&self, state: VisualState) {
        let state = if state.is_valid() {
            state
        } else {
            VisualState::Unavailable
        };
        let mut inner = locked(&self.0.state);
        if inner.active {
            self.write(&state);
            inner.last = Some(state);
        }
    }

    fn repeat(&self) {
        let inner = locked(&self.0.state);
        if let (true, Some(last)) = (inner.active, &inner.last) {
            self.write(last);
        }
    }

    // The state lock is held by the caller, which keeps a repeat from overtaking a newer update.
    fn write(&self, state: &VisualState) {
        // A failed write means the host is gone; the dispatch loop sees the same and stops.
        let _ = self.0.writer.send(
            self.0.id,
            Message::Visual {
                state: state.clone(),
            },
        );
    }

    pub(crate) fn deactivate(&self) {
        locked(&self.0.state).active = false;
    }
}

pub(crate) enum Refused {
    Full,
    Duplicate,
}

/// The subscriptions of one session, bounded by [`crate::MAX_SUBSCRIPTIONS`].
pub(crate) struct Registry {
    limit: usize,
    subscriptions: Mutex<HashMap<SubscriptionId, Subscription>>,
}
impl Registry {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            subscriptions: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn insert(&self, subscription: &Subscription) -> Result<(), Refused> {
        let mut table = locked(&self.subscriptions);
        if table.contains_key(&subscription.id()) {
            Err(Refused::Duplicate)
        } else if table.len() >= self.limit {
            Err(Refused::Full)
        } else {
            table.insert(subscription.id(), subscription.clone());
            Ok(())
        }
    }

    pub(crate) fn remove(&self, id: SubscriptionId) -> Option<Subscription> {
        let removed = locked(&self.subscriptions).remove(&id);
        if let Some(subscription) = &removed {
            subscription.deactivate();
        }
        removed
    }

    pub(crate) fn drain(&self) -> Vec<Subscription> {
        let all: Vec<_> = locked(&self.subscriptions)
            .drain()
            .map(|(_, s)| s)
            .collect();
        all.iter().for_each(Subscription::deactivate);
        all
    }

    fn repeat_all(&self) {
        // Removal waits for the heartbeat, so it never retains a credential handle
        // after unsubscribe completed.
        for subscription in locked(&self.subscriptions).values() {
            subscription.repeat();
        }
    }
}

/// Repeats the last state of every subscription each `interval`, stopping when dropped.
pub(crate) struct Heartbeat {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}
impl Heartbeat {
    pub(crate) fn start(registry: Arc<Registry>, interval: Duration) -> Self {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let signal = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let (flag, wake) = &*signal;
            loop {
                let mut stopped = locked(flag);
                let deadline = std::time::Instant::now() + interval;
                while !*stopped {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    stopped = wake
                        .wait_timeout(stopped, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                if *stopped {
                    return;
                }
                drop(stopped);
                registry.repeat_all();
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Heartbeat {
    fn drop(&mut self) {
        *locked(&self.stop.0) = true;
        self.stop.1.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
