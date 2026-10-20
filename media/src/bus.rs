//! The connection to the session bus and the calls made over it.
//!
//! Every call carries a deadline and is made without auto-start: asking for a player that is not
//! running must never launch it. Connecting has a deadline too, so a bus that accepts connections
//! and never answers costs a caller no more than its own time limit.
use gio::{
    Cancellable, DBusCallFlags, DBusConnection, DBusConnectionFlags, DBusError, IOErrorEnum,
    prelude::CancellableExt,
};
use glib::{ControlFlow, MainContext, Priority, Variant, VariantTy};
use rad_plugin_sdk::FailureCode;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak},
    time::{Duration, Instant},
};

/// Why the bus or a player could not be asked, in the terms the plugin reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// No bus, or nobody with that name on it (any more).
    Unavailable,
    /// The bus or the player did not answer in time.
    TimedOut,
    Failed,
    /// The player says it cannot do what was asked.
    Incompatible,
}

impl Failure {
    pub(crate) fn code(self) -> FailureCode {
        match self {
            Self::Unavailable => FailureCode::Unavailable,
            Self::TimedOut => FailureCode::TimedOut,
            Self::Failed => FailureCode::Failed,
            Self::Incompatible => FailureCode::Incompatible,
        }
    }

    fn of(error: &glib::Error) -> Self {
        if error.matches(IOErrorEnum::TimedOut)
            || error.matches(DBusError::TimedOut)
            || error.matches(DBusError::Timeout)
        {
            Self::TimedOut
        } else if error.matches(IOErrorEnum::Closed)
            || error.matches(DBusError::Disconnected)
            || error.matches(DBusError::ServiceUnknown)
            || error.matches(DBusError::NameHasNoOwner)
            // The peer left while the call was with it, and the bus answers for it.
            || error.matches(DBusError::NoReply)
        {
            Self::Unavailable
        } else {
            Self::Failed
        }
    }
}

/// A method of an object on the bus.
pub(crate) struct Method<'a> {
    /// The bus name of the peer.
    pub(crate) destination: &'a str,
    pub(crate) path: &'a str,
    pub(crate) interface: &'a str,
    pub(crate) name: &'a str,
}

/// The shared connection, and whether somebody is making it right now.
#[derive(Default)]
struct Link {
    open: Weak<Connection>,
    connecting: bool,
}

/// Owned by effective callers, never by the idle plugin. Each operation keeps a lease across
/// its calls; the last operation closes it, without interrupting another caller.
pub(crate) struct Connection(DBusConnection);

impl std::ops::Deref for Connection {
    type Target = DBusConnection;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // This closes the local transport, not a method call awaiting the bus or player.
        let _ = self.0.close_sync(None::<&Cancellable>);
    }
}

/// The session bus the plugin was started with. It connects when first needed and again after the
/// connection was lost, so a bus that comes back is used again.
pub(crate) struct Bus {
    address: Option<String>,
    link: Mutex<Link>,
    /// Signalled whenever an attempt to connect ends.
    attempted: Condvar,
}

/// Marks that a connection is being made, for as long as it lives.
struct Attempt<'a>(&'a Bus);

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        self.0.locked().connecting = false;
        self.0.attempted.notify_all();
    }
}

impl Bus {
    /// `address` is the bus to use; `None` means there is none.
    pub(crate) fn new(address: Option<String>) -> Self {
        Self {
            address,
            link: Mutex::new(Link::default()),
            attempted: Condvar::new(),
        }
    }

    fn locked(&self) -> MutexGuard<'_, Link> {
        self.link.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A new connection to the bus, made before `deadline`: the bus not being there is
    /// `Unavailable`, and a bus that does not answer in time is `TimedOut`.
    ///
    /// The connection's `closed` signal is emitted in `context`, so a thread that runs a main
    /// loop makes its connection in its own context. The thread iterates `context` while it
    /// waits.
    pub(crate) fn connect(
        &self,
        context: &MainContext,
        deadline: Instant,
    ) -> Result<DBusConnection, Failure> {
        let address = self.address.as_deref().ok_or(Failure::Unavailable)?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::TimedOut);
        }
        context
            .with_thread_default(|| connect_within(address, context, left))
            .map_err(|_| Failure::Failed)?
    }

    /// The connection calls are made over: shared, and made again once it was lost. Whoever finds
    /// it missing makes it; the others wait for that, but no longer than their own `deadline`.
    pub(crate) fn connection(&self, deadline: Instant) -> Result<Arc<Connection>, Failure> {
        let mut link = self.locked();
        loop {
            if let Some(open) = link.open.upgrade().filter(|c| !c.is_closed()) {
                return Ok(open);
            }
            if !link.connecting {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failure::TimedOut);
            }
            link = self
                .attempted
                .wait_timeout(link, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        link.connecting = true;
        drop(link);
        let _attempt = Attempt(self);
        let connection = Arc::new(Connection(self.connect(&MainContext::new(), deadline)?));
        self.locked().open = Arc::downgrade(&connection);
        Ok(connection)
    }

    /// Calls `method` and returns the reply, which must have the type `reply_type`.
    pub(crate) fn call(
        &self,
        method: &Method,
        parameters: Option<&Variant>,
        reply_type: &str,
        deadline: Instant,
    ) -> Result<Variant, Failure> {
        let connection = self.connection(deadline)?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::TimedOut);
        }
        let timeout = i32::try_from(left.as_millis().max(1)).unwrap_or(i32::MAX);
        connection
            .call_sync(
                Some(method.destination),
                method.path,
                method.interface,
                method.name,
                parameters,
                Some(VariantTy::new(reply_type).map_err(|_| Failure::Failed)?),
                DBusCallFlags::NO_AUTO_START,
                timeout,
                None::<&Cancellable>,
            )
            .map_err(|error| Failure::of(&error))
    }

    /// The bus names of the players that are on the bus right now.
    pub(crate) fn player_names(&self, deadline: Instant) -> Result<Vec<String>, Failure> {
        let list_names = Method {
            destination: "org.freedesktop.DBus",
            path: "/org/freedesktop/DBus",
            interface: "org.freedesktop.DBus",
            name: "ListNames",
        };
        let reply = self.call(&list_names, None, "(as)", deadline)?;
        let (mut names,) = reply.get::<(Vec<String>,)>().ok_or(Failure::Failed)?;
        names.retain(|name| crate::names::app_identity(name).is_some());
        Ok(names)
    }
}

/// Connects to `address` in the thread-default context, which is `context`: the connection is
/// made asynchronously and `context` is iterated until it is made, or until `limit` has passed
/// and the attempt has been cancelled.
fn connect_within(
    address: &str,
    context: &MainContext,
    limit: Duration,
) -> Result<DBusConnection, Failure> {
    let cancellable = Cancellable::new();
    let outcome = Rc::new(RefCell::new(None));
    let made = Rc::clone(&outcome);
    // Not EXIT_ON_CLOSE: losing the bus must not end the plugin.
    DBusConnection::for_address(
        address,
        DBusConnectionFlags::AUTHENTICATION_CLIENT | DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        Some(&cancellable),
        move |result| *made.borrow_mut() = Some(result),
    );
    let timer = glib::timeout_source_new(limit, Some("media-connect-limit"), Priority::DEFAULT, {
        let cancellable = cancellable.clone();
        move || {
            cancellable.cancel();
            ControlFlow::Break
        }
    });
    timer.attach(Some(context));
    let result = loop {
        if let Some(result) = outcome.borrow_mut().take() {
            break result;
        }
        context.iteration(true);
    };
    timer.destroy();
    result.map_err(|error| {
        if error.matches(IOErrorEnum::Cancelled) {
            Failure::TimedOut
        } else {
            Failure::Unavailable
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(domain_error: glib::Error) -> Failure {
        Failure::of(&domain_error)
    }

    #[test]
    fn errors_of_the_bus_become_the_failure_the_plugin_reports() {
        let io = |kind| of(glib::Error::new(kind, "test"));
        let dbus = |kind| of(glib::Error::new(kind, "test"));
        // A call that got no answer in time.
        assert_eq!(io(IOErrorEnum::TimedOut), Failure::TimedOut);
        assert_eq!(dbus(DBusError::TimedOut), Failure::TimedOut);
        assert_eq!(dbus(DBusError::Timeout), Failure::TimedOut);
        // Nobody to ask: the player left, or the bus did.
        assert_eq!(dbus(DBusError::ServiceUnknown), Failure::Unavailable);
        assert_eq!(dbus(DBusError::NameHasNoOwner), Failure::Unavailable);
        // The peer left with the call unanswered: the bus says so with `NoReply`.
        assert_eq!(dbus(DBusError::NoReply), Failure::Unavailable);
        assert_eq!(dbus(DBusError::Disconnected), Failure::Unavailable);
        assert_eq!(io(IOErrorEnum::Closed), Failure::Unavailable);
        // Anything else is a failure of the call itself.
        assert_eq!(dbus(DBusError::UnknownMethod), Failure::Failed);
        assert_eq!(dbus(DBusError::AccessDenied), Failure::Failed);
        assert_eq!(io(IOErrorEnum::InvalidArgument), Failure::Failed);
    }
}
