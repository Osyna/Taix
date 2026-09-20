//! Dispatch layer - routes ops to GTK or JavaScript.
//!
//! Host-side ops (navigate, back/forward/reload, screenshot) are GTK calls.
//! Everything else is a single JavaScript call into the page through the
//! injected `window.__taix` bridge.

use gtk::prelude::*;
use serde_json::{Value, json};
use webkit6::prelude::*;

/// Execute one browser operation and answer exactly once.
///
/// The callback is invoked on the GTK main loop, either immediately for
/// synchronous ops (navigate, back, forward, reload) or when the JavaScript
/// promise settles. Errors are caught and returned as `{ok: false, error: "..."}`.
pub fn dispatch(view: &webkit6::WebView, op: &Value, reply: impl FnOnce(Value) + 'static) {
    let op_type = op.get("op").and_then(Value::as_str).unwrap_or("");

    match op_type {
        // `goto` never reaches here: the widget answers that one when the
        // document has loaded.
        "navigate" => {
            let action = op.get("action").and_then(Value::as_str).unwrap_or("goto");
            match action {
                "back" => view.go_back(),
                "forward" => view.go_forward(),
                "reload" => view.reload(),
                other => {
                    reply(
                        json!({"ok": false, "error": format!("unknown navigate action: {other}")}),
                    );
                    return;
                }
            }
            reply(json!({
                "ok": true,
                "url": view.uri().unwrap_or_default().to_string(),
                "title": view.title().unwrap_or_default().to_string(),
            }));
        }
        "snapshot" => {
            let filter = op.get("filter").and_then(Value::as_str).map(String::from);
            let max = op.get("max").and_then(Value::as_u64).unwrap_or(150);

            let args = json!({ "filter": filter, "max": max });
            let code = format!("return JSON.stringify(window.__taix.snapshot({}));", args);

            eval_js(view, &code, move |result| match result {
                Ok(mut data) => {
                    data["ok"] = json!(true);
                    reply(data);
                }
                Err(e) => {
                    reply(json!({"ok": false, "error": e}));
                }
            });
        }
        "act" => {
            let args = json!({
                "action": op.get("action"),
                "ref": op.get("ref"),
                "text": op.get("text"),
                "submit": op.get("submit"),
                "values": op.get("values"),
                "key": op.get("key"),
                "dx": op.get("dx"),
                "dy": op.get("dy")
            });
            let code = format!("return JSON.stringify(await window.__taix.act({}));", args);

            eval_js(view, &code, move |result| match result {
                Ok(data) => reply(data),
                Err(e) => reply(json!({"ok": false, "error": e})),
            });
        }
        "eval" => {
            let js = op.get("js").and_then(Value::as_str).unwrap_or("() => null");
            // With a ref the function is handed that element, which is what
            // makes "read this one thing" cheap: no selector, no snapshot.
            let code = match op.get("ref").and_then(Value::as_str) {
                Some(target) => format!(
                    "return JSON.stringify({{ ok: true, value: await ({js})(window.__taix.resolve({target:?})) }});"
                ),
                None => format!("return JSON.stringify({{ ok: true, value: await ({js})() }});"),
            };

            eval_js(view, &code, move |result| match result {
                Ok(data) => reply(data),
                Err(e) => reply(json!({"ok": false, "error": e})),
            });
        }
        "wait" => {
            let args = json!({
                "text": op.get("text"),
                "gone": op.get("gone"),
                "url": op.get("url"),
                "ms": op.get("ms")
            });
            let code = format!(
                "return JSON.stringify(await window.__taix.waitFor({}));",
                args
            );

            eval_js(view, &code, move |result| match result {
                Ok(data) => reply(data),
                Err(e) => reply(json!({"ok": false, "error": e})),
            });
        }
        "console" => {
            let clear = op.get("clear").and_then(Value::as_bool).unwrap_or(false);
            let args = json!({ "clear": clear });
            let code = format!(
                "return JSON.stringify({{ ok: true, messages: window.__taix.console({}) }});",
                args
            );

            eval_js(view, &code, move |result| match result {
                Ok(data) => reply(data),
                Err(e) => reply(json!({"ok": false, "error": e})),
            });
        }
        "shot" => {
            let path = op
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("/tmp/page.png");

            view.snapshot(
                webkit6::SnapshotRegion::FullDocument,
                webkit6::SnapshotOptions::NONE,
                None::<&gtk::gio::Cancellable>,
                {
                    let path = path.to_string();
                    move |result| match result {
                        Ok(texture) => match texture.save_to_png(&path) {
                            Ok(_) => {
                                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                                reply(json!({
                                    "ok": true,
                                    "path": path,
                                    "bytes": bytes
                                }));
                            }
                            Err(e) => {
                                reply(json!({
                                    "ok": false,
                                    "error": format!("failed to save screenshot: {}", e)
                                }));
                            }
                        },
                        Err(e) => {
                            reply(json!({
                                "ok": false,
                                "error": format!("snapshot failed: {}", e)
                            }));
                        }
                    }
                },
            );
        }
        _ => {
            reply(json!({"ok": false, "error": format!("unknown op: {}", op_type)}));
        }
    }
}

/// Evaluate JavaScript and parse the result as JSON.
///
/// The code MUST return a JSON string via `JSON.stringify(...)`. The callback
/// receives either `Ok(parsed_json)` or `Err(error_message)`.
fn eval_js(
    view: &webkit6::WebView,
    code: &str,
    callback: impl FnOnce(Result<Value, String>) + 'static,
) {
    // webkit6 0.6.1 call_async_javascript_function:
    // fn call_async_javascript_function(
    //     &self,
    //     body: &str,
    //     arguments: Option<&Variant>,
    //     world_name: Option<&str>,
    //     source_uri: Option<&str>,
    //     cancellable: Option<&impl IsA<Cancellable>>,
    //     callback: impl FnOnce(Result<javascriptcore::Value, Error>) + 'static,
    // )

    view.call_async_javascript_function(
        code,
        None, // arguments
        None, // world_name (main world)
        None, // source_uri
        None::<&gtk::gio::Cancellable>,
        move |result| match result {
            Ok(js_value) => {
                let json_str = js_value.to_str();
                match serde_json::from_str::<Value>(&json_str) {
                    Ok(value) => callback(Ok(value)),
                    Err(e) => {
                        callback(Err(format!("JSON parse error: {}", e)));
                    }
                }
            }
            Err(e) => {
                callback(Err(format!("JavaScript error: {}", e)));
            }
        },
    );
}
