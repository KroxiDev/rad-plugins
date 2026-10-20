use crate::Error;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;
pub const MAX_CREDENTIAL_REVISIONS: usize = 67;
/// One plaintext slot. Debug is redacted; owned storage is wiped on drop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialValue {
    pub slot: String,
    pub value: String,
}
impl std::fmt::Debug for CredentialValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialValue([redacted])")
    }
}
impl Drop for CredentialValue {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}
/// Validate a canonical complete snapshot, not a patch. Empty means all slots absent.
pub fn validate_credentials(values: &[CredentialValue]) -> Result<(), Error> {
    if values.len() > 32
        || values
            .iter()
            .any(|v| !crate::manifest::identifier(&v.slot) || v.value.len() > 4096)
        || values.iter().map(|v| v.value.len()).sum::<usize>() > 8192
        || values.windows(2).any(|w| w[0].slot >= w[1].slot)
    {
        return Err(Error::InvalidMessage);
    }
    Ok(())
}
/// Validated snapshot suitable for sharing privately with an execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialValues(Vec<CredentialValue>);
impl CredentialValues {
    pub fn new(values: Vec<CredentialValue>) -> Result<Self, Error> {
        validate_credentials(&values)?;
        Ok(Self(values))
    }
    pub fn as_slice(&self) -> &[CredentialValue] {
        &self.0
    }
}
