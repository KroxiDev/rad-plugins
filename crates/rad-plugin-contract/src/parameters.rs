use crate::{Definition, Error, FieldKind};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Ordinary parameters only. Credential values have no representation here.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub schema_version: u32,
    pub values: Map<String, Value>,
}
impl std::fmt::Debug for Parameters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Parameters([redacted])")
    }
}
impl Parameters {
    pub fn text(&self, key: &str) -> Option<&str> {
        self.values.get(key).and_then(Value::as_str)
    }
    pub(crate) fn check_shape(&self) -> Result<(), Error> {
        if self.schema_version == 0
            || self.values.len() > 32
            || self.values.iter().any(|(key, value)| {
                !crate::manifest::identifier(key)
                    || !(value.is_boolean()
                        || value.as_i64().is_some()
                        || value.as_str().is_some_and(|s| s.len() <= 4096))
            })
        {
            return Err(Error::InvalidParameters);
        }
        crate::bounded::json(self, 16384)?;
        Ok(())
    }
    pub fn validate(&self, definition: &Definition) -> Result<(), Error> {
        definition.validate()?;
        self.check_shape()?;
        if self.schema_version != definition.schema.version {
            return Err(Error::UnsupportedVersion);
        }
        if self
            .values
            .keys()
            .any(|k| !definition.schema.fields.iter().any(|f| &f.id == k))
        {
            return Err(Error::InvalidParameters);
        }
        for field in &definition.schema.fields {
            let Some(value) = self.values.get(&field.id) else {
                if field.required {
                    return Err(Error::InvalidParameters);
                } else {
                    continue;
                }
            };
            let valid = match &field.kind {
                FieldKind::Boolean => value.is_boolean(),
                FieldKind::Integer { min, max } => {
                    value.as_i64().is_some_and(|n| (*min..=*max).contains(&n))
                }
                FieldKind::Text { max_bytes } => {
                    value.as_str().is_some_and(|s| s.len() <= *max_bytes)
                }
                // The list is dynamic and may be unavailable: no membership check here.
                FieldKind::DynamicChoice { .. } => {
                    value.as_str().is_some_and(crate::manifest::label)
                }
                FieldKind::Choice { options } => value
                    .as_str()
                    .is_some_and(|s| options.iter().any(|o| o.value == s)),
            };
            if !valid {
                return Err(Error::InvalidParameters);
            }
        }
        Ok(())
    }
}
