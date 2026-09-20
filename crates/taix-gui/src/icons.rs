//! Harness icons: brand marks for the agents we know, generic glyphs for the
//! rest, and whatever image file the user points a harness at.
//!
//! The SVGs are compiled into the binary and written to the cache directory
//! at startup, because a GTK icon theme is a directory: `IconTheme` looks up
//! names on disk, and going through it is what makes a `*-symbolic` icon
//! follow the widget's colour like every other icon in the window. Brand
//! marks are from Simple Icons (CC0), reduced to a single path.

use std::path::PathBuf;
use std::sync::LazyLock;

macro_rules! bundled {
    ($($name:literal),* $(,)?) => {
        /// Every icon shipped: (name, SVG bytes).
        pub const BUNDLED: &[(&str, &str)] = &[
            $(($name, include_str!(concat!("../icons/taix-", $name, "-symbolic.svg")))),*
        ];
    };
}

bundled!(
    // Brands, named after the catalogue id they belong to.
    "claude",
    "codex",
    "gemini",
    "qwen",
    "opencode",
    "cursor-agent",
    "copilot",
    "amp",
    "q",
    // Brands for commands you add yourself.
    "ollama",
    "mistral",
    "deepseek",
    "huggingface",
    "langchain",
    // Generic.
    "terminal",
    "robot",
    "sparkle",
    "chat",
    "code",
    "bolt",
    "chip",
    "rocket",
    "gear",
    "star",
    "cloud",
    "globe",
    "flask",
    "wrench",
);

/// What a harness with no icon of its own shows.
pub const FALLBACK: &str = "robot";

fn dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".cache")))?;
    Some(cache.join("taix/icons"))
}

/// Write the bundle out (only files whose bytes changed) and put the
/// directory on the icon search path. Call once, after the display exists.
pub fn install() {
    let Some(dir) = dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let display = gtk::gdk::Display::default().expect("no display");
    gtk::IconTheme::for_display(&display).add_search_path(&dir);
}

/// Write one icon's SVG the first time something asks for it. A session
/// showing three harnesses has no reason to put twenty-six files on disk.
fn ensure(name: &str) {
    static WRITTEN: LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        LazyLock::new(Default::default);
    let Ok(mut set) = WRITTEN.lock() else { return };
    if set.contains(name) {
        return;
    }
    let Some(dir) = dir() else { return };
    let Some(&svg) = BUNDLED.iter().find(|(n, _)| *n == name).map(|(_, s)| s) else {
        return;
    };
    let path = dir.join(format!("taix-{name}-symbolic.svg"));
    if std::fs::read_to_string(&path).ok().as_deref() != Some(svg) {
        let _ = std::fs::write(&path, svg);
    }
    set.insert(name.to_owned());
}

/// Point an image at a harness icon: a bundled name, or a file path.
///
/// A file keeps its own colours - a PNG logo is not a symbolic icon - and is
/// scaled to the same box, so a mixed list still lines up.
pub fn set(image: &gtk::Image, icon: Option<&str>) {
    let icon = icon.unwrap_or(FALLBACK);
    if icon.starts_with('/') {
        image.set_from_file(Some(icon));
    } else {
        ensure(icon);
        image.set_icon_name(Some(&format!("taix-{icon}-symbolic")));
    }
}

pub fn image(icon: Option<&str>, size: i32) -> gtk::Image {
    let image = gtk::Image::builder().pixel_size(size).build();
    set(&image, icon);
    image
}
