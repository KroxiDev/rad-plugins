//! The bus monitor: one thread with a GLib main loop that listens for what can change a visual,
//! and says so. It exists while something is displayed, and is told to stop when nothing is.
//!
//! It reports (but does not interpret) two kinds of signal: a player appearing or leaving the bus
//! (`NameOwnerChanged` for an MPRIS name) and a player announcing a new property value
//! (`PropertiesChanged` of the Player interface). What a change means is found by reading the
//! player again. If the bus is lost the monitor reports it, and keeps trying to connect again.
use crate::{Timing, bus::Bus, names::MPRIS_PREFIX, wake::Wake};
use gio::{DBusConnection, DBusSignalFlags, SignalSubscription};
use glib::{MainContext, prelude::*};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

/// Called, on the monitor's thread, whenever something may have changed.
pub(crate) type Changed = Arc<dyn Fn() + Send + Sync + 'static>;

/// Called, on the monitor's thread, once it was told to stop: `true` if it is to end (and has been
/// forgotten by whoever started it), `false` if it is needed after all.
pub(crate) type Retire = Arc<dyn Fn() -> bool + Send + Sync + 'static>;

/// A running monitor. Stopping it is a request: its thread ends by itself, if nobody has asked for
/// it again by then.
pub(crate) struct Monitor {
    control: Arc<Control>,
}

struct Control {
    /// Stop requests, and the pause between attempts to reconnect.
    wake: Wake,
    /// Where the signals are delivered; waking it ends the wait for one.
    context: MainContext,
    /// `retire` said it is to end: that is final, as it has been forgotten by then.
    ended: AtomicBool,
}

impl Control {
    /// Whether the monitor has to end now. A stop request that `retire` turns down is withdrawn.
    fn must_end(&self, retire: &Retire) -> bool {
        if self.ended.load(Ordering::Acquire) {
            return true;
        }
        if !self.wake.is_stopped() {
            return false;
        }
        if retire() {
            self.ended.store(true, Ordering::Release);
            return true;
        }
        self.wake.resume();
        false
    }
}

impl Monitor {
    /// Starts listening. `None` if the thread could not be started.
    pub(crate) fn start(
        bus: Arc<Bus>,
        timing: Timing,
        changed: Changed,
        retire: Retire,
    ) -> Option<Self> {
        let control = Arc::new(Control {
            wake: Wake::new(),
            context: MainContext::new(),
            ended: AtomicBool::new(false),
        });
        let thread_control = Arc::clone(&control);
        thread::Builder::new()
            .name("media-monitor".into())
            .spawn(move || {
                // The thread owns the context for as long as it runs.
                let _ = thread_control.context.with_thread_default(|| {
                    serve(&bus, &timing, &thread_control, &changed, &retire);
                });
            })
            .ok()?;
        Some(Self { control })
    }

    pub(crate) fn stop(&self) {
        self.control.wake.stop();
        self.control.context.wakeup();
    }
}

/// Listens on the bus until it has to end, connecting again whenever the bus was lost.
fn serve(bus: &Bus, timing: &Timing, control: &Control, changed: &Changed, retire: &Retire) {
    while !control.must_end(retire) {
        // Its own connection: it is the one whose loss this thread is told about.
        let deadline = Instant::now() + timing.refresh_deadline;
        if let Ok(connection) = bus.connect(&control.context, deadline) {
            listen(&connection, control, changed, retire);
        }
        if control.wake.is_stopped() {
            continue;
        }
        // No bus (any more): what is displayed has to say so.
        changed();
        control.wake.sleep(timing.retry_wait);
    }
}

/// Reports the changes seen on `connection` until it closes or the monitor has to end.
fn listen(connection: &DBusConnection, control: &Control, changed: &Changed, retire: &Retire) {
    let closed = Arc::new(AtomicBool::new(false));
    let on_close = {
        let (closed, context) = (Arc::clone(&closed), control.context.clone());
        connection.connect_closed(move |_, _, _| {
            closed.store(true, Ordering::Release);
            context.wakeup();
        })
    };
    let _signals = subscribe(connection, changed);
    // Subscribed, so from here on nothing is missed; and what happened before is looked at now.
    changed();
    while !control.must_end(retire) && !closed.load(Ordering::Acquire) && !connection.is_closed() {
        control.context.iteration(true);
    }
    connection.disconnect(on_close);
}

fn subscribe(connection: &DBusConnection, changed: &Changed) -> [SignalSubscription; 2] {
    let on_owner = {
        let changed = Arc::clone(changed);
        move |signal: gio::DBusSignalRef| {
            let mpris = signal
                .parameters
                .get::<(String, String, String)>()
                .is_some_and(|(name, _, _)| name.starts_with(MPRIS_PREFIX));
            if mpris {
                changed();
            }
        }
    };
    let on_properties = {
        let changed = Arc::clone(changed);
        move |_: gio::DBusSignalRef| changed()
    };
    [
        connection.subscribe_to_signal(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            None,
            DBusSignalFlags::NONE,
            on_owner,
        ),
        connection.subscribe_to_signal(
            None,
            Some("org.freedesktop.DBus.Properties"),
            Some("PropertiesChanged"),
            Some("/org/mpris/MediaPlayer2"),
            Some("org.mpris.MediaPlayer2.Player"),
            DBusSignalFlags::NONE,
            on_properties,
        ),
    ]
}
