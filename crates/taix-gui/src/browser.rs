//! Embedded WebKitGTK panel.
//!
//! This is the payoff for choosing GTK4: `WebKitWebView` is a real GtkWidget,
//! so it lives inside the window next to the agent panes. In a wgpu toolkit
//! (egui/iced/slint) a web view is an opaque OS surface and can only be a
//! separate window or a screenshot stream.
//!
//! Built on demand — WebKit spawns its own network and web-content processes,
//! so an unopened panel must cost nothing.

use gtk::prelude::*;
use webkit6::prelude::*;

pub struct Browser {
    pub root: gtk::Box,
    view: webkit6::WebView,
    url: gtk::Entry,
}

impl Browser {
    pub fn new(home_uri: &str) -> Browser {
        let view = webkit6::WebView::new();
        view.set_hexpand(true);
        view.set_vexpand(true);
        // The developer tools are WebKit's own: enabling extras is what puts
        // the inspector - console, sources, elements, network - behind
        // `show()`, and adds WebKit's *Inspect Element* to the page menu.
        // Writing a JS console and a source view by hand would be a worse
        // copy of what the engine already ships.
        if let Some(settings) = webkit6::prelude::WebViewExt::settings(&view) {
            settings.set_enable_developer_extras(true);
        }

        let back = gtk::Button::from_icon_name("go-previous-symbolic");
        let forward = gtk::Button::from_icon_name("go-next-symbolic");
        let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
        // Without this the start page is reachable only by emptying the bar,
        // and `browser_home` has no button of its own.
        let home = gtk::Button::from_icon_name("go-home-symbolic");
        home.set_tooltip_text(Some("Home"));
        let url = gtk::Entry::builder()
            .placeholder_text("URL")
            .hexpand(true)
            .build();

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        bar.add_css_class("toolbar");
        bar.append(&back);
        bar.append(&forward);
        bar.append(&reload);
        bar.append(&home);
        bar.append(&url);

        // The page over the console, in the one column. The inspector is a
        // widget like anything else, so it docks here instead of opening a
        // second window - which is also the only way to get it: WebKit's own
        // `can_attach` is false below 750px of page width, and a side panel
        // is narrower than that.
        let split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&view)
            .resize_start_child(true)
            .shrink_start_child(false)
            .resize_end_child(true)
            .shrink_end_child(false)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&bar);
        root.append(&split);
        // Floor for the divider drag; a squeezed web view is useless.
        root.set_size_request(360, -1);

        back.connect_clicked({
            let view = view.clone();
            move |_| view.go_back()
        });
        forward.connect_clicked({
            let view = view.clone();
            move |_| view.go_forward()
        });
        reload.connect_clicked({
            let view = view.clone();
            move |_| view.reload()
        });
        home.connect_clicked({
            let (view, entry, target) = (view.clone(), url.clone(), home_uri.to_string());
            move |_| go(&view, &entry, &target)
        });
        url.connect_activate({
            let view = view.clone();
            move |entry| go(&view, entry, &entry.text())
        });
        // Follow in-page navigation so the bar is not lying about where we are.
        view.connect_uri_notify({
            let url = url.clone();
            move |view| {
                // Skip while the user is typing, or the bar fights them.
                // `about:blank` is the start page: leave the bar empty and
                // prompting rather than showing a URI nobody typed.
                if let Some(uri) = view.uri().filter(|u| u != "about:blank")
                    && !url.has_focus()
                {
                    url.set_text(&uri);
                }
            }
        });

        // Wherever WebKit would put the inspector - its own window, or
        // docked inside the view when the page is wide enough - it lands in
        // the split below the page instead. Returning `true` claims the
        // signal, so no second window is ever built; `detach` gives it back,
        // because WebKit's default handler needs the widget unparented.
        if let Some(inspector) = view.inspector() {
            inspector.connect_open_window({
                let split = split.clone();
                move |inspector| dock(&split, inspector)
            });
            inspector.connect_attach({
                let split = split.clone();
                move |inspector| dock(&split, inspector)
            });
            inspector.connect_detach({
                let split = split.clone();
                move |_| {
                    undock(&split);
                    false
                }
            });
            inspector.connect_closed({
                let split = split.clone();
                move |_| undock(&split)
            });
        }

        // Right-click the page → *Developer console*, on top of the menu
        // WebKit builds for what is under the pointer. One item, and it
        // toggles: the same click closes it again.
        let devtools = gtk::gio::SimpleAction::new("taix-devtools", None);
        devtools.connect_activate({
            let view = view.clone();
            move |_, _| toggle_inspector(&view)
        });
        view.connect_context_menu(move |_, menu, _| {
            menu.append(&webkit6::ContextMenuItem::new_separator());
            menu.append(&webkit6::ContextMenuItem::from_gaction(
                &devtools,
                "Developer console",
                None,
            ));
            false
        });

        let browser = Browser { root, view, url };
        browser.load(home_uri);
        browser
    }

    pub fn load(&self, target: &str) {
        go(&self.view, &self.url, target);
    }
}

/// Navigate, or show the start page. Nothing to load is not an error and
/// not `about:blank`'s white rectangle: it is the prompt to pick somewhere.
fn go(view: &webkit6::WebView, url: &gtk::Entry, target: &str) {
    let uri = normalise(target);
    if uri == "about:blank" {
        url.set_text("");
        view.load_html(&start_page(), None);
        return;
    }
    url.set_text(&uri);
    view.load_uri(&uri);
}

/// Open or close the console. Whether it is on screen is asked of the
/// widget tree rather than of `is_attached()`, which is WebKit's own idea
/// of docking and stays false while we hold the view ourselves.
fn toggle_inspector(view: &webkit6::WebView) {
    let Some(inspector) = view.inspector() else {
        return;
    };
    if inspector.web_view().and_then(|c| c.parent()).is_some() {
        inspector.close();
    } else {
        inspector.show();
    }
}

/// Put the inspector's own web view under the page: elements, console,
/// sources, network - WebKit's DevTools, not a copy of them.
fn dock(split: &gtk::Paned, inspector: &webkit6::WebInspector) -> bool {
    let Some(console) = inspector.web_view() else {
        return false;
    };
    if console.parent().is_none() {
        split.set_end_child(Some(&console));
    }
    // Two thirds page, one third console: the shape every browser ships,
    // and the divider is draggable from there.
    let height = split.height();
    if height > 240 {
        split.set_position(height * 2 / 3);
    }
    true
}

fn undock(split: &gtk::Paned) {
    split.set_end_child(None::<&gtk::Widget>);
}

/// The landing page. `about:blank` is a white rectangle in a dark window and
/// says nothing about what to do next, so the blank target is rendered as a
/// document instead - painted from the installed palette, because a web view
/// is the one surface GTK's stylesheet cannot reach.
const START: &str = include_str!("../start.html");

fn start_page() -> String {
    START
        .replace("$bg", &crate::theme::color("taix_bg", "#0e1115"))
        .replace("$fg", &crate::theme::color("taix_fg", "#d7dde3"))
        .replace("$dim", &crate::theme::color("taix_dim", "#6b747c"))
        .replace("$accent", &crate::theme::color("taix_accent", "#7ad6bd"))
        .replace("$border", &crate::theme::color("taix_border", "#242a30"))
}

/// Accept what a human types. A bare host or `localhost:3000` is the common
/// case for an agent's dev server, and `load_uri` silently does nothing
/// without a scheme.
fn normalise(input: &str) -> String {
    let text = input.trim();
    if text.is_empty() {
        return "about:blank".to_string();
    }
    if text.contains("://") || text.starts_with("about:") || text.starts_with("data:") {
        return text.to_string();
    }
    if let Some(rest) = text.strip_prefix("localhost") {
        return format!("http://localhost{rest}");
    }
    if text.starts_with("127.0.0.1") || text.starts_with("0.0.0.0") || text.starts_with('[') {
        return format!("http://{text}");
    }
    if text.starts_with('/') {
        return format!("file://{text}");
    }
    // Something with a dot and no spaces is a host; anything else is a search.
    if text.contains(' ') || !text.contains('.') {
        return format!(
            "https://duckduckgo.com/?q={}",
            gtk::glib::Uri::escape_string(text, None, false)
        );
    }
    format!("https://{text}")
}

#[cfg(test)]
mod tests {
    use super::{normalise, start_page};

    #[test]
    fn schemeless_hosts_get_a_scheme() {
        // load_uri does nothing at all without one, so this is the difference
        // between a working address bar and a dead one.
        assert_eq!(normalise("example.com"), "https://example.com");
        assert_eq!(normalise("example.com/a/b"), "https://example.com/a/b");
    }

    #[test]
    fn local_dev_servers_stay_on_http() {
        // https to a local dev server just fails the handshake.
        assert_eq!(normalise("localhost:3000"), "http://localhost:3000");
        assert_eq!(normalise("127.0.0.1:8080"), "http://127.0.0.1:8080");
    }

    #[test]
    fn explicit_schemes_and_paths_are_preserved() {
        assert_eq!(normalise("http://a.test"), "http://a.test");
        assert_eq!(normalise("about:blank"), "about:blank");
        assert_eq!(normalise("/tmp/report.html"), "file:///tmp/report.html");
    }

    #[test]
    fn non_hosts_become_a_search() {
        assert!(normalise("rust paned widget").starts_with("https://duckduckgo.com/?q="));
        assert!(normalise("notahost").starts_with("https://duckduckgo.com/?q="));
    }

    #[test]
    fn blank_input_does_not_produce_a_bogus_uri() {
        assert_eq!(normalise("   "), "about:blank");
    }

    #[test]
    fn start_page_leaves_no_placeholder_behind() {
        // An unsubstituted `$accent` is a colour the page renders as nothing,
        // which looks like a layout bug rather than a missing wire-up.
        let page = start_page();
        assert!(!page.contains('$'), "unsubstituted token in the start page");
        assert!(page.contains("#7ad6bd"), "fallback palette not applied");
    }
}
