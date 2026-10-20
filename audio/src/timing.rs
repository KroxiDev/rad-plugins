use std::time::Duration;

/// Time limits of the plugin. The defaults are for production; tests shorten them.
///
/// Start from [`Timing::default`] and change what is needed with the methods below, each of which
/// returns the updated value. The fields are private, so a new limit is not a breaking change and
/// there is a single way to build one:
///
/// ```compile_fail,E0451
/// use rad_plugin_audio::Timing;
/// let _ = Timing { ..Timing::default() };
/// ```
#[derive(Debug, Clone)]
pub struct Timing {
    pub(crate) action_deadline: Duration,
    pub(crate) options_deadline: Duration,
    pub(crate) refresh_deadline: Duration,
    pub(crate) settle: Duration,
    pub(crate) max_settle: Duration,
    pub(crate) retry_wait: Duration,
    pub(crate) retry_max: Duration,
    pub(crate) recheck: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            action_deadline: Duration::from_millis(3000),
            options_deadline: Duration::from_millis(1500),
            refresh_deadline: Duration::from_millis(2000),
            settle: Duration::from_millis(50),
            max_settle: Duration::from_millis(250),
            retry_wait: Duration::from_millis(1000),
            retry_max: Duration::from_millis(30_000),
            recheck: Duration::from_secs(10),
        }
    }
}

impl Timing {
    /// Re-observe healthy sources even without events; only that observation renews visuals.
    pub fn recheck(mut self, interval: Duration) -> Self {
        self.recheck = interval.clamp(Duration::from_millis(1), Duration::from_secs(10));
        self
    }
    /// Longest an action may take in total, `pactl` runs included (3 s by default). The host
    /// gives up at 5 s, so this stays below it and a hung `pactl` is killed and reported before
    /// that.
    pub fn action_deadline(mut self, limit: Duration) -> Self {
        self.action_deadline = limit;
        self
    }

    /// Longest an options query may take (1.5 s by default); the host's own limit is 2 s.
    pub fn options_deadline(mut self, limit: Duration) -> Self {
        self.options_deadline = limit;
        self
    }

    /// Longest one refresh of the device cache may take (2 s by default).
    pub fn refresh_deadline(mut self, limit: Duration) -> Self {
        self.refresh_deadline = limit;
        self
    }

    /// Quiet time after a server event before the cache is refreshed (50 ms by default), so a
    /// burst of events becomes one refresh.
    pub fn settle(mut self, quiet: Duration) -> Self {
        self.settle = quiet;
        self
    }

    /// Longest a refresh is postponed by a continuous stream of events (250 ms by default).
    pub fn max_settle(mut self, limit: Duration) -> Self {
        self.max_settle = limit;
        self
    }

    /// First wait before trying again after a failed refresh, an ended event stream (the server
    /// went away) or a failure inside the watching (1 s by default). It is never less than a
    /// millisecond, so that the waits can still grow, and goes back to this value once the
    /// server answers again.
    pub fn retry_wait(mut self, wait: Duration) -> Self {
        self.retry_wait = wait;
        self
    }

    /// Longest wait between tries (30 s by default). The wait doubles after each failed try, up
    /// to this limit; a limit below `retry_wait` is raised to it.
    pub fn retry_max(mut self, limit: Duration) -> Self {
        self.retry_max = limit;
        self
    }
}
