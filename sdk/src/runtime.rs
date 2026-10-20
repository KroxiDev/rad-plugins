//! Handshake and the dispatch loop.
use crate::{
    ChoiceList, Envelope, Error, FailureCode, Message, OptionsScope, Outcome, PROTOCOL, Parameters,
    Reply, Subscription, SubscriptionId, Version, frame,
    reply::Slots,
    subscription::{Heartbeat, Refused, Registry},
    writer::Writer,
};
use std::{
    io::{BufReader, Read, Write},
    ops::RangeInclusive,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::Duration,
};

/// Default period of the visual heartbeat. The host expires a visual 1000 ms after its last
/// message, so the period must stay well below that.
pub const DEFAULT_HEARTBEAT: Duration = Duration::from_millis(400);
/// Longest heartbeat period the SDK will use, whatever [`Config::heartbeat`] asks for.
pub const MAX_HEARTBEAT: Duration = Duration::from_millis(500);
/// Actions a plugin may run at once; the next one is answered `busy` without being queued.
pub const MAX_INVOCATIONS: usize = 16;
/// Option queries a plugin may answer at once; extra ones are answered `unavailable`.
pub const MAX_OPTION_QUERIES: usize = 4;
/// Visual subscriptions kept at once; extra ones are answered `unavailable` and not tracked.
pub const MAX_SUBSCRIPTIONS: usize = 64;

/// Identity and runtime settings of a plugin.
///
/// Process lifecycle is static manifest metadata (`on_demand` by default or an
/// explicit `always_loaded`), not a setting negotiated through this configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Plugin id, as in the manifest.
    pub id: String,
    pub version: Version,
    /// Inclusive range of host APIs the plugin supports (`host_min..=host_max` of the manifest).
    pub host_api: RangeInclusive<u32>,
    heartbeat: Duration,
    credential_slots: Vec<String>,
}
impl Config {
    pub fn new(id: impl Into<String>, version: Version, host_api: RangeInclusive<u32>) -> Self {
        Self {
            id: id.into(),
            version,
            host_api,
            heartbeat: DEFAULT_HEARTBEAT,
            credential_slots: Vec::new(),
        }
    }

    /// Declared union of credential slot IDs, matching the manifest.
    pub fn credential_slots(mut self, slots: Vec<String>) -> Self {
        self.credential_slots = slots;
        self
    }
    /// Period at which the last state of every subscription is repeated. Clamped to
    /// [`MAX_HEARTBEAT`].
    pub fn heartbeat(mut self, interval: Duration) -> Self {
        self.heartbeat = interval.min(MAX_HEARTBEAT);
        self
    }
}

/// An action the host asks the plugin to run.
#[derive(Debug, Clone)]
pub struct Invoke {
    pub credentials: Option<crate::Credentials>,
    pub action: String,
    pub parameters: Parameters,
}

/// A request for the current options of a dynamic choice field.
#[derive(Debug, Clone)]
pub struct OptionsQuery {
    pub credentials: Option<crate::Credentials>,
    pub scope: OptionsScope,
    pub definition: String,
    pub field: String,
    pub context: Parameters,
}

/// The behaviour of a plugin. Methods run on the loop's thread and must return quickly: do slow
/// work on another thread and complete the [`Reply`] or [`Subscription`] from there.
///
/// Construct on-demand implementations without opening external resources. Acquire only
/// resources needed by each handler, share them among compatible consumers, and release
/// them after the last consumer's actual work ends. Keep credential revisions isolated.
/// On reactivation, refresh current state without replaying previous actions.
/// The SDK dispatches demand; it cannot manage arbitrary implementation-owned resources.
pub trait Plugin: Send + Sync + 'static {
    /// Run an action. Dropping `reply` unanswered answers `failed`.
    /// Action resources may be acquired here rather than prewarmed on process startup.
    fn invoke(&self, call: Invoke, reply: Reply<Outcome>);
    /// List the options of a dynamic choice field. Dropping `reply` unanswered answers
    /// `unavailable`.
    /// Acquire only this query's resources; listing options must not invoke an action.
    fn options(&self, query: OptionsQuery, reply: Reply<ChoiceList>);
    /// The host subscribed to a visual source. Publish its state with [`Subscription::update`].
    /// Share required resources with compatible consumers instead of duplicating readers.
    fn subscribe(&self, subscription: Subscription);
    /// The subscription ended, because the host asked or because the session closed.
    /// Release this consumer and stop resources whose last consumer has gone.
    fn unsubscribe(&self, subscription: SubscriptionId);
    /// Retire any external session belonging to this revision before returning.
    /// Runs only after all operation handles have been dropped; do not block.
    fn credentials_released(&self, _revision: u64) {}
}

/// Serves the host on `input`/`output` until the host closes the stream (`Ok`), breaks the
/// protocol or declares an incompatible API (`Err`).
pub fn run<P, R, W>(config: Config, plugin: P, input: R, output: W) -> Result<(), Error>
where
    P: Plugin,
    R: Read,
    W: Write + Send + 'static,
{
    let writer = Writer::new(output);
    let mut input = BufReader::new(input);
    let Some((generation, api)) = handshake(&config, &mut input, &writer)? else {
        return Ok(());
    };
    let credentials = crate::credentials::Registry::new(config.credential_slots)?;
    if credentials.declared() && (api < 3 || *config.host_api.start() < 3) {
        return Err(Error::Protocol("credentials require API 3"));
    }
    let mut session = Session {
        api,
        credentials,
        plugin,
        writer,
        generation,
        invocations: Slots::new(MAX_INVOCATIONS),
        queries: Slots::new(if api >= 3 { 2 } else { MAX_OPTION_QUERIES }),
        subscriptions: Registry::new(if api >= 3 { 32 } else { MAX_SUBSCRIPTIONS }),
    };
    let heartbeat = Heartbeat::start(Arc::clone(&session.subscriptions), config.heartbeat);
    let result = session.serve(&mut input);
    drop(heartbeat);
    // The host is gone, so every subscription is over.
    for subscription in session.subscriptions.drain() {
        guarded(|| session.plugin.unsubscribe(subscription.id()));
    }
    result
}

/// Serves the host over stdin and stdout.
pub fn run_stdio<P: Plugin>(config: Config, plugin: P) -> Result<(), Error> {
    run(config, plugin, std::io::stdin(), std::io::stdout())
}

struct Session<P> {
    plugin: P,
    api: u32,
    credentials: crate::credentials::Registry,
    writer: Writer,
    generation: u64,
    invocations: Arc<Slots>,
    queries: Arc<Slots>,
    subscriptions: Arc<Registry>,
}

impl<P: Plugin> Session<P> {
    fn serve(&mut self, input: &mut impl Read) -> Result<(), Error> {
        let mut last_request = 1;
        while let Some(envelope) = next_envelope(input)? {
            let correlation = envelope.correlation;
            if correlation.generation != self.generation {
                return Err(Error::Protocol("generation changed"));
            }
            if self.api >= 3 && correlation.request <= last_request {
                return Err(Error::Protocol("request reused"));
            }
            last_request = correlation.request;
            match envelope.message {
                Message::Credentials { revision, values } if self.api >= 3 => {
                    self.credentials.install(revision, values)?;
                    self.writer
                        .send(correlation, Message::CredentialsApplied { revision })?;
                }
                Message::CredentialsRelease { revision } if self.api >= 3 => {
                    self.credentials.release(revision)?;
                    // A failed cleanup cannot acknowledge external resources released.
                    catch_unwind(AssertUnwindSafe(|| {
                        self.plugin.credentials_released(revision)
                    }))
                    .map_err(|_| Error::Protocol("credential release handler failed"))?;
                    self.writer
                        .send(correlation, Message::CredentialsReleased { revision })?;
                }
                Message::Invoke {
                    action,
                    parameters,
                    credential_revision,
                } => {
                    let credentials = self.credentials.acquire(credential_revision)?;
                    match self.invocations.acquire() {
                        Some(permit) => {
                            let reply = Reply::new(
                                self.writer.clone(),
                                correlation,
                                permit,
                                credentials.clone(),
                            );
                            guarded(|| {
                                self.plugin.invoke(
                                    Invoke {
                                        action,
                                        parameters,
                                        credentials,
                                    },
                                    reply,
                                )
                            });
                        }
                        None => self.writer.send(
                            correlation,
                            Message::Result {
                                outcome: Outcome::Failure {
                                    code: FailureCode::Busy,
                                },
                            },
                        )?,
                    }
                }
                Message::Options {
                    scope,
                    definition,
                    field,
                    context,
                    credential_revision,
                } if self.api >= 2 => {
                    let credentials = self.credentials.acquire(credential_revision)?;
                    match self.queries.acquire() {
                        Some(permit) => {
                            let reply = Reply::new(
                                self.writer.clone(),
                                correlation,
                                permit,
                                credentials.clone(),
                            );
                            let query = OptionsQuery {
                                credentials,
                                scope,
                                definition,
                                field,
                                context,
                            };
                            guarded(|| self.plugin.options(query, reply));
                        }
                        None => self.writer.send(
                            correlation,
                            Message::Choices {
                                list: ChoiceList::Unavailable,
                            },
                        )?,
                    }
                }
                Message::Subscribe {
                    source,
                    parameters,
                    credential_revision,
                } => {
                    let credentials = self.credentials.acquire(credential_revision)?;
                    let subscription = Subscription::new(
                        self.writer.clone(),
                        correlation,
                        source,
                        parameters,
                        credentials,
                    );
                    match self.subscriptions.insert(&subscription) {
                        Ok(()) => guarded(|| self.plugin.subscribe(subscription)),
                        Err(Refused::Full) => subscription.update(crate::VisualState::Unavailable),
                        Err(Refused::Duplicate) => {
                            return Err(Error::Protocol("subscription correlation reused"));
                        }
                    }
                }
                Message::Unsubscribe { subscription } => {
                    if self.subscriptions.remove(subscription).is_some() {
                        guarded(|| self.plugin.unsubscribe(subscription));
                    }
                }
                _ => return Err(Error::Protocol("message not allowed from a host")),
            }
        }
        Ok(())
    }
}

/// A panic in a handler fails that request (its `Reply` answers while unwinding) and nothing else.
fn guarded(handler: impl FnOnce()) {
    let _ = catch_unwind(AssertUnwindSafe(handler));
}

fn next_envelope(input: &mut impl Read) -> Result<Option<Envelope>, Error> {
    let Some(payload) = frame::read(input)? else {
        return Ok(None);
    };
    let payload = zeroize::Zeroizing::new(payload);
    crate::json::validate(&payload).map_err(|_| Error::Protocol("invalid message"))?;
    let envelope: Envelope =
        serde_json::from_slice(&payload).map_err(|_| Error::Protocol("invalid message"))?;
    if envelope.protocol != PROTOCOL {
        return Err(Error::Protocol("unsupported protocol version"));
    }
    if envelope.correlation.generation == 0 || envelope.correlation.request == 0 {
        return Err(Error::Protocol("correlation ids must be positive"));
    }
    Ok(Some(envelope))
}

/// Answers the host's hello. `Ok(None)` if the host left before saying hello.
fn handshake(
    config: &Config,
    input: &mut impl Read,
    writer: &Writer,
) -> Result<Option<(u64, u32)>, Error> {
    let Some(envelope) = next_envelope(input)? else {
        return Ok(None);
    };
    let Message::Hello { host_api, .. } = envelope.message else {
        return Err(Error::Protocol("expected hello"));
    };
    if envelope.correlation.request != 1 {
        return Err(Error::Protocol("hello must use request 1"));
    }
    if !config.host_api.contains(&host_api) {
        return Err(Error::IncompatibleHost {
            requested: host_api,
            min: *config.host_api.start(),
            max: *config.host_api.end(),
        });
    }
    writer.send(
        envelope.correlation,
        Message::Ready {
            plugin_id: config.id.clone(),
            plugin_version: config.version,
        },
    )?;
    Ok(Some((envelope.correlation.generation, host_api)))
}
