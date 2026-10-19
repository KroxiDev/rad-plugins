//! Immutable operation-scoped credentials. Keep sessions keyed by revision, not by a mutable map.
use crate::Error;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use zeroize::Zeroize;
pub const MAX_CREDENTIAL_REVISIONS: usize = 67;
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
pub(crate) fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
#[derive(Debug)]
struct Snapshot {
    revision: u64,
    values: Vec<CredentialValue>,
}
/// A frozen snapshot for one operation. Do not retain it in an idle external session:
/// store the session by revision and retire it in Plugin::credentials_released instead.
#[derive(Debug, Clone)]
pub struct Credentials(Arc<Snapshot>);
impl Credentials {
    pub fn revision(&self) -> u64 {
        self.0.revision
    }
    pub fn get(&self, slot: &str) -> Option<&str> {
        self.0
            .values
            .iter()
            .find(|v| v.slot == slot)
            .map(|v| v.value.as_str())
    }
}
pub(crate) struct Registry {
    slots: Vec<String>,
    last: u64,
    maps: BTreeMap<u64, Credentials>,
}
impl Registry {
    pub fn new(slots: Vec<String>) -> Result<Self, Error> {
        if slots.len() > 32
            || slots.iter().any(|s| !identifier(s))
            || slots
                .iter()
                .enumerate()
                .any(|(i, s)| slots[..i].contains(s))
        {
            return Err(Error::Protocol("invalid credential declaration"));
        }
        Ok(Self {
            slots,
            last: 0,
            maps: BTreeMap::new(),
        })
    }
    pub fn declared(&self) -> bool {
        !self.slots.is_empty()
    }
    pub fn install(&mut self, revision: u64, values: Vec<CredentialValue>) -> Result<(), Error> {
        if !self.declared()
            || revision <= self.last
            || self.maps.len() >= MAX_CREDENTIAL_REVISIONS
            || values.len() > 32
            || values
                .iter()
                .any(|v| !self.slots.contains(&v.slot) || v.value.len() > 4096)
            || values.windows(2).any(|w| w[0].slot >= w[1].slot)
            || values.iter().map(|v| v.value.len()).sum::<usize>() > 8192
        {
            return Err(Error::Protocol("invalid credential snapshot"));
        }
        self.maps.insert(
            revision,
            Credentials(Arc::new(Snapshot { revision, values })),
        );
        self.last = revision;
        Ok(())
    }
    pub fn acquire(&self, revision: Option<u64>) -> Result<Option<Credentials>, Error> {
        if !self.declared() && revision.is_none() {
            return Ok(None);
        }
        revision
            .and_then(|r| self.maps.get(&r))
            .cloned()
            .map(Some)
            .ok_or(Error::Protocol("unknown credential revision"))
    }
    pub fn release(&mut self, revision: u64) -> Result<(), Error> {
        if self
            .maps
            .get(&revision)
            .is_none_or(|v| Arc::strong_count(&v.0) != 1)
        {
            return Err(Error::Protocol(
                "credential revision still in use or unknown",
            ));
        }
        self.maps.remove(&revision);
        Ok(())
    }
}
