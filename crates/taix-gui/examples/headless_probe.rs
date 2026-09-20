//! Does a web view that is never put in a window still load a page and run
//! JavaScript? That is the whole question behind headless browser windows,
//! and it is not answerable by reading WebKit's documentation.
//!
//!   cargo run -p taix-gui --features browser --example headless_probe
//!
//! Exits 0 when the page answered with the elements it was given, 1 when it
//! did not answer at all - which would mean headless has to hold an
//! unmapped toplevel instead.

use gtk::glib;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    gtk::init()?;
    let browser = taix_gui::browser::Browser::new("about:blank");
    browser.load(concat!(
        "data:text/html,<title>Probe</title>",
        "<h1>Headless</h1><button id=go>Click me</button>",
        "<script>window.__probe = 41 + 1</script>"
    ));

    // Nothing here is parented: `browser.root` is dropped on the floor on
    // purpose. Give the load a moment, then ask the page about itself.
    let probe = std::rc::Rc::new(browser);
    glib::timeout_add_local_once(Duration::from_secs(2), {
        let probe = probe.clone();
        move || {
            let inner = probe.clone();
            probe.dispatch(
                &serde_json::json!({ "op": "eval", "js": "() => [document.title, window.__probe, document.querySelectorAll('button').length]" }),
                move |answer| {
                    println!("eval: {answer}");
                    let value = answer.get("value").cloned().unwrap_or_default();
                    let ok = value.get(1).and_then(serde_json::Value::as_i64) == Some(42);
                    if !ok {
                        eprintln!("the page did not run its script: headless needs a toplevel");
                        std::process::exit(1);
                    }
                    inner.dispatch(&serde_json::json!({ "op": "snapshot" }), |answer| {
                        println!("snapshot: {answer}");
                        let tree = answer
                            .get("tree")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        if tree.contains("Click me") {
                            println!("unparented view: loads, scripts and snapshots");
                            std::process::exit(0);
                        }
                        eprintln!("snapshot missed the button the page has");
                        std::process::exit(1);
                    });
                },
            );
        }
    });
    // A page that never answers is the failure this probe exists to catch.
    glib::timeout_add_local_once(Duration::from_secs(10), || {
        eprintln!("no answer in 10s: an unparented web view does not load");
        std::process::exit(1);
    });

    glib::MainLoop::new(None, false).run();
    Ok(())
}
