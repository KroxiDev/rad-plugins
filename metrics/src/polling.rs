//! One worker per plugin. The timer never executes driver code or joins that worker.
use crate::readings::Memory;
use rad_plugin_sdk::{VisualContent, VisualState};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};

pub const INTERVAL: Duration = Duration::from_secs(1);
pub const MAX_AGE: Duration = Duration::from_millis(1500);

/// Private resource demand, including releases coalesced before the worker wakes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Demand {
    pub sources: [bool; 4],
    pub epoch: u64,
    pub cpu_epoch: u64,
    pub nvml_epoch: u64,
}
impl Demand {
    pub fn set(&mut self, sources: [bool; 4]) {
        if self.sources == sources {
            return;
        }
        self.epoch += 1;
        if self.sources[0] && !sources[0] {
            self.cpu_epoch += 1;
        }
        if (self.sources[2] || self.sources[3]) && !(sources[2] || sources[3]) {
            self.nvml_epoch += 1;
        }
        self.sources = sources;
    }
    pub fn active(self) -> bool {
        self.sources.into_iter().any(|v| v)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub cpu: Option<u32>,
    pub ram: Option<Memory>,
    pub gpu: Option<u32>,
    pub vram: Option<Memory>,
}
impl Sample {
    fn contents(&self) -> [Option<VisualContent>; 4] {
        fn percent(label: &str, value: Option<u32>) -> Option<String> {
            value.filter(|v| *v <= 100).map(|v| format!("{label} {v}%"))
        }
        fn memory(label: &str, value: Option<Memory>, available: bool) -> Option<String> {
            let m = value?;
            if m.total == 0
                || m.used > m.total
                || m.available > m.total
                || m.used.checked_add(m.available)? != m.total
            {
                return None;
            }
            let gib = 1073741824.0;
            Some(if available {
                format!(
                    "{label} {:.1} GiB used {:.1} GiB avail",
                    m.used as f64 / gib,
                    m.available as f64 / gib
                )
            } else {
                format!(
                    "{label} {:.1}/{:.1} GiB",
                    m.used as f64 / gib,
                    m.total as f64 / gib
                )
            })
        }
        [
            percent("CPU", self.cpu),
            memory("RAM", self.ram, true),
            percent("GPU", self.gpu),
            memory("VRAM", self.vram, false),
        ]
        .map(|text| {
            text.map(|text| VisualContent {
                text: Some(text),
                ..Default::default()
            })
        })
    }
}

#[derive(Clone, Copy)]
struct Request {
    demand: Demand,
    sample: bool,
}

pub struct Poller {
    request: SyncSender<Request>,
    response: Receiver<(Request, Sample)>,
    stopped: Arc<AtomicBool>,
    pending: Option<Duration>,
    delivered: Option<Demand>,
    demand: Demand,
    next: Duration,
    sample: Option<(Duration, Sample)>,
}
impl Poller {
    pub fn new(mut read: impl FnMut() -> Sample + Send + 'static) -> std::io::Result<Self> {
        Self::with_demand_factory(move || {
            move |_, sample| {
                if sample { read() } else { Sample::default() }
            }
        })
    }
    pub(crate) fn with_demand_factory<F, R>(factory: F) -> std::io::Result<Self>
    where
        F: FnOnce() -> R + Send + 'static,
        R: FnMut(Demand, bool) -> Sample + 'static,
    {
        let (request, requests) = mpsc::sync_channel::<Request>(1);
        let (responses, response) = mpsc::sync_channel(1);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        std::thread::Builder::new()
            .name("metrics-reader".into())
            .spawn(move || {
                // Construction, reconciliation (including unload), and reads share this worker.
                let Ok(mut request) = requests.recv() else {
                    return;
                };
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let mut read = factory();
                loop {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let sample = read(request.demand, request.sample);
                    if stop.load(Ordering::Acquire) || responses.send((request, sample)).is_err() {
                        break;
                    }
                    let Ok(next) = requests.recv() else {
                        break;
                    };
                    request = next;
                }
            })?;
        Ok(Self {
            request,
            response,
            stopped,
            pending: None,
            delivered: None,
            demand: Demand::default(),
            next: Duration::ZERO,
            sample: None,
        })
    }
    /// Compatibility reader: all sources share the requested active state.
    pub fn tick(&mut self, now: Duration, active: bool) -> [VisualState; 4] {
        let mut demand = self.demand;
        demand.set([active; 4]);
        self.tick_demand(now, demand)
    }
    /// Monotonic time from one fixed origin, not wall-clock time.
    pub(crate) fn tick_demand(&mut self, now: Duration, demand: Demand) -> [VisualState; 4] {
        if demand != self.demand {
            self.sample = None;
            self.demand = demand;
        }
        // Deadline is checked before receiving: a late result never regains freshness.
        if self
            .pending
            .is_some_and(|started| now.saturating_sub(started) >= MAX_AGE)
        {
            self.stopped.store(true, Ordering::Release);
        }
        if !self.stopped.load(Ordering::Acquire) {
            if let Ok((request, sample)) = self.response.try_recv()
                && let Some(started) = self.pending.take()
                && request.sample
                && request.demand == demand
            {
                self.sample = Some((started, sample));
            }
            let sample = demand.active() && now >= self.next;
            // Even empty demand must reach a previously activated reader to release resources.
            let reconcile = self.delivered.is_some_and(|last| last != demand);
            if self.pending.is_none()
                && (sample || reconcile)
                && self.request.try_send(Request { demand, sample }).is_ok()
            {
                self.delivered = Some(demand);
                self.pending = Some(now);
                if sample {
                    self.next = now.saturating_add(INTERVAL);
                }
            }
        }
        match &self.sample {
            None => std::array::from_fn(|_| VisualState::Unavailable),
            Some((started, sample)) => {
                let contents = sample.contents();
                std::array::from_fn(|i| match &contents[i] {
                    Some(content) if demand.sources[i] => {
                        if now.saturating_sub(*started) >= MAX_AGE {
                            VisualState::Stale {
                                content: content.clone(),
                            }
                        } else {
                            VisualState::Current {
                                content: content.clone(),
                            }
                        }
                    }
                    _ => VisualState::Unavailable,
                })
            }
        }
    }
    pub(crate) fn observation(&self) -> Option<Duration> {
        self.sample.as_ref().map(|s| s.0)
    }
    /// No idle timer once release has been acknowledged. A blocked reader remains terminal.
    pub(crate) fn wake_after(&self, now: Duration) -> Option<Duration> {
        let running = !self.stopped.load(Ordering::Acquire);
        let mut wait = if running && self.pending.is_some() {
            Some(Duration::from_millis(50))
        } else if running && self.demand.active() {
            Some(self.next.saturating_sub(now))
        } else {
            None
        };
        if let Some((started, _)) = &self.sample {
            let age = now.saturating_sub(*started);
            if age < MAX_AGE {
                let expiry = MAX_AGE - age;
                wait = Some(wait.map_or(expiry, |w| w.min(expiry)));
            }
        }
        wait
    }
}
impl Drop for Poller {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn empty_demand_is_acknowledged_then_idle_has_no_timer_or_reads() {
        let (tx, rx) = mpsc::channel();
        let mut poller = Poller::with_demand_factory(move || {
            move |demand, sample| {
                tx.send((demand, sample)).unwrap();
                Sample::default()
            }
        })
        .unwrap();
        assert_eq!(poller.wake_after(Duration::ZERO), None);
        let mut demand = Demand::default();
        demand.set([false, false, true, false]);
        poller.tick_demand(Duration::ZERO, demand);
        assert!(rx.recv_timeout(INTERVAL).unwrap().1);
        demand.set([false; 4]);
        let deadline = std::time::Instant::now() + INTERVAL;
        loop {
            poller.tick_demand(Duration::from_millis(1), demand);
            if let Ok((empty, sample)) = rx.try_recv() {
                assert_eq!(empty, demand);
                assert!(!sample);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        while poller.pending.is_some() {
            poller.tick_demand(Duration::from_millis(2), demand);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(poller.wake_after(Duration::from_secs(10)), None);
        poller.tick_demand(Duration::from_secs(10), demand);
        assert!(rx.try_recv().is_err());
    }
}
