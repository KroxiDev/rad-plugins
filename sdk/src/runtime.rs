//! Handshake and the dispatch loop.
use crate::{
    ChoiceList, Envelope, Error, FailureCode, Message, OptionsScope, Outcome, Parameters, Reply,
    Subscription, SubscriptionId, Version, frame,
    reply::Slots,
    subscription::{Refused, Registry},
    writer::Writer,
};
use std::{
    io::{BufReader, Read, Write},
    ops::RangeInclusive,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

/// Actions a plugin may run at once; the next one is answered `busy` without being queued.
pub const MAX_INVOCATIONS: usize = 16;
/// Option queries a plugin may answer at once; extra ones are answered `unavailable`.
pub const MAX_OPTION_QUERIES: usize = 2;
/// Visual subscriptions kept at once; extra ones are answered `unavailable` and not tracked.
pub const MAX_SUBSCRIPTIONS: usize = 32;

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
    capabilities: Vec<String>,
    credential_slots: Vec<String>,
}
impl Config {
    pub fn new(id: impl Into<String>, version: Version, host_api: RangeInclusive<u32>) -> Self {
        Self {
            id: id.into(),
            version,
            host_api,
            capabilities: vec![],
            credential_slots: Vec::new(),
        }
    }

    /// Declared union of credential slot IDs, matching the manifest.
    pub fn credential_slots(mut self, slots: Vec<String>) -> Self {
        self.credential_slots = slots;
        self
    }
    /// Features declared by this plugin, matching its manifest.
    pub fn capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities = capabilities;
        self
    }
}

/// An action the host asks the plugin to run.
#[derive(Debug, Clone)]
pub struct Invoke {
    pub credentials: Option<crate::Credentials>,
    pub action: String,
    pub settings: Option<Parameters>,
    pub control: Option<crate::ControlEvent>,
    pub parameters: Parameters,
}

/// A request for the current options of a dynamic choice field.
#[derive(Debug, Clone)]
pub struct OptionsQuery {
    pub settings: Option<Parameters>,
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
/// A custom output must itself enforce bounded writes. Native plugins should use
/// [`run_stdio`], which provides the nonblocking stdout deadline and terminal close.
pub fn run<P, R, W>(config: Config, plugin: P, input: R, output: W) -> Result<(), Error>
where
    P: Plugin,
    R: Read,
    W: Write + Send + 'static,
{
    let writer = Writer::new(output);
    let mut input = BufReader::new(input);
    let Some((generation, capabilities)) = handshake(&config, &mut input, &writer)? else {
        return Ok(());
    };
    let credentials = crate::credentials::Registry::new(config.credential_slots)?;
    if credentials.declared()
        && !rad_plugin_contract::supports(&capabilities, rad_plugin_contract::CREDENTIALS)
    {
        return Err(Error::Protocol("credentials capability required"));
    }
    let mut session = Session {
        capabilities,
        credentials,
        plugin,
        writer,
        generation,
        invocations: Slots::new(MAX_INVOCATIONS),
        holds: crate::holds::Holds::new(),
        queries: Slots::new(MAX_OPTION_QUERIES),
        subscriptions: Registry::new(MAX_SUBSCRIPTIONS),
    };
    let result = session.serve(&mut input);
    // The host is gone, so every subscription is over.
    for subscription in session.subscriptions.drain() {
        guarded(|| session.plugin.unsubscribe(subscription.id()));
    }
    result
}

/// Serves the host over stdin and stdout.
pub fn run_stdio<P: Plugin>(config: Config, plugin: P) -> Result<(), Error> {
    let output = crate::output::HostOutput::open(1).unwrap_or_else(|_| std::process::exit(74));
    run(config, plugin, std::io::stdin(), output)
}

struct Session<P> {
    plugin: P,
    capabilities: Vec<String>,
    credentials: crate::credentials::Registry,
    writer: Writer,
    generation: u64,
    invocations: Arc<Slots>,
    holds: crate::holds::Holds,
    queries: Arc<Slots>,
    subscriptions: Arc<Registry>,
}

impl<P: Plugin> Session<P> {
    fn serve(&mut self, input: &mut impl Read) -> Result<(), Error> {
        let mut last_request = 1;
        while let Some(envelope) = next_envelope(input)? {
            if !envelope.message.permitted(&self.capabilities) {
                return Err(Error::Protocol("unnegotiated feature"));
            }
            let correlation = envelope.correlation;
            if correlation.generation != self.generation {
                return Err(Error::Protocol("generation changed"));
            }
            if correlation.request <= last_request {
                return Err(Error::Protocol("request reused"));
            }
            last_request = correlation.request;
            match envelope.message {
                Message::Credentials { revision, values }
                    if rad_plugin_contract::supports(
                        &self.capabilities,
                        rad_plugin_contract::CREDENTIALS,
                    ) =>
                {
                    self.credentials.install(revision, values)?;
                    self.writer
                        .send(correlation, Message::CredentialsApplied { revision })?;
                }
                Message::CredentialsRelease { revision }
                    if rad_plugin_contract::supports(
                        &self.capabilities,
                        rad_plugin_contract::CREDENTIALS,
                    ) =>
                {
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
                    settings,
                    control,
                    action,
                    parameters,
                    credential_revision,
                } => {
                    let credentials = self.credentials.acquire(credential_revision)?;
                    use rad_plugin_contract::ControlGesture;
                    let permit = match control.as_ref().map(|c| c.gesture) {
                        Some(ControlGesture::HoldEnd) => Some(self.holds.release(
                            &control.as_ref().expect("matched control").control,
                            &action,
                            &parameters,
                            &settings,
                            credential_revision,
                        )?),
                        Some(ControlGesture::HoldStart) => {
                            self.invocations.acquire().filter(|_| {
                                self.holds.reserve(
                                    control.as_ref().expect("matched control").control.clone(),
                                    &action,
                                    &parameters,
                                    &settings,
                                    credential_revision,
                                    credentials.clone(),
                                )
                            })
                        }
                        _ => self.invocations.acquire(),
                    };
                    match permit {
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
                                        settings,
                                        control,
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
                    settings,
                    scope,
                    definition,
                    field,
                    context,
                    credential_revision,
                } if rad_plugin_contract::supports(
                    &self.capabilities,
                    rad_plugin_contract::DYNAMIC_OPTIONS,
                ) =>
                {
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
                                settings,
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
                    settings,
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
                        settings,
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
    let mut framed = zeroize::Zeroizing::new((payload.len() as u32).to_be_bytes().to_vec());
    framed.extend_from_slice(&payload);
    let (envelope, _) = rad_plugin_contract::decode(&framed)
        .map_err(|_| Error::Protocol("invalid message"))?
        .ok_or(Error::Protocol("incomplete message"))?;
    Ok(Some(envelope))
}

/// Answers the host's hello. `Ok(None)` if the host left before saying hello.
fn handshake(
    config: &Config,
    input: &mut impl Read,
    writer: &Writer,
) -> Result<Option<(u64, Vec<String>)>, Error> {
    let Some(envelope) = next_envelope(input)? else {
        return Ok(None);
    };
    let Message::Hello {
        host_api,
        capabilities,
        plugin_id,
        plugin_version,
        target,
    } = envelope.message
    else {
        return Err(Error::Protocol("expected hello"));
    };
    if envelope.correlation.request != 1 {
        return Err(Error::Protocol("hello must use request 1"));
    }
    if host_api != rad_plugin_contract::HOST_API || !config.host_api.contains(&host_api) {
        return Err(Error::IncompatibleHost {
            requested: host_api,
            min: *config.host_api.start(),
            max: *config.host_api.end(),
        });
    }
    if plugin_id != config.id
        || plugin_version != config.version
        || target != "x86_64-unknown-linux-gnu"
    {
        return Err(Error::Protocol("hello identity or target mismatch"));
    }
    let capabilities =
        rad_plugin_contract::negotiate_capabilities(&capabilities, &config.capabilities)
            .map_err(|_| Error::Protocol("invalid capabilities"))?;
    writer.negotiate(capabilities.clone());
    writer.send(
        envelope.correlation,
        Message::Ready {
            capabilities: capabilities.clone(),
            plugin_id: config.id.clone(),
            plugin_version: config.version,
        },
    )?;
    Ok(Some((envelope.correlation.generation, capabilities)))
}
