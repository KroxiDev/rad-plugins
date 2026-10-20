//! Start reserves its matching End before reaching user code. Identical Starts pair FIFO.
use crate::{
    Credentials, Error, Parameters,
    reply::{Permit, Slots},
};
use rad_plugin_contract::Control;
use std::sync::Arc;

pub(crate) const MAX_HOLDS: usize = 32;
pub(crate) struct Holds {
    slots: Arc<Slots>,
    active: Vec<Held>,
}
struct Held {
    control: Control,
    action: String,
    parameters: Parameters,
    settings: Option<Parameters>,
    revision: Option<u64>,
    _credentials: Option<Credentials>,
    permit: Permit,
}
impl Holds {
    pub fn new() -> Self {
        Self {
            slots: Slots::new(MAX_HOLDS),
            active: vec![],
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn reserve(
        &mut self,
        control: Control,
        action: &str,
        parameters: &Parameters,
        settings: &Option<Parameters>,
        revision: Option<u64>,
        credentials: Option<Credentials>,
    ) -> bool {
        let Some(permit) = self.slots.acquire() else {
            return false;
        };
        self.active.push(Held {
            control,
            action: action.into(),
            parameters: parameters.clone(),
            settings: settings.clone(),
            revision,
            _credentials: credentials,
            permit,
        });
        true
    }
    pub fn release(
        &mut self,
        control: &Control,
        action: &str,
        parameters: &Parameters,
        settings: &Option<Parameters>,
        revision: Option<u64>,
    ) -> Result<Permit, Error> {
        let index = self
            .active
            .iter()
            .position(|h| {
                &h.control == control
                    && h.action == action
                    && &h.parameters == parameters
                    && &h.settings == settings
                    && h.revision == revision
            })
            .ok_or(Error::Protocol("hold end without matching start"))?;
        Ok(self.active.remove(index).permit)
    }
}
