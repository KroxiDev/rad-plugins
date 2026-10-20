//! Pure versioned plugin contracts; no processes, I/O, installation or permissions.
/// Current contract major. The pre-release linear APIs 2/3 are not majors.
pub const HOST_API: u32 = 1;
/// First published major; there is no preceding published major yet.
pub const HOST_API_MIN: u32 = 1;
pub const MAX_PLUGINS: usize = 32;
mod manifest;
mod parameters;
pub use manifest::*;
pub use parameters::Parameters;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    InvalidDocument,
    UnsupportedVersion,
    IncompatibleHost,
    IncompatibleTarget,
    InvalidManifest,
    InvalidParameters,
    CredentialsUnavailable,
    InvalidMessage,
    InvalidFrame,
    LimitExceeded,
}
pub struct Document(Vec<u8>);
impl Document {
    pub fn new(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > 65536 {
            return Err(Error::LimitExceeded);
        }
        Ok(Self(bytes.to_vec()))
    }
    pub fn parameters(&self, definition: &Definition) -> Result<(), Error> {
        self.response_budget_ms(definition).map(|_| ())
    }
    pub fn response_budget_ms(&self, definition: &Definition) -> Result<u64, Error> {
        if self.0.len() > 16384 {
            return Err(Error::LimitExceeded);
        }
        let value = rad_wire::parse_json(&self.0).map_err(|_| Error::InvalidDocument)?;
        let params: Parameters =
            serde_json::from_value(value).map_err(|_| Error::InvalidParameters)?;
        definition.response_budget_ms(&params)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn manifest(&self, host_api: u32, target: &str) -> Result<Manifest, Error> {
        let value = rad_wire::parse_json(&self.0).map_err(|_| Error::InvalidDocument)?;
        if value
            .get("contract")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|v| v != 1)
        {
            return Err(Error::UnsupportedVersion);
        }
        let manifest: Manifest =
            serde_json::from_value(value).map_err(|_| Error::InvalidManifest)?;
        manifest.validate(host_api, target)?;
        Ok(manifest)
    }
}
impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Document([redacted])")
    }
}

mod protocol;
pub use protocol::*;

mod bounded;

mod credentials;
pub use credentials::*;

mod capabilities;
pub use capabilities::*;
