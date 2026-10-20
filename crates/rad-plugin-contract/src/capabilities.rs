//! Additive features of contract major 1. An unrecognized offered name is harmless;
//! only the known intersection may appear in negotiated traffic.
use crate::Error;
pub const DYNAMIC_OPTIONS: &str = "dynamic_options";
pub const CREDENTIALS: &str = "credentials";
pub const JPEG: &str = "jpeg";
pub const RESPONSE_TIMEOUT: &str = "response_timeout";
pub const CONTROLS: &str = "controls";
pub const SETTINGS: &str = "settings";
pub const VISUAL_FRESHNESS: &str = "visual_freshness";
pub const SUPPORTED_CAPABILITIES: &[&str] = &[
    CONTROLS,
    CREDENTIALS,
    DYNAMIC_OPTIONS,
    JPEG,
    RESPONSE_TIMEOUT,
    SETTINGS,
    VISUAL_FRESHNESS,
];
pub fn validate_capabilities(values: &[String]) -> Result<(), Error> {
    if values.len() > 32
        || values.iter().any(|v| !crate::manifest::identifier(v))
        || values
            .iter()
            .enumerate()
            .any(|(i, v)| values[..i].contains(v))
    {
        return Err(Error::InvalidMessage);
    }
    Ok(())
}
pub fn negotiate_capabilities(
    offered: &[String],
    supported: &[String],
) -> Result<Vec<String>, Error> {
    validate_capabilities(offered)?;
    validate_capabilities(supported)?;
    Ok(SUPPORTED_CAPABILITIES
        .iter()
        .filter(|name| offered.iter().any(|v| v == **name) && supported.iter().any(|v| v == **name))
        .map(|s| (*s).to_owned())
        .collect())
}
pub fn supports(capabilities: &[String], capability: &str) -> bool {
    capabilities.iter().any(|c| c == capability)
}
