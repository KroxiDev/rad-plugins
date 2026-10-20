use crate::{Error, Parameters, Version};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correlation {
    pub generation: u64,
    pub request: u64,
}
impl Correlation {
    pub fn matches(self, other: Self) -> bool {
        self.generation != 0 && self.request != 0 && self == other
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub protocol: u32,
    pub correlation: Correlation,
    pub message: Message,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Hello {
        capabilities: Vec<String>,
        host_api: u32,
        target: String,
        plugin_id: String,
        plugin_version: Version,
    },
    Ready {
        capabilities: Vec<String>,
        plugin_id: String,
        plugin_version: Version,
    },
    Invoke {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        control: Option<ControlEvent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settings: Option<Parameters>,
        action: String,
        parameters: Parameters,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_revision: Option<u64>,
    },
    Result {
        outcome: Outcome,
    },
    Subscribe {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settings: Option<Parameters>,
        source: String,
        parameters: Parameters,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_revision: Option<u64>,
    },
    Unsubscribe {
        subscription: Correlation,
    },
    VisualRenew {
        valid_for_ms: u64,
    },
    Visual {
        valid_for_ms: u64,
        state: VisualState,
    },
    Options {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settings: Option<Parameters>,
        scope: OptionsScope,
        definition: String,
        field: String,
        context: Parameters,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_revision: Option<u64>,
    },
    Credentials {
        revision: u64,
        values: Vec<crate::CredentialValue>,
    },
    CredentialsApplied {
        revision: u64,
    },
    CredentialsRelease {
        revision: u64,
    },
    CredentialsReleased {
        revision: u64,
    },
    Choices {
        list: ChoiceList,
    },
}
/// Most options one `choices` reply may carry; a list is always a single message.
pub const MAX_CHOICES: usize = 64;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionsScope {
    Action,
    Visual,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChoiceList {
    Available { options: Vec<ChoiceOption> },
    Unavailable,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    pub value: String,
    pub label: String,
}
impl std::fmt::Debug for ChoiceOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChoiceOption([redacted])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Success,
    Failure { code: FailureCode },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    InvalidParameters,
    Unavailable,
    Failed,
    TimedOut,
    Busy,
    Incompatible,
    CredentialsUnavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum VisualState {
    Current { content: VisualContent },
    Stale { content: VisualContent },
    Unavailable,
}
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualContent {
    pub text: Option<String>,
    pub png_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jpeg_hex: Option<String>,
    pub color: Option<[u8; 4]>,
}
impl std::fmt::Debug for VisualContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VisualContent([redacted])")
    }
}
/// One complete frame. None means incomplete, consumes nothing; trailing frames remain.
pub fn decode(bytes: &[u8]) -> Result<Option<(Envelope, usize)>, Error> {
    let Some(frame) = rad_wire::read_frame(bytes).map_err(|_| Error::InvalidFrame)? else {
        return Ok(None);
    };
    let value = rad_wire::parse_json(frame.payload()).map_err(|_| Error::InvalidDocument)?;
    if value
        .get("protocol")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|v| v != 1)
    {
        return Err(Error::UnsupportedVersion);
    }
    let envelope: Envelope = serde_json::from_value(value).map_err(|_| Error::InvalidMessage)?;
    envelope.validate()?;
    Ok(Some((envelope, frame.consumed())))
}
pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, Error> {
    envelope.validate()?;
    let bytes = zeroize::Zeroizing::new(crate::bounded::json(envelope, 65536)?);
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(&bytes);
    Ok(frame)
}
impl Envelope {
    fn validate(&self) -> Result<(), Error> {
        use crate::manifest::identifier;
        if self.protocol != 1 {
            return Err(Error::UnsupportedVersion);
        }
        if !self.correlation.matches(self.correlation) {
            return Err(Error::InvalidMessage);
        }
        match &self.message {
            Message::Invoke {
                settings, control, ..
            } => {
                if let Some(settings) = settings {
                    settings.check_shape()?;
                }
                if control
                    .as_ref()
                    .is_some_and(|e| !identifier(&e.control.kind))
                {
                    return Err(Error::InvalidMessage);
                }
            }
            Message::Subscribe { settings, .. } | Message::Options { settings, .. } => {
                if let Some(settings) = settings {
                    settings.check_shape()?;
                }
            }
            _ => {}
        }
        let valid = match &self.message {
            Message::Hello {
                capabilities,
                host_api,
                target,
                plugin_id,
                ..
            } => {
                crate::validate_capabilities(capabilities)?;
                *host_api > 0
                    && identifier(target)
                    && identifier(plugin_id)
                    && self.correlation.request == 1
            }
            Message::Ready {
                plugin_id,
                capabilities,
                ..
            } => {
                crate::validate_capabilities(capabilities)?;
                identifier(plugin_id) && self.correlation.request == 1
            }
            Message::Invoke {
                action,
                parameters,
                credential_revision,
                ..
            } => {
                parameters.check_shape()?;
                identifier(action) && credential_revision.is_none_or(|r| r > 0)
            }
            Message::Subscribe {
                source,
                parameters,
                credential_revision,
                ..
            } => {
                parameters.check_shape()?;
                identifier(source) && credential_revision.is_none_or(|r| r > 0)
            }
            Message::Options {
                definition,
                field,
                context,
                credential_revision,
                ..
            } => {
                context.check_shape()?;
                identifier(definition)
                    && identifier(field)
                    && credential_revision.is_none_or(|r| r > 0)
            }
            Message::Credentials { revision, values } => {
                crate::validate_credentials(values)?;
                *revision > 0 && self.correlation.request >= 2
            }
            Message::CredentialsApplied { revision }
            | Message::CredentialsRelease { revision }
            | Message::CredentialsReleased { revision } => {
                *revision > 0 && self.correlation.request >= 2
            }
            Message::Choices { list } => list.is_valid(),
            Message::Result { .. } => true,
            Message::Unsubscribe { subscription } => {
                subscription.matches(*subscription)
                    && subscription.generation == self.correlation.generation
            }
            Message::VisualRenew { valid_for_ms } => (1000..=60000).contains(valid_for_ms),
            Message::Visual {
                state,
                valid_for_ms,
            } => {
                (1000..=60000).contains(valid_for_ms)
                    && match state {
                        VisualState::Unavailable => true,
                        VisualState::Current { content } | VisualState::Stale { content } => {
                            content.is_valid()
                        }
                    }
            }
        };
        if valid {
            Ok(())
        } else {
            Err(Error::InvalidMessage)
        }
    }
}
impl ChoiceList {
    pub fn is_valid(&self) -> bool {
        use crate::manifest::label;
        match self {
            ChoiceList::Unavailable => true,
            ChoiceList::Available { options } => {
                options.len() <= MAX_CHOICES
                    && options.iter().all(|o| label(&o.value) && label(&o.label))
                    && options
                        .iter()
                        .enumerate()
                        .all(|(i, o)| options[..i].iter().all(|earlier| earlier.value != o.value))
            }
        }
    }
}
impl VisualContent {
    pub fn is_valid(&self) -> bool {
        fn hex(s: &str) -> bool {
            !s.is_empty()
                && s.len() <= 16384
                && s.len().is_multiple_of(2)
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        // An image is PNG or JPEG, never both.
        (self.text.is_some()
            || self.png_hex.is_some()
            || self.jpeg_hex.is_some()
            || self.color.is_some())
            && !(self.png_hex.is_some() && self.jpeg_hex.is_some())
            && self.text.as_ref().is_none_or(|s| crate::manifest::label(s))
            && self.png_hex.as_deref().is_none_or(hex)
            && self.jpeg_hex.as_deref().is_none_or(hex)
    }
}

impl VisualState {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Unavailable => true,
            Self::Current { content } | Self::Stale { content } => content.is_valid(),
        }
    }
}

impl<'de> Deserialize<'de> for Outcome {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
        enum Strict {
            Success {},
            Failure { code: FailureCode },
        }
        Ok(match Strict::deserialize(deserializer)? {
            Strict::Success {} => Self::Success,
            Strict::Failure { code } => Self::Failure { code },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub kind: String,
    pub index: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlGesture {
    Press,
    HoldStart,
    HoldEnd,
    LongPress,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlEvent {
    pub control: Control,
    pub gesture: ControlGesture,
}

impl Message {
    /// Every additive field and message must belong to the negotiated intersection.
    pub fn permitted(&self, capabilities: &[String]) -> bool {
        use crate::{
            CONTROLS, CREDENTIALS, DYNAMIC_OPTIONS, JPEG, SETTINGS, VISUAL_FRESHNESS, supports,
        };
        let has = |name| supports(capabilities, name);
        match self {
            Self::Invoke {
                settings,
                control,
                credential_revision,
                ..
            } => {
                (settings.is_none() || has(SETTINGS))
                    && (control.is_none() || has(CONTROLS))
                    && (credential_revision.is_none() || has(CREDENTIALS))
            }
            Self::Subscribe {
                settings,
                credential_revision,
                ..
            } => {
                has(VISUAL_FRESHNESS)
                    && (settings.is_none() || has(SETTINGS))
                    && (credential_revision.is_none() || has(CREDENTIALS))
            }
            Self::Options {
                settings,
                credential_revision,
                ..
            } => {
                has(DYNAMIC_OPTIONS)
                    && (settings.is_none() || has(SETTINGS))
                    && (credential_revision.is_none() || has(CREDENTIALS))
            }
            Self::Credentials { .. }
            | Self::CredentialsApplied { .. }
            | Self::CredentialsRelease { .. }
            | Self::CredentialsReleased { .. } => has(CREDENTIALS),
            Self::Choices { .. } => has(DYNAMIC_OPTIONS),
            Self::Visual { state, .. } => {
                has(VISUAL_FRESHNESS)
                    && match state {
                        VisualState::Current { content } | VisualState::Stale { content } => {
                            content.jpeg_hex.is_none() || has(JPEG)
                        }
                        _ => true,
                    }
            }
            Self::VisualRenew { .. } | Self::Unsubscribe { .. } => has(VISUAL_FRESHNESS),
            _ => true,
        }
    }
}

// Empty struct alternatives make serde reject fields on internally tagged unit variants.
impl<'de> Deserialize<'de> for ChoiceList {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
        enum Strict {
            Available { options: Vec<ChoiceOption> },
            Unavailable {},
        }
        Ok(match Strict::deserialize(deserializer)? {
            Strict::Available { options } => Self::Available { options },
            Strict::Unavailable {} => Self::Unavailable,
        })
    }
}
impl<'de> Deserialize<'de> for VisualState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
        enum Strict {
            Current { content: VisualContent },
            Stale { content: VisualContent },
            Unavailable {},
        }
        Ok(match Strict::deserialize(deserializer)? {
            Strict::Current { content } => Self::Current { content },
            Strict::Stale { content } => Self::Stale { content },
            Strict::Unavailable {} => Self::Unavailable,
        })
    }
}
