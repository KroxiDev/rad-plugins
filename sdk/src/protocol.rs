//! Message types of the plugin protocol (JSON inside u32-BE frames, `protocol: 1`).
//!
//! Hosts and plugins exchange [`Envelope`]s. Replies and visual updates carry the correlation of
//! the message they answer; the handshake always uses request 1.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Protocol version carried by every envelope.
pub const PROTOCOL: u32 = 1;
/// Longest visible text, option value or option label, in bytes.
pub const MAX_LABEL: usize = 256;
/// Most options a single `choices` reply may carry.
pub const MAX_CHOICES: usize = 64;
/// Longest `png_hex`/`jpeg_hex`, in characters (8 KiB of image).
pub const MAX_IMAGE_HEX: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// Ties a reply to its request; `generation` identifies the plugin process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correlation {
    pub generation: u64,
    pub request: u64,
}

/// Values of an action or visual source, already validated by the host against the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub schema_version: u32,
    pub values: Map<String, Value>,
}
impl Parameters {
    /// The string stored under `key`, if the value exists and is a string.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.values.get(key).and_then(Value::as_str)
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
        host_api: u32,
        target: String,
        plugin_id: String,
        plugin_version: Version,
    },
    Ready {
        plugin_id: String,
        plugin_version: Version,
    },
    Invoke {
        action: String,
        parameters: Parameters,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_revision: Option<u64>,
    },
    Result {
        outcome: Outcome,
    },
    Subscribe {
        source: String,
        parameters: Parameters,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_revision: Option<u64>,
    },
    Unsubscribe {
        subscription: Correlation,
    },
    Visual {
        state: VisualState,
    },
    Options {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure { code: FailureCode },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    InvalidParameters,
    /// The thing the action targets does not exist right now.
    Unavailable,
    Failed,
    TimedOut,
    /// The plugin is already running its maximum number of actions.
    Busy,
    Incompatible,
    CredentialsUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum VisualState {
    Current { content: VisualContent },
    Stale { content: VisualContent },
    Unavailable,
}

/// What a visual source shows. At least one of `text`, an image or `color` is required; an image
/// is `png_hex` or `jpeg_hex`, never both (lowercase hex, even length, up to 16384 characters).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualContent {
    pub text: Option<String>,
    pub png_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jpeg_hex: Option<String>,
    /// RGBA.
    pub color: Option<[u8; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionsScope {
    Action,
    Visual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum ChoiceList {
    /// A real answer; it may be empty.
    Available { options: Vec<ChoiceOption> },
    /// The source of the options does not exist right now.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    /// What the host stores.
    pub value: String,
    /// What the user sees.
    pub label: String,
}

/// Non-empty, at most [`MAX_LABEL`] bytes and free of control characters.
pub fn is_label(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_LABEL && !text.chars().any(char::is_control)
}

impl ChoiceList {
    /// Whether the host would accept this reply: up to [`MAX_CHOICES`] options, valid labels and
    /// values, no repeated value.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Unavailable => true,
            Self::Available { options } => {
                options.len() <= MAX_CHOICES
                    && options
                        .iter()
                        .all(|o| is_label(&o.value) && is_label(&o.label))
                    && options
                        .iter()
                        .enumerate()
                        .all(|(i, o)| options[..i].iter().all(|earlier| earlier.value != o.value))
            }
        }
    }
}

impl VisualContent {
    /// Whether the host would accept this content.
    pub fn is_valid(&self) -> bool {
        fn hex(s: &str) -> bool {
            !s.is_empty()
                && s.len() <= MAX_IMAGE_HEX
                && s.len().is_multiple_of(2)
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        (self.text.is_some()
            || self.png_hex.is_some()
            || self.jpeg_hex.is_some()
            || self.color.is_some())
            && !(self.png_hex.is_some() && self.jpeg_hex.is_some())
            && self.text.as_deref().is_none_or(is_label)
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
