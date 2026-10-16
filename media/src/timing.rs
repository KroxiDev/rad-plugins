use std::time::Duration;

/// Time limits of the plugin. The defaults are for production; tests shorten them.
#[derive(Debug, Clone)]
pub struct Timing {
    /// Longest an action may take in total, bus calls included. The host gives up at 5 s, so
    /// this stays below it and a player that never answers is reported before that.
    pub action_deadline: Duration,
    /// Longest an options query may take; the host's own limit is 2 s.
    pub options_deadline: Duration,
    /// Longest the plugin waits for one player to say its name when listing options. A player that
    /// does not answer in time is listed under its application name.
    pub label_deadline: Duration,
    /// Longest one refresh of a visual may take, and the longest the bus monitor waits for one
    /// attempt to connect to the bus.
    pub refresh_deadline: Duration,
    /// Quiet time after a change before a visual is refreshed, so a burst becomes one refresh.
    pub settle: Duration,
    /// Longest a refresh is postponed by a continuous stream of changes.
    pub max_settle: Duration,
    /// How long a displayed player may go without being read before it is read anyway. It is
    /// read when it announces a change; this is for the player that cannot announce anything
    /// because it stopped answering, which would otherwise stay on display as it was.
    pub recheck: Duration,
    /// How long the reader of an application stays after its last button left, in case a button
    /// comes back: a burst of buttons coming and going then reuses the reader instead of
    /// starting a new one each time.
    pub linger: Duration,
    /// Wait before reconnecting to the bus after losing it.
    pub retry_wait: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            action_deadline: Duration::from_millis(3000),
            options_deadline: Duration::from_millis(1500),
            label_deadline: Duration::from_millis(300),
            refresh_deadline: Duration::from_millis(1500),
            settle: Duration::from_millis(30),
            max_settle: Duration::from_millis(120),
            recheck: Duration::from_secs(10),
            linger: Duration::from_millis(1000),
            retry_wait: Duration::from_millis(1000),
        }
    }
}
