//! Source-observed visual subscriptions with content-free renewal.
use crate::{Correlation, Message, Parameters, VisualState, writer::Writer};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
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
    settings: Option<Parameters>,
    credentials: Option<crate::Credentials>,
    writer: Writer,
    state: Mutex<State>,
}

/// A live visual subscription. Clones share it, so it can be kept by the plugin and updated from
/// any observing source thread. No independent task renews cached data.
#[derive(Clone)]
pub struct Subscription(Arc<Inner>);

impl Subscription {
    pub(crate) fn new(
        writer: Writer,
        id: SubscriptionId,
        source: String,
        parameters: Parameters,
        settings: Option<Parameters>,
        credentials: Option<crate::Credentials>,
    ) -> Self {
        Self(Arc::new(Inner {
            id,
            source,
            parameters,
            settings,
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

    pub fn settings(&self) -> Option<&Parameters> {
        self.0.settings.as_ref()
    }

    /// Publish from the observing source loop, never from a separate heartbeat.
    /// Equal state renews validity without retransmitting content or image.
    pub fn publish(&self, state: VisualState, valid_for_ms: u64) {
        let state = if state.is_valid() {
            state
        } else {
            VisualState::Unavailable
        };
        let valid_for_ms = valid_for_ms.clamp(1000, 60000);
        let mut inner = locked(&self.0.state);
        if !inner.active {
            return;
        }
        let message = if inner.last.as_ref() == Some(&state) {
            Message::VisualRenew { valid_for_ms }
        } else {
            Message::Visual {
                state: state.clone(),
                valid_for_ms,
            }
        };
        if self.0.writer.send(self.0.id, message).is_ok() {
            inner.last = Some(state);
        }
    }
    /// Default 25-second validity. Calling this asserts a new observation of the source.
    pub fn update(&self, state: VisualState) {
        self.publish(state, 25000);
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
}
