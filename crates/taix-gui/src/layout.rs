//! Divider positions, remembered across runs.
//!
//! A resizable layout the user has to re-drag on every launch is not really
//! resizable. Two integers in a flat key=value file — deliberately not TOML,
//! not the SQLite store: this is throwaway window state, and losing it must
//! never be able to fail a startup.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub sidebar: i32,
    /// Width of the file tree, not the divider's position: the divider moves
    /// with the window, the panel's width is what the user chose.
    pub files: i32,
    /// Projects the user folded away in the sidebar.
    pub folded: Vec<i64>,
    /// Projects whose agents must not raise a desktop notification.
    pub muted: Vec<i64>,
    /// `presets::Preset::id` of the pane arrangement in use. A string, so an
    /// unknown value from a newer build degrades to the default instead of
    /// failing to parse.
    pub preset: String,
    /// Per-project pane trees the user has arranged by hand, as
    /// `tree::Node::encode` text. Absent means "the preset's shape".
    pub trees: Vec<(i64, String)>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            sidebar: 262,
            files: 280,
            folded: Vec::new(),
            muted: Vec::new(),
            preset: "balanced".to_string(),
            trees: Vec::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/share")))?;
    Some(data.join("taix/layout"))
}

impl Layout {
    pub fn load() -> Layout {
        let text = path().and_then(|p| std::fs::read_to_string(p).ok());
        text.as_deref().map(parse).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let ids = |v: &[i64]| v.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let trees: String = self
            .trees
            .iter()
            .map(|(id, t)| format!("tree.{id}={t}\n"))
            .collect();
        let _ = std::fs::write(
            path,
            format!(
                "sidebar={}\nfiles={}\nfolded={}\nmuted={}\npreset={}\n{trees}",
                self.sidebar,
                self.files,
                ids(&self.folded),
                ids(&self.muted),
                self.preset,
            ),
        );
    }
}

/// Unknown keys, blank lines and garbage values are ignored rather than
/// failing: a corrupt state file must degrade to the default layout.
fn parse(text: &str) -> Layout {
    let mut layout = Layout::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            // A project that has since been removed simply never matches.
            "folded" | "muted" => {
                let ids = value.split(',').filter_map(|id| id.parse().ok()).collect();
                if key.trim() == "folded" {
                    layout.folded = ids;
                } else {
                    layout.muted = ids;
                }
                continue;
            }
            // Validated where it is used, not here: this file must never
            // reject a value, only fail to understand it.
            "preset" if !value.is_empty() => {
                layout.preset = value.to_string();
                continue;
            }
            key => {
                if let Some(id) = key.strip_prefix("tree.").and_then(|id| id.parse().ok())
                    && !value.is_empty()
                {
                    layout.trees.push((id, value.to_string()));
                    continue;
                }
            }
        }
        let Ok(value) = value.parse::<i32>() else {
            continue;
        };
        match key.trim() {
            // A stored zero or negative would collapse the pane on restore.
            "sidebar" if value > 0 => layout.sidebar = value,
            "files" if value > 0 => layout.files = value,
            _ => {}
        }
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_file_format() {
        let saved = Layout {
            sidebar: 310,
            files: 300,
            folded: vec![2, 7],
            muted: vec![4],
            preset: "main-left".to_string(),
            trees: vec![(3, "h[1,v[2,3]]".to_string())],
        };
        let text = format!(
            "sidebar={}\nfiles={}\nfolded=2,7\nmuted=4\npreset=main-left\ntree.3=h[1,v[2,3]]\n",
            saved.sidebar, saved.files
        );
        assert_eq!(parse(&text), saved);
        // A file from a build that still wrote the browser panel's divider
        // loads: an unknown key is ignored, not a parse failure.
        assert_eq!(
            parse("sidebar=310\nbrowser=940\n").files,
            Layout::default().files
        );
    }

    #[test]
    fn an_unknown_preset_is_kept_verbatim_for_the_caller_to_reject() {
        // Parsing must not know the preset list: a file written by a newer
        // build has to load, and the caller falls back when it looks it up.
        assert_eq!(parse("preset=from-the-future").preset, "from-the-future");
        assert_eq!(parse("preset=\n").preset, Layout::default().preset);
    }

    #[test]
    fn folded_survives_junk_and_an_empty_list() {
        // An empty value is "nothing folded", not a parse failure, and one
        // unreadable id must not discard the others.
        assert!(parse("folded=\n").folded.is_empty());
        assert_eq!(parse("folded=1,abc,3").folded, vec![1, 3]);
    }

    #[test]
    fn corrupt_or_partial_state_falls_back_to_defaults() {
        let d = Layout::default();
        assert_eq!(parse(""), d);
        assert_eq!(parse("garbage\nsidebar\n"), d);
        assert_eq!(parse("sidebar=abc"), d);
        // A partial file keeps the default for the missing key.
        assert_eq!(parse("sidebar=300").files, d.files);
        assert_eq!(parse("sidebar=300").sidebar, 300);
    }

    #[test]
    fn nonpositive_positions_are_rejected() {
        // Restoring 0 would render a collapsed, apparently broken window.
        assert_eq!(parse("sidebar=0\nfiles=-5"), Layout::default());
    }
}
