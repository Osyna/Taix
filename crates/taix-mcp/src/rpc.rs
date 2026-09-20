use crate::tools::{self, State};
use serde_json::{Value, json};

const PROTOCOL_VERSION: &str = "2024-11-05";

pub fn handle(state: &State, line: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            return Some(error_response(Value::Null, -32700, "Parse error"));
        }
    };

    let id = request.get("id").cloned();
    let method = match request.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            return id
                .as_ref()
                .map(|i| error_response(i.clone(), -32600, "Invalid Request"));
        }
    };

    let params = request.get("params").cloned().unwrap_or(Value::Null);

    let result = match method {
        "initialize" => handle_initialize(),
        "ping" => Ok(json!({})),
        "tools/list" => handle_tools_list(),
        "tools/call" => handle_tools_call(state, &params),
        _ => Err((-32601, "Method not found".to_string())),
    };

    // Notifications (no id) get no response
    let id = id?;

    match result {
        Ok(r) => Some(success_response(id, r)),
        Err((code, msg)) => Some(error_response(id, code, &msg)),
    }
}

fn handle_initialize() -> Result<Value, (i32, String)> {
    Ok(json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "taix",
            "version": env!("CARGO_PKG_VERSION")
        }
    }))
}

fn handle_tools_list() -> Result<Value, (i32, String)> {
    Ok(json!({
        "tools": [
            {
                "name": "list_projects",
                "description": "List all TaiX projects",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "list_agents",
                "description": "List agents, optionally filtered by project",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "integer",
                            "description": "Optional project ID to filter by"
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "agent_output",
                "description": "Capture recent output from an agent's pane",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent": {
                            "type": "integer",
                            "description": "Agent ID"
                        },
                        "lines": {
                            "type": "integer",
                            "description": "Number of lines to capture (default 200, max 5000)"
                        }
                    },
                    "required": ["agent"]
                }
            },
            {
                "name": "send_to_agent",
                "description": "Send text to an agent's pane",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent": {
                            "type": "integer",
                            "description": "Agent ID"
                        },
                        "text": {
                            "type": "string",
                            "description": "Text to send"
                        },
                        "enter": {
                            "type": "boolean",
                            "description": "Press Enter after text (default true)"
                        }
                    },
                    "required": ["agent", "text"]
                }
            },
            {
                "name": "interrupt_agent",
                "description": "Send Ctrl-C to an agent's pane",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent": {
                            "type": "integer",
                            "description": "Agent ID"
                        }
                    },
                    "required": ["agent"]
                }
            }
        ]
    }))
}

fn handle_tools_call(state: &State, params: &Value) -> Result<Value, (i32, String)> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or((-32602, "missing required argument \"name\"".to_string()))?;

    let arguments = params.get("arguments").unwrap_or(&Value::Null);

    let result = match name {
        "list_projects" => tools::list_projects(state),
        "list_agents" => tools::list_agents(state, arguments),
        "agent_output" => tools::agent_output(state, arguments),
        "send_to_agent" => tools::send_to_agent(state, arguments),
        "interrupt_agent" => tools::interrupt_agent(state, arguments),
        _ => Err(format!("unknown tool: {}", name)),
    };

    match result {
        Ok(text) => Ok(json!({
            "content": [{"type": "text", "text": text}],
            "isError": false
        })),
        Err(msg) => Ok(json!({
            "content": [{"type": "text", "text": msg}],
            "isError": true
        })),
    }
}

fn success_response(id: Value, result: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
    .to_string()
}

fn error_response(id: Value, code: i32, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use taix_core::Store;

    fn test_state() -> State {
        let store = Store::open_memory().unwrap();
        let config = taix_core::Config::default();
        State::new(store, config)
    }

    #[test]
    fn test_initialize() {
        let state = test_state();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let res = handle(&state, req).unwrap();
        let v: Value = serde_json::from_str(&res).unwrap();
        assert_eq!(v["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(v["result"]["serverInfo"]["name"], "taix");
    }

    #[test]
    fn test_tools_list() {
        let state = test_state();
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let res = handle(&state, req).unwrap();
        let v: Value = serde_json::from_str(&res).unwrap();
        let tools = v["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            vec![
                "list_projects",
                "list_agents",
                "agent_output",
                "send_to_agent",
                "interrupt_agent"
            ]
        );
    }

    #[test]
    fn test_notification_no_response() {
        let state = test_state();
        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let res = handle(&state, req);
        assert!(res.is_none());
    }

    #[test]
    fn test_malformed_json() {
        let state = test_state();
        let req = r#"not json"#;
        let res = handle(&state, req).unwrap();
        let v: Value = serde_json::from_str(&res).unwrap();
        assert_eq!(v["error"]["code"], -32700);
        assert_eq!(v["id"], Value::Null);
    }

    #[test]
    fn test_unknown_method() {
        let state = test_state();
        let req = r#"{"jsonrpc":"2.0","id":3,"method":"unknown/method"}"#;
        let res = handle(&state, req).unwrap();
        let v: Value = serde_json::from_str(&res).unwrap();
        assert_eq!(v["error"]["code"], -32601);
    }

    #[test]
    fn test_missing_tool_name() {
        let state = test_state();
        let req = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"arguments":{}}}"#;
        let res = handle(&state, req).unwrap();
        let v: Value = serde_json::from_str(&res).unwrap();
        assert_eq!(v["error"]["code"], -32602);
        assert!(v["error"]["message"].as_str().unwrap().contains("name"));
    }

    #[test]
    fn test_unknown_tool() {
        let state = test_state();
        let req = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"unknown_tool","arguments":{}}}"#;
        let res = handle(&state, req).unwrap();
        let v: Value = serde_json::from_str(&res).unwrap();
        assert_eq!(v["result"]["isError"], true);
        assert!(
            v["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown tool")
        );
    }
}
