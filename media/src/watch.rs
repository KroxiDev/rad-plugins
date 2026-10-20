//! Visual subscriptions: which applications are displayed, and the threads that keep them current.
//!
//! One reader thread per displayed application reads its player whenever something may have changed
//! (and now and then anyway, see [`Timing::recheck`]) and publishes the result to every button
//! showing it. One monitor, shared by all readers, listens to the bus and tells them. With nothing
//! displayed there is neither, a moment ([`Timing::linger`]) after the last button left.
//!
//! The threads are bounded, because a reader can be stuck for as long as a player takes to answer,
//! and buttons can come and go faster than that:
//! - An application has one reader at most. It stays for a moment after its last button left, and
//!   is used again by a button that comes back, instead of another being started.
//! - There are [`MAX_READERS`] readers at most. An application that finds them all busy waits,
//!   and is served by the first reader that has nothing else to do.
//! - There is one monitor at most, and it ends only when no application is left.
use crate::{Timing, bus::Bus, cover::Converter, monitor::Monitor, visual::Reader, wake::Wake};
use rad_plugin_sdk::{MAX_SUBSCRIPTIONS, Subscription, SubscriptionId, VisualState, is_label};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
    thread,
};

/// Most readers at once: one for each button the host may have, so the limit never holds back a
/// button that is wanted.
const MAX_READERS: usize = MAX_SUBSCRIPTIONS;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The buttons showing one application, and what they show.
#[derive(Default)]
struct Buttons {
    subscriptions: Vec<Subscription>,
    shown: Option<VisualState>,
    /// Identifies one continuous interval of demand; residual reads belong to their old interval.
    interval: Arc<()>,
}

/// An application the plugin has to do with: it has buttons, or a reader that has not left yet.
struct Group {
    buttons: Arc<Mutex<Buttons>>,
    wake: Arc<Wake>,
    /// A reader is serving it. A group with buttons and no reader is waiting for one.
    served: bool,
}

impl Group {
    fn new() -> Self {
        Self {
            buttons: Arc::default(),
            wake: Arc::new(Wake::new()),
            served: false,
        }
    }

    fn has_buttons(&self) -> bool {
        !locked(&self.buttons).subscriptions.is_empty()
    }
}

/// What a reader thread works on: an application, and what the reader knows of it.
struct Job {
    app: String,
    buttons: Arc<Mutex<Buttons>>,
    wake: Arc<Wake>,
    reader: Reader,
}

impl Job {
    /// Serving `group` from now on: it is read straight away.
    fn of(app: &str, group: &Group, converter: Arc<Converter>) -> Self {
        group.wake.now();
        Self {
            app: app.to_owned(),
            buttons: Arc::clone(&group.buttons),
            wake: Arc::clone(&group.wake),
            reader: Reader::new(app.to_owned(), converter),
        }
    }
}

/// Shows `state` on every button of the group, unless it is already what they show.
fn publish(buttons: &Mutex<Buttons>, interval: &Arc<()>, state: VisualState) {
    let mut buttons = locked(buttons);
    if !Arc::ptr_eq(&buttons.interval, interval) {
        return;
    }
    {
        for subscription in &buttons.subscriptions {
            subscription.update(state.clone());
        }
        buttons.shown = Some(state);
    }
}

/// The body of a reader thread: reads its application until nobody wants it, then goes on with
/// whatever the table gives it, or ends.
fn serve(watch: &Weak<Watch>, bus: &Bus, timing: &Timing, mut job: Job) {
    loop {
        while job.wake.wait(
            timing.settle,
            timing.max_settle,
            timing.recheck,
            timing.linger,
        ) {
            let (shown, interval) = {
                let buttons = locked(&job.buttons);
                (buttons.shown.clone(), Arc::clone(&buttons.interval))
            };
            let state = job.reader.read(bus, timing, shown.as_ref());
            publish(&job.buttons, &interval, state);
        }
        match watch.upgrade().and_then(|watch| watch.next_job(job)) {
            Some(next) => job = next,
            None => return,
        }
    }
}

struct State {
    groups: HashMap<String, Group>,
    /// Exists while there is any group, and a moment after.
    monitor: Option<Monitor>,
    /// Reader threads that are running.
    readers: usize,
    converter: Option<Arc<Converter>>,
}

impl State {
    /// The job of an application that has buttons and no reader, which is now served by the one
    /// that takes the job.
    fn take_waiting(&mut self) -> Option<Job> {
        let (app, group) = self
            .groups
            .iter_mut()
            .find(|(_, group)| !group.served && group.has_buttons())?;
        group.served = true;
        Some(Job::of(
            app,
            group,
            Arc::clone(
                self.converter
                    .get_or_insert_with(|| Arc::new(Converter::default())),
            ),
        ))
    }

    /// With no group left the monitor is told to end, which it does if nothing changes by then.
    fn stop_if_idle(&mut self) {
        if self.groups.is_empty() {
            self.converter = None;
        }
        if self.groups.is_empty()
            && let Some(monitor) = &self.monitor
        {
            monitor.stop();
        }
    }

    /// An application that cannot be followed: its buttons show it, and it is forgotten.
    fn give_up(&mut self, app: &str) {
        if let Some(group) = self.groups.remove(app) {
            for subscription in &locked(&group.buttons).subscriptions {
                subscription.update(VisualState::Unavailable);
            }
        }
    }
}

pub(crate) struct Watch {
    bus: Arc<Bus>,
    timing: Timing,
    state: Mutex<State>,
}

impl Watch {
    pub(crate) fn new(bus: Arc<Bus>, timing: Timing) -> Arc<Self> {
        Arc::new(Self {
            bus,
            timing,
            state: Mutex::new(State {
                groups: HashMap::new(),
                monitor: None,
                readers: 0,
                converter: None,
            }),
        })
    }

    /// Something may have changed: every application is read again.
    fn changed(&self) {
        for group in locked(&self.state).groups.values() {
            group.wake.changed();
        }
    }

    /// The monitor was told to end. It does, unless an application came back meanwhile.
    fn retire_monitor(&self) -> bool {
        let mut state = locked(&self.state);
        let ends = state.groups.is_empty();
        if ends {
            state.monitor = None;
        }
        ends
    }

    fn start_monitor(self: &Arc<Self>) -> Option<Monitor> {
        let (changing, retiring) = (Arc::downgrade(self), Arc::downgrade(self));
        let changed = Arc::new(move || {
            if let Some(watch) = Weak::upgrade(&changing) {
                watch.changed();
            }
        });
        let retire = Arc::new(move || Weak::upgrade(&retiring).is_none_or(|w| w.retire_monitor()));
        Monitor::start(Arc::clone(&self.bus), self.timing.clone(), changed, retire)
    }

    /// A reader found that nobody wanted its application for a while. If buttons came back it goes
    /// on with the application; otherwise the application is forgotten, and the reader goes on
    /// with one that is waiting, or ends.
    fn next_job(&self, job: Job) -> Option<Job> {
        let mut state = locked(&self.state);
        let own = state
            .groups
            .get(&job.app)
            .filter(|group| Arc::ptr_eq(&group.wake, &job.wake));
        if own.is_some_and(Group::has_buttons) {
            job.wake.resume();
            return Some(job);
        }
        if own.is_some() {
            state.groups.remove(&job.app);
        }
        let next = state.take_waiting();
        if next.is_none() {
            state.readers -= 1;
            state.stop_if_idle();
        }
        next
    }

    fn spawn_reader(self: &Arc<Self>, job: Job) -> bool {
        let (watch, bus, timing) = (
            Arc::downgrade(self),
            Arc::clone(&self.bus),
            self.timing.clone(),
        );
        thread::Builder::new()
            .name("media-reader".into())
            .spawn(move || serve(&watch, &bus, &timing, job))
            .is_ok()
    }

    /// Gives a reader to the applications that wait for one, as far as the readers allowed go.
    fn start_readers(self: &Arc<Self>, state: &mut State) {
        while state.readers < MAX_READERS
            && let Some(job) = state.take_waiting()
        {
            let app = job.app.clone();
            if self.spawn_reader(job) {
                state.readers += 1;
            } else {
                state.give_up(&app);
            }
        }
    }

    /// Starts showing `subscription`. One that cannot work is shown unavailable and not tracked.
    pub(crate) fn add(self: &Arc<Self>, subscription: Subscription) {
        let app = match subscription.parameters().text("player") {
            Some(app) if subscription.source() == "now_playing" && is_label(app) => app.to_owned(),
            _ => return subscription.update(VisualState::Unavailable),
        };
        let mut state = locked(&self.state);
        if state.monitor.is_none() {
            state.monitor = self.start_monitor();
        }
        if state.monitor.is_none() {
            // Nothing could keep it current.
            return subscription.update(VisualState::Unavailable);
        }
        let group = state.groups.entry(app).or_insert_with(Group::new);
        group.wake.resume();
        {
            let mut buttons = locked(&group.buttons);
            if buttons.subscriptions.is_empty() {
                // A retained reader is only residual capacity, not evidence of current state.
                buttons.shown = None;
                buttons.interval = Arc::new(());
                group.wake.now();
            }
            // Cached content is not a new observation. Wake the source for this subscriber.
            group.wake.now();
            buttons.subscriptions.push(subscription);
        }
        self.start_readers(&mut state);
    }

    /// Stops showing a subscription. The last button of an application tells its reader, which
    /// leaves after a moment; the last application tells the monitor.
    pub(crate) fn remove(&self, id: SubscriptionId) {
        let mut state = locked(&self.state);
        state.groups.retain(|_, group| {
            let mut buttons = locked(&group.buttons);
            buttons.subscriptions.retain(|s| s.id() != id);
            if !buttons.subscriptions.is_empty() {
                return true;
            }
            group.wake.stop();
            // Without a reader there is nobody to leave: the group goes now.
            group.served
        });
        state.stop_if_idle();
    }
}
