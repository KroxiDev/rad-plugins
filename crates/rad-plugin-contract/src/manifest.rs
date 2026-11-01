use crate::{ChoiceOption, Error, HOST_API_MIN};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}
/// Static developer policy, known before a plugin process is launched.
/// Omission is on-demand and serializes unchanged for legacy manifests. Explicit
/// policies are rejected by old strict readers, never silently treated as eager.
/// Unknown values are invalid; always-loaded is a developer opt-in, not a user setting.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginLifecycle {
    #[default]
    OnDemand,
    AlwaysLoaded,
}
impl PluginLifecycle {
    fn is_on_demand(&self) -> bool {
        *self == Self::OnDemand
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "PluginLifecycle::is_on_demand")]
    pub lifecycle: PluginLifecycle,
    pub contract: u32,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<Resource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Schema>,
    pub id: String,
    pub version: Version,
    pub host_min: u32,
    pub host_max: u32,
    pub targets: Vec<String>,
    pub actions: Vec<Definition>,
    pub visuals: Vec<Definition>,
    /// From the `labels` capability: the plugin's own texts in other languages, keyed by
    /// language and then by the text as the manifest writes it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub translations: BTreeMap<String, BTreeMap<String, String>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<VisualStateDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual: Option<String>,
    #[serde(default = "default_gestures")]
    pub gestures: Vec<Gesture>,
    pub id: String,
    pub label: String,
    pub schema: Schema,
    pub credential_slots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_timeout: Option<ResponseTimeout>,
}
/// Ordered binary states; the second is shown when the source is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualStateDefinition {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}
fn valid_icon(hex: &str) -> bool {
    if hex.is_empty()
        || hex.len() > 16384
        || !hex.len().is_multiple_of(2)
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return false;
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("checked hex"))
        .collect();
    let decoder = png::Decoder::new_with_limits(
        std::io::Cursor::new(bytes),
        png::Limits { bytes: 1024 * 1024 },
    );
    let Ok(mut reader) = decoder.read_info() else {
        return false;
    };
    let info = reader.info();
    if info.width == 0
        || info.width > 256
        || info.width != info.height
        || info.animation_control.is_some()
    {
        return false;
    }
    let Some(size) = reader.output_buffer_size().filter(|n| *n <= 1024 * 1024) else {
        return false;
    };
    reader.next_frame(&mut vec![0; size]).is_ok() && reader.finish().is_ok()
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTimeout {
    /// Optional integer field that lets the user change the budget; without one the
    /// action always has `default_seconds`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub default_seconds: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    pub version: u32,
    pub fields: Vec<Field>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub id: String,
    pub label: String,
    pub required: bool,
    pub kind: FieldKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldKind {
    Boolean,
    Integer {
        min: i64,
        max: i64,
    },
    Text {
        max_bytes: usize,
    },
    Choice {
        #[serde(serialize_with = "fixed_options")]
        options: Vec<ChoiceOption>,
    },
    DynamicChoice {
        depends_on: Vec<String>,
    },
}
/// A fixed option whose label is its value keeps the plain form older readers expect.
fn fixed_options<S: serde::Serializer>(
    options: &[ChoiceOption],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(options.len()))?;
    for option in options {
        if option.label == option.value {
            seq.serialize_element(&option.value)?;
        } else {
            seq.serialize_element(option)?;
        }
    }
    seq.end()
}

pub(crate) fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
pub(crate) fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}
fn unique<'a>(items: impl Iterator<Item = &'a str>) -> bool {
    let mut seen = BTreeSet::new();
    items.into_iter().all(|s| seen.insert(s))
}
impl Manifest {
    pub fn validate(&self, host_api: u32, target: &str) -> Result<(), Error> {
        if self.contract != 1 {
            return Err(Error::UnsupportedVersion);
        }
        if self.resources.len() > 5
            || self
                .resources
                .iter()
                .enumerate()
                .any(|(i, r)| self.resources[..i].contains(r))
        {
            return Err(Error::InvalidManifest);
        }
        if !identifier(&self.id)
            || self.host_min == 0
            || self.host_min > self.host_max
            || self.targets.is_empty()
            || self.targets.len() > 16
            || !self.targets.iter().all(|t| identifier(t))
            || !unique(self.targets.iter().map(String::as_str))
            || self.actions.len() > 64
            || self.visuals.len() > 64
            || self.actions.is_empty() && self.visuals.is_empty()
        {
            return Err(Error::InvalidManifest);
        }
        for definitions in [&self.actions, &self.visuals] {
            if !unique(definitions.iter().map(|d| d.id.as_str())) {
                return Err(Error::InvalidManifest);
            }
            for definition in definitions {
                definition.validate()?;
                // Dynamic options exist only from API 2: older hosts must never load them.
                if definition.has_dynamic_choice()
                    && !crate::supports(&self.capabilities, crate::DYNAMIC_OPTIONS)
                {
                    return Err(Error::InvalidManifest);
                }
            }
        }
        if self.visuals.iter().any(|d| d.response_timeout.is_some())
            || (!crate::supports(&self.capabilities, crate::RESPONSE_TIMEOUT)
                && self.actions.iter().any(|d| d.response_timeout.is_some()))
        {
            return Err(Error::InvalidManifest);
        }
        if self.actions.iter().any(|d| !d.states.is_empty())
            || self.visuals.iter().any(|d| d.visual.is_some())
            || (!crate::supports(&self.capabilities, crate::VISUAL_STATES)
                && self
                    .actions
                    .iter()
                    .chain(&self.visuals)
                    .any(|d| d.visual.is_some() || !d.states.is_empty()))
        {
            return Err(Error::InvalidManifest);
        }
        for action in &self.actions {
            if let Some(id) = &action.visual {
                let visual = self
                    .visuals
                    .iter()
                    .find(|v| &v.id == id)
                    .ok_or(Error::InvalidManifest)?;
                if visual.states.len() != 2
                    || visual.schema.fields.iter().any(|field| {
                        !action.schema.fields.iter().any(|a| {
                            a.id == field.id
                                && a.kind == field.kind
                                && (!field.required || a.required)
                        })
                    })
                {
                    return Err(Error::InvalidManifest);
                }
            }
        }
        let slots = self.credential_slots();
        if slots.len() > 32
            || (!slots.is_empty() && !crate::supports(&self.capabilities, crate::CREDENTIALS))
        {
            return Err(Error::InvalidManifest);
        }
        crate::validate_capabilities(&self.capabilities).map_err(|_| Error::InvalidManifest)?;
        if !self.visuals.is_empty() && !crate::supports(&self.capabilities, crate::VISUAL_FRESHNESS)
        {
            return Err(Error::InvalidManifest);
        }
        if self
            .actions
            .iter()
            .any(|d| d.gestures != default_gestures())
            && !crate::supports(&self.capabilities, crate::CONTROLS)
        {
            return Err(Error::InvalidManifest);
        }
        if let Some(schema) = &self.settings {
            if !crate::supports(&self.capabilities, crate::SETTINGS) {
                return Err(Error::InvalidManifest);
            }
            Definition {
                id: "settings".into(),
                label: "Settings".into(),
                schema: schema.clone(),
                gestures: default_gestures(),
                credential_slots: vec![],
                states: vec![],
                visual: None,
                response_timeout: None,
            }
            .validate()?;
        }
        if (self.labelled_options() || !self.translations.is_empty())
            && !crate::supports(&self.capabilities, crate::LABELS)
        {
            return Err(Error::InvalidManifest);
        }
        if self.translations.len() > 16
            || self.translations.iter().any(|(language, texts)| {
                !(2..=3).contains(&language.len())
                    || !language.bytes().all(|b| b.is_ascii_lowercase())
                    || texts.len() > 512
                    || texts
                        .iter()
                        .any(|(text, translated)| !label(text) || !label(translated))
            })
        {
            return Err(Error::InvalidManifest);
        }
        crate::bounded::json(self, 65536)?;
        if self.host_min.max(HOST_API_MIN) > self.host_max.min(host_api) {
            return Err(Error::IncompatibleHost);
        }
        if !self.targets.iter().any(|t| t == target) {
            return Err(Error::IncompatibleTarget);
        }
        Ok(())
    }
}
impl Manifest {
    /// `text` as the plugin writes it for `language`, or the text itself when the plugin
    /// brings no translation for it.
    pub fn translate<'a>(&'a self, language: &str, text: &'a str) -> &'a str {
        self.translations
            .get(language)
            .and_then(|texts| texts.get(text))
            .map_or(text, String::as_str)
    }
    /// Some fixed option shows the user a text other than its stored value.
    fn labelled_options(&self) -> bool {
        self.actions
            .iter()
            .chain(&self.visuals)
            .map(|d| &d.schema)
            .chain(&self.settings)
            .flat_map(|schema| &schema.fields)
            .any(|field| match &field.kind {
                FieldKind::Choice { options } => options.iter().any(|o| o.label != o.value),
                _ => false,
            })
    }
    /// API used with this plugin: the newest both sides speak. Meaningful for a
    /// manifest that `validate` accepted against the same `host_api`.
    /// Union of slots this process is allowed to receive, in canonical ASCII order.
    pub fn credential_slots(&self) -> BTreeSet<&str> {
        self.actions
            .iter()
            .chain(&self.visuals)
            .flat_map(|d| d.credential_slots.iter().map(String::as_str))
            .collect()
    }
    pub fn negotiated_api(&self, host_api: u32) -> u32 {
        host_api.min(self.host_max)
    }
}
impl Definition {
    /// Validated per-action response budget; legacy actions retain 5 seconds.
    pub fn response_budget_ms(&self, parameters: &crate::Parameters) -> Result<u64, Error> {
        parameters.validate(self)?;
        Ok(match &self.response_timeout {
            None => 5000,
            Some(timeout) => {
                timeout
                    .field
                    .as_ref()
                    .and_then(|field| parameters.values.get(field))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(u64::from(timeout.default_seconds))
                    * 1000
            }
        })
    }
    pub(crate) fn has_dynamic_choice(&self) -> bool {
        self.schema
            .fields
            .iter()
            .any(|f| matches!(f.kind, FieldKind::DynamicChoice { .. }))
    }
    /// Pure check of an options query: `field` must be a `dynamic_choice`, and
    /// `context` may carry only values of its `depends_on` fields, each valid for
    /// its kind. Dynamic values are not looked up in any list.
    pub fn validate_options_context(
        &self,
        field: &str,
        context: &crate::Parameters,
    ) -> Result<(), Error> {
        let fields = &self.schema.fields;
        let Some(FieldKind::DynamicChoice { depends_on }) =
            fields.iter().find(|f| f.id == field).map(|f| &f.kind)
        else {
            return Err(Error::InvalidParameters);
        };
        context.check_shape()?;
        if context.schema_version != self.schema.version {
            return Err(Error::UnsupportedVersion);
        }
        for (key, value) in &context.values {
            let kind = depends_on
                .contains(key)
                .then(|| fields.iter().find(|f| &f.id == key).map(|f| &f.kind))
                .flatten();
            let valid = match (kind, value.as_str()) {
                (Some(FieldKind::Choice { options }), Some(s)) => {
                    options.iter().any(|o| o.value == s)
                }
                (Some(FieldKind::DynamicChoice { .. }), Some(s)) => label(s),
                _ => false,
            };
            if !valid {
                return Err(Error::InvalidParameters);
            }
        }
        Ok(())
    }
    /// Whether a reply names a state declared by this visual (legacy replies may omit it).
    pub fn accepts_state(&self, state: &crate::VisualState) -> bool {
        match state {
            crate::VisualState::Unavailable => true,
            crate::VisualState::Current { content } | crate::VisualState::Stale { content } => {
                content
                    .state
                    .as_ref()
                    .is_none_or(|id| self.states.iter().any(|s| &s.id == id))
            }
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.visual.as_deref().is_some_and(|id| !identifier(id))
            || (!self.states.is_empty()
                && (self.states.len() != 2
                    || !unique(self.states.iter().map(|s| s.id.as_str()))
                    || self.states.iter().any(|s| {
                        !identifier(&s.id)
                            || !label(&s.label)
                            || s.icon.as_deref().is_some_and(|icon| !valid_icon(icon))
                    })))
        {
            return Err(Error::InvalidManifest);
        }
        if self.gestures.is_empty()
            || self.gestures.len() > 3
            || self
                .gestures
                .iter()
                .enumerate()
                .any(|(i, g)| self.gestures[..i].contains(g))
        {
            return Err(Error::InvalidManifest);
        }
        if !identifier(&self.id)
            || !label(&self.label)
            || self.schema.version == 0
            || self.schema.fields.len() > 32
            || self.credential_slots.len() > 32
            || !unique(self.schema.fields.iter().map(|f| f.id.as_str()))
            || !unique(self.credential_slots.iter().map(String::as_str))
            || !self.credential_slots.iter().all(|s| identifier(s))
        {
            return Err(Error::InvalidManifest);
        }
        if let Some(timeout) = &self.response_timeout
            && (!(1..=120).contains(&timeout.default_seconds)
                || timeout.field.as_ref().is_some_and(|field| {
                    !self.schema.fields.iter().any(|f| {
                        &f.id == field
                            && !f.required
                            && matches!(f.kind, FieldKind::Integer { min: 1, max: 120 })
                    })
                }))
        {
            return Err(Error::InvalidManifest);
        }
        for (index, field) in self.schema.fields.iter().enumerate() {
            if !identifier(&field.id)
                || !label(&field.label)
                || self.credential_slots.contains(&field.id)
            {
                return Err(Error::InvalidManifest);
            }
            let valid = match &field.kind {
                FieldKind::Boolean => true,
                FieldKind::Integer { min, max } => min <= max,
                FieldKind::Text { max_bytes } => (1..=4096).contains(max_bytes),
                FieldKind::DynamicChoice { depends_on } => {
                    // Earlier fields only: dependencies are acyclic by construction.
                    depends_on.len() <= 4
                        && unique(depends_on.iter().map(String::as_str))
                        && depends_on.iter().all(|dependency| {
                            self.schema.fields[..index].iter().any(|f| {
                                &f.id == dependency
                                    && matches!(
                                        f.kind,
                                        FieldKind::Choice { .. } | FieldKind::DynamicChoice { .. }
                                    )
                            })
                        })
                }
                FieldKind::Choice { options } => {
                    !options.is_empty()
                        && options.len() <= 64
                        && options.iter().all(|o| label(&o.value) && label(&o.label))
                        && unique(options.iter().map(|o| o.value.as_str()))
                }
            };
            if !valid {
                return Err(Error::InvalidManifest);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gesture {
    Press,
    Hold,
    LongPress,
}
pub fn default_gestures() -> Vec<Gesture> {
    vec![Gesture::Press]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    Internet,
    SystemPrograms,
    ExternalFiles,
    Devices,
    DesktopServices,
}

impl<'de> Deserialize<'de> for FieldKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum Strict {
            Boolean {},
            Integer { min: i64, max: i64 },
            Text { max_bytes: usize },
            Choice { options: Vec<FixedOption> },
            DynamicChoice { depends_on: Vec<String> },
        }
        /// A plain value, or a value with the text the user reads (`labels` capability).
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum FixedOption {
            Plain(String),
            Labelled(ChoiceOption),
        }
        Ok(match Strict::deserialize(deserializer)? {
            Strict::Boolean {} => Self::Boolean,
            Strict::Integer { min, max } => Self::Integer { min, max },
            Strict::Text { max_bytes } => Self::Text { max_bytes },
            Strict::Choice { options } => Self::Choice {
                options: options
                    .into_iter()
                    .map(|option| match option {
                        FixedOption::Plain(value) => ChoiceOption {
                            label: value.clone(),
                            value,
                        },
                        FixedOption::Labelled(option) => option,
                    })
                    .collect(),
            },
            Strict::DynamicChoice { depends_on } => Self::DynamicChoice { depends_on },
        })
    }
}
