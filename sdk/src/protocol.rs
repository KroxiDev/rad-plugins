//! Shared wire types: the host and SDK compile the same strict contract.
pub use rad_plugin_contract::{
    ChoiceList, ChoiceOption, Control, ControlEvent, ControlGesture, Correlation, Envelope,
    FailureCode, Gesture, MAX_CHOICES, Message, OptionsScope, Outcome, Parameters, Version,
    VisualContent, VisualState,
};
pub const PROTOCOL: u32 = 1;
pub const MAX_LABEL: usize = 256;
pub const MAX_IMAGE_HEX: usize = 16_384;
pub fn is_label(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_LABEL && !text.chars().any(char::is_control)
}
