use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use taix_core::Config;

const TIMEOUT: Duration = Duration::from_secs(35);

fn port_and_key() -> Result<(u16, String), String> {
    let config = Config::load();
    let port = config.web.port;

    let key = taix_core::web_key().map_err(|e| format!("failed to read web key: {}", e))?;

    if key.is_empty() {
        return Err(
            "the TaiX desktop has never served; browser tools need `taix --gui`".to_string(),
        );
    }

    Ok((port, key))
}

/// One request to the running desktop. Short-lived: the server keeps
/// connections alive, so without `Connection: close` reading to the end
/// would sit here until the read timeout on every single call.
fn post(op: Value) -> Result<Value, String> {
    let (port, key) = port_and_key()?;
    post_to(port, op, &key)
}

/// One request to the running desktop. Short-lived on purpose: the server
/// keeps connections alive, so without `Connection: close` reading to the
/// end would sit here until the read timeout on every single call.
fn post_to(port: u16, op: Value, key: &str) -> Result<Value, String> {
    let body = serde_json::to_string(&op).unwrap();
    let host = format!("127.0.0.1:{port}");

    let mut stream = TcpStream::connect(&host).map_err(|_| {
        "the TaiX desktop is not running; browser tools need `taix --gui`".to_string()
    })?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|e| format!("failed to set timeout: {e}"))?;

    let request = format!(
        "POST /api/browser HTTP/1.1\r\n\
         Host: {host}\r\n\
         Cookie: taix={key}\r\n\
         Connection: close\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len(),
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write failed: {e}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| format!("read failed: {e}"))?;
    let response = String::from_utf8_lossy(&response);
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or("the desktop sent a malformed response")?;
    // A refused key is a different problem from a page that said no, and
    // the difference is what the user has to act on.
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status == "401" || status == "403" {
        return Err("the desktop refused the web key; check config.toml".into());
    }
    serde_json::from_str(body).map_err(|e| format!("the desktop sent {status}: {e}"))
}

/// `{"ok": false}` is the page or the desktop saying no, in a sentence
/// written for the agent to act on. It becomes the tool's error.
fn unwrap_answer(answer: Value) -> Result<Value, String> {
    if answer.get("ok") == Some(&Value::Bool(false)) {
        return Err(answer
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the desktop refused, and did not say why")
            .to_string());
    }
    Ok(answer)
}

fn ask(op: Value) -> Result<Value, String> {
    unwrap_answer(post(op)?)
}

/// Copy the arguments the caller gave into the op, leaving out what they
/// did not: an absent `tab` means "wherever the browser already is", and
/// the desktop decides that, not this process.
fn carry(op: &mut Value, args: &Value, fields: &[&str]) {
    for field in fields {
        if let Some(value) = args.get(*field) {
            op[*field] = value.clone();
        }
    }
}

fn text(answer: &Value, field: &str) -> String {
    answer
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn tabs(args: &Value) -> Result<String, String> {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("list");
    let mut op = json!({ "op": "tabs", "action": action });
    carry(&mut op, args, &["tab", "url", "project", "headless"]);
    if (action == "select" || action == "close") && op.get("tab").is_none() {
        return Err(format!("{action} needs the id of a browser window"));
    }
    let answer = ask(op)?;
    let tabs = answer.get("tabs").cloned().unwrap_or(Value::Null);
    Ok(serde_json::to_string_pretty(&tabs).unwrap())
}

pub fn navigate(args: &Value) -> Result<String, String> {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("goto");
    let mut op = json!({ "op": "navigate", "action": action });
    carry(&mut op, args, &["tab", "url"]);
    let answer = ask(op)?;
    Ok(format!(
        "{}\n{}",
        text(&answer, "url"),
        text(&answer, "title")
    ))
}

pub fn snapshot(args: &Value) -> Result<String, String> {
    let mut op = json!({ "op": "snapshot" });
    carry(&mut op, args, &["tab", "filter", "max"]);
    let answer = ask(op)?;
    let mut out = format!(
        "{}\n{}\n\n{}",
        text(&answer, "url"),
        text(&answer, "title"),
        text(&answer, "tree")
    );
    if answer.get("truncated") == Some(&Value::Bool(true)) {
        out.push_str("\n\n(cut short: raise max, or filter)");
    }
    Ok(out)
}

pub fn click(args: &Value) -> Result<String, String> {
    act(args, "click")
}

pub fn type_text(args: &Value) -> Result<String, String> {
    act(args, "type")
}

pub fn press_key(args: &Value) -> Result<String, String> {
    act(args, "key")
}

/// Every acting tool answers in one line. The URL is added only when the
/// action moved the page, because that is the only time it is news.
fn act(args: &Value, action: &str) -> Result<String, String> {
    let mut op = json!({ "op": "act", "action": action });
    carry(
        &mut op,
        args,
        &["tab", "ref", "text", "submit", "key", "values", "dx", "dy"],
    );
    let answer = ask(op)?;
    let did = text(&answer, "did");
    match args.get("url").and_then(Value::as_str) {
        Some(before) if before != text(&answer, "url") => {
            Ok(format!("{did}\n{}", text(&answer, "url")))
        }
        _ => Ok(did),
    }
}

pub fn evaluate(args: &Value) -> Result<String, String> {
    let mut op = json!({ "op": "eval" });
    carry(&mut op, args, &["tab", "js", "ref"]);
    let answer = ask(op)?;
    let value = answer.get("value").cloned().unwrap_or(Value::Null);
    Ok(serde_json::to_string_pretty(&value).unwrap())
}

pub fn wait_for(args: &Value) -> Result<String, String> {
    let mut op = json!({ "op": "wait" });
    carry(&mut op, args, &["tab", "text", "gone", "url", "ms"]);
    let answer = ask(op)?;
    let waited = answer.get("waited_ms").and_then(Value::as_i64).unwrap_or(0);
    Ok(format!("waited {waited}ms"))
}

pub fn console(args: &Value) -> Result<String, String> {
    let mut op = json!({ "op": "console" });
    carry(&mut op, args, &["tab", "clear"]);
    let answer = ask(op)?;
    let messages = answer
        .get("messages")
        .and_then(Value::as_array)
        .map(|lines| {
            lines
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    Ok(messages)
}

pub fn screenshot(args: &Value) -> Result<String, String> {
    let mut op = json!({ "op": "shot" });
    carry(&mut op, args, &["tab", "path"]);
    let answer = ask(op)?;
    let bytes = answer.get("bytes").and_then(Value::as_i64).unwrap_or(0);
    Ok(format!("{} ({bytes} bytes)", text(&answer, "path")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_connection_names_the_missing_desktop() {
        // Nothing is listening on this port, which is what an agent hits
        // when the user has only the TUI open.
        let answer = post_to(9, json!({ "op": "tabs", "action": "list" }), "key");
        let message = answer.unwrap_err();
        assert!(message.contains("desktop is not running"), "{message}");
    }

    #[test]
    fn a_refusal_from_the_page_becomes_a_tool_error() {
        let answer = json!({ "ok": false, "error": "ref e7 is stale, snapshot again" });
        let err = unwrap_answer(answer).unwrap_err();
        assert_eq!(err, "ref e7 is stale, snapshot again");
    }
}
