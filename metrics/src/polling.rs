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

pub struct Poller {
    request: SyncSender<()>,
    response: Receiver<Sample>,
    stopped: Arc<AtomicBool>,
    pending: Option<Duration>,
    next: Duration,
    sample: Option<(Duration, Sample)>,
}
impl Poller {
    pub fn new(read: impl FnMut() -> Sample + Send + 'static) -> std::io::Result<Self> {
        Self::with_factory(move || read)
    }
    pub(crate) fn with_factory<F, R>(factory: F) -> std::io::Result<Self>
    where
        F: FnOnce() -> R + Send + 'static,
        R: FnMut() -> Sample + 'static,
    {
        let (request, requests) = mpsc::sync_channel(1);
        let (responses, response) = mpsc::sync_channel(1);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        std::thread::Builder::new()
            .name("metrics-reader".into())
            .spawn(move || {
                // Initialize libraries only on first demand, and on this isolated thread.
                if requests.recv().is_err() || stop.load(Ordering::Acquire) {
                    return;
                }
                let mut read = factory();
                loop {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let sample = read();
                    if stop.load(Ordering::Acquire) || responses.send(sample).is_err() {
                        break;
                    }
                    if requests.recv().is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            request,
            response,
            stopped,
            pending: None,
            next: Duration::ZERO,
            sample: None,
        })
    }
    /// Monotonic time from one fixed origin, not wall-clock time.
    pub fn tick(&mut self, now: Duration, active: bool) -> [VisualState; 4] {
        // Deadline is checked before receiving: a late result never regains freshness.
        if self
            .pending
            .is_some_and(|started| now.saturating_sub(started) >= MAX_AGE)
        {
            self.stopped.store(true, Ordering::Release);
        }
        if !self.stopped.load(Ordering::Acquire) {
            if let Ok(sample) = self.response.try_recv()
                && let Some(started) = self.pending.take()
            {
                self.sample = Some((started, sample));
            }
            if active
                && self.pending.is_none()
                && now >= self.next
                && self.request.try_send(()).is_ok()
            {
                self.pending = Some(now);
                self.next = now.saturating_add(INTERVAL);
            }
        }
        match &self.sample {
            None => std::array::from_fn(|_| VisualState::Unavailable),
            Some((started, sample)) => sample.contents().map(|content| match content {
                Some(content) if now.saturating_sub(*started) >= MAX_AGE => {
                    VisualState::Stale { content }
                }
                Some(content) => VisualState::Current { content },
                None => VisualState::Unavailable,
            }),
        }
    }
}
impl Drop for Poller {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}
