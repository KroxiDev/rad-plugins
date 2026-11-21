use crate::{polling::Sample, readings::Memory};
use rad_plugin_sdk::{Parameters, VisualContent};

/// The metric a visual shows, declared in the order of `Demand::sources`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Cpu,
    Ram,
    Gpu,
    Vram,
}

impl Source {
    pub(crate) const ALL: [Self; 4] = [Self::Cpu, Self::Ram, Self::Gpu, Self::Vram];

    /// The source of a visual by its manifest id.
    pub(crate) fn from_visual(id: &str) -> Option<Self> {
        match id {
            "cpu" => Some(Self::Cpu),
            "ram" => Some(Self::Ram),
            "gpu" => Some(Self::Gpu),
            "vram" => Some(Self::Vram),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Ram => "RAM",
            Self::Gpu => "GPU",
            Self::Vram => "VRAM",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Style {
    #[default]
    Text,
    Bar,
}

#[derive(Clone, Copy, Default)]
enum MemoryValue {
    #[default]
    UsedTotal,
    Used,
    Percent,
}

#[derive(Default)]
pub(crate) struct Presentation {
    style: Style,
    value: MemoryValue,
    hide_name: bool,
}

impl Presentation {
    pub(crate) fn parse(source: Source, parameters: &Parameters) -> Option<Self> {
        if parameters.schema_version != 1 {
            return None;
        }
        let mut presentation = Self::default();
        for (key, value) in &parameters.values {
            match (key.as_str(), value.as_str()?) {
                ("style", "text") | ("label", "shown") => {}
                ("style", "bar") => presentation.style = Style::Bar,
                ("label", "hidden") => presentation.hide_name = true,
                ("value", choice) if matches!(source, Source::Ram | Source::Vram) => {
                    presentation.value = match choice {
                        "used_total" => MemoryValue::UsedTotal,
                        "used" => MemoryValue::Used,
                        "percent" => MemoryValue::Percent,
                        _ => return None,
                    };
                }
                _ => return None,
            }
        }
        Some(presentation)
    }

    pub(crate) fn content(&self, source: Source, sample: &Sample) -> Option<VisualContent> {
        let utilization = |percent: Option<u32>| {
            let percent = percent.filter(|v| *v <= 100)?;
            Some((format!("{percent}%"), percent))
        };
        let (value, percent) = match source {
            Source::Cpu => utilization(sample.cpu)?,
            Source::Ram => self.memory_value(sample.ram?)?,
            Source::Gpu => utilization(sample.gpu)?,
            Source::Vram => self.memory_value(sample.vram?)?,
        };
        let text = if self.hide_name {
            value
        } else {
            format!("{} {value}", source.name())
        };
        Some(VisualContent {
            text: Some(text),
            png_hex: match self.style {
                Style::Text => None,
                Style::Bar => Some(crate::bar::png_hex(percent)?),
            },
            ..Default::default()
        })
    }

    fn memory_value(&self, memory: Memory) -> Option<(String, u32)> {
        let Memory {
            used,
            available,
            total,
        } = memory;
        if total == 0 || used.checked_add(available)? != total {
            return None;
        }
        let percent = ((u128::from(used) * 100 + u128::from(total / 2)) / u128::from(total)) as u32;
        const GIB: f64 = 1_073_741_824.0;
        let value = match self.value {
            MemoryValue::UsedTotal => {
                format!("{:.1}/{:.1} GiB", used as f64 / GIB, total as f64 / GIB)
            }
            MemoryValue::Used => format!("{:.1} GiB", used as f64 / GIB),
            MemoryValue::Percent => format!("{percent}%"),
        };
        Some((value, percent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readings::Memory;
    use serde_json::{Value, json};

    fn fixture() -> Sample {
        let gib = 1 << 30;
        Sample {
            cpu: Some(25),
            ram: Some(Memory {
                used: 8 * gib,
                available: 8 * gib,
                total: 16 * gib,
            }),
            gpu: Some(73),
            vram: Some(Memory {
                used: 2 * gib,
                available: 6 * gib,
                total: 8 * gib,
            }),
        }
    }

    fn parameters(values: Value) -> Parameters {
        serde_json::from_value(json!({"schema_version":1, "values":values})).unwrap()
    }

    fn text(source: Source, values: Value, sample: &Sample) -> Option<String> {
        Presentation::parse(source, &parameters(values))?
            .content(source, sample)?
            .text
    }

    #[test]
    fn defaults_fit_existing_keys_and_show_memory_capacity() {
        for (source, expected) in Source::ALL.into_iter().zip([
            "CPU 25%",
            "RAM 8.0/16.0 GiB",
            "GPU 73%",
            "VRAM 2.0/8.0 GiB",
        ]) {
            let content = Presentation::parse(source, &parameters(json!({})))
                .unwrap()
                .content(source, &fixture())
                .unwrap();
            assert_eq!(content.text.as_deref(), Some(expected));
            assert_eq!(content.png_hex, None);
        }
    }

    #[test]
    fn value_and_name_can_be_chosen_independently() {
        for (source, name, variants) in [
            (Source::Ram, "RAM", ["8.0/16.0 GiB", "8.0 GiB", "50%"]),
            (Source::Vram, "VRAM", ["2.0/8.0 GiB", "2.0 GiB", "25%"]),
        ] {
            for (value, expected) in ["used_total", "used", "percent"].into_iter().zip(variants) {
                for (label, expected) in [
                    ("shown", format!("{name} {expected}")),
                    ("hidden", expected.to_owned()),
                ] {
                    assert_eq!(
                        text(source, json!({"value":value,"label":label}), &fixture()),
                        Some(expected)
                    );
                }
            }
        }
        for (source, expected) in [(Source::Cpu, "25%"), (Source::Gpu, "73%")] {
            assert_eq!(
                text(source, json!({"style":"text","label":"hidden"}), &fixture()).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn memory_percentage_rounds_to_nearest_and_accepts_full_width_counters() {
        for (used, total, expected) in [
            (1, 8, "RAM 13%"),
            (1, 3, "RAM 33%"),
            (u64::MAX, u64::MAX, "RAM 100%"),
        ] {
            let mut sample = fixture();
            sample.ram = Some(Memory {
                used,
                available: total - used,
                total,
            });
            assert_eq!(
                text(Source::Ram, json!({"value":"percent"}), &sample).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn invalid_parameters_and_incoherent_memory_produce_no_content() {
        for (source, values) in [
            (Source::Ram, json!({"extra": "x"})),
            (Source::Vram, json!({"style":"ring"})),
            (Source::Ram, json!({"label": false})),
            (Source::Ram, json!({"value":"unknown"})),
            (Source::Cpu, json!({"value":"percent"})),
            (Source::Gpu, json!({"value":"used"})),
        ] {
            assert!(Presentation::parse(source, &parameters(values)).is_none());
        }
        for (used, available, total) in [(0, 0, 0), (1, 1, 3), (u64::MAX, 1, u64::MAX)] {
            let mut sample = fixture();
            sample.ram = Some(Memory {
                used,
                available,
                total,
            });
            assert!(text(Source::Ram, json!({}), &sample).is_none());
        }
    }
}
