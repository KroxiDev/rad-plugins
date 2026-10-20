//! The options of the `player` field: the applications that can be picked.
use crate::{Timing, bus::Bus, names::app_identity, player};
use rad_plugin_sdk::{ChoiceList, ChoiceOption, MAX_CHOICES, MAX_LABEL};
use std::{collections::BTreeMap, time::Instant};

/// The applications the product is expected to be used with. They are offered while closed too, so
/// a button can be set up before the player is started.
const EXPECTED: [(&str, &str); 2] = [("brave", "Brave"), ("strawberry", "Strawberry")];

/// A label the host accepts: control characters become spaces, runs of spaces collapse, and the
/// text is cut at a character boundary. `None` if nothing is left.
fn clean_label(text: &str) -> Option<String> {
    let spaced: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut label = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.len() > MAX_LABEL {
        let cut = (0..=MAX_LABEL)
            .rev()
            .find(|&i| label.is_char_boundary(i))
            .unwrap_or(0);
        label.truncate(cut);
        label.truncate(label.trim_end().len());
    }
    (!label.is_empty()).then_some(label)
}

/// The applications that are running, then the expected ones that are not, at most
/// [`MAX_CHOICES`] in all. `Unavailable` if the bus cannot be asked.
pub(crate) fn player_options(bus: &Bus, timing: &Timing, deadline: Instant) -> ChoiceList {
    let Ok(_connection) = bus.connection(deadline) else {
        return ChoiceList::Unavailable;
    };
    let names = match bus.player_names(deadline) {
        Ok(names) => names,
        Err(_) => return ChoiceList::Unavailable,
    };
    // Per application, the instance the plugin would use.
    let mut running: BTreeMap<&str, &str> = BTreeMap::new();
    for name in names.iter().map(String::as_str) {
        if let Some(identity) = app_identity(name) {
            let first = running.entry(identity).or_insert(name);
            *first = (*first).min(name);
        }
    }
    let closed: Vec<ChoiceOption> = EXPECTED
        .iter()
        .filter(|(value, _)| !running.contains_key(value))
        .map(|(value, label)| ChoiceOption {
            value: (*value).to_owned(),
            label: format!("{label} (not running)"),
        })
        .collect();
    let mut options: Vec<ChoiceOption> = running
        .iter()
        .take(MAX_CHOICES - closed.len())
        .map(|(identity, name)| {
            let asked = Instant::now() + timing.label_deadline;
            let label = player::identity(bus, name, asked.min(deadline))
                .ok()
                .and_then(|text| clean_label(&text))
                .unwrap_or_else(|| (*identity).to_owned());
            ChoiceOption {
                value: (*identity).to_owned(),
                label,
            }
        })
        .collect();
    options.extend(closed);
    ChoiceList::Available { options }
}
