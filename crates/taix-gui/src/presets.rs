//! Pane layout presets: the shapes `tree::Node::from_preset` seeds.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Balanced,
    Columns,
    Rows,
    MainLeft,
    MainTop,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::Balanced,
        Preset::Columns,
        Preset::Rows,
        Preset::MainLeft,
        Preset::MainTop,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Preset::Balanced => "balanced",
            Preset::Columns => "columns",
            Preset::Rows => "rows",
            Preset::MainLeft => "main-left",
            Preset::MainTop => "main-top",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Preset::Balanced => "Balanced",
            Preset::Columns => "Columns",
            Preset::Rows => "Rows",
            Preset::MainLeft => "Main Left",
            Preset::MainTop => "Main Top",
        }
    }

    pub fn from_id(id: &str) -> Option<Preset> {
        match id {
            "balanced" => Some(Preset::Balanced),
            "columns" => Some(Preset::Columns),
            "rows" => Some(Preset::Rows),
            "main-left" => Some(Preset::MainLeft),
            "main-top" => Some(Preset::MainTop),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_round_trips_through_its_stored_id() {
        // The id is what lands in the layout file, so a preset whose id does
        // not parse back would silently reset the layout on the next launch.
        for preset in Preset::ALL {
            assert_eq!(Preset::from_id(preset.id()), Some(preset));
            assert!(!preset.label().is_empty());
        }
        assert_eq!(Preset::from_id("from-the-future"), None);
    }
}
