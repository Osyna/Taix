# taix-mcp

MCP stdio server exposing TaiX projects, agents, pane output, and browser windows.

## Installation

Build the binary:

```bash
cargo build -p taix-mcp --release
```

## Configuration

Add to your MCP client configuration (e.g., Claude Desktop):

```json
{
  "mcpServers": {
    "taix": {
      "command": "/path/to/taix-mcp"
    }
  }
}
```

Replace `/path/to/taix-mcp` with the absolute path to the built binary (typically `target/release/taix-mcp`).

## Available Tools

| Tool | Description |
|------|-------------|
| `list_projects` | List all TaiX projects with their IDs, names, and root directories |
| `list_agents` | List agents, optionally filtered by project ID. Shows agent state, harness, branch, and pane liveness |
| `agent_output` | Capture recent output from an agent's tmux pane (default 200 lines, max 5000) |
| `send_to_agent` | Send text to an agent's pane, optionally followed by Enter |
| `interrupt_agent` | Send Ctrl-C to an agent's pane |

### Browser Tools

**Requires the TaiX desktop (`taix --gui`) to be running.** Browser tools communicate over HTTP with the running desktop.

| Tool | Description |
|------|-------------|
| `browser_tabs` | List, create, select, or close browser tabs |
| `browser_navigate` | Navigate to URL or go back/forward/reload |
| `browser_snapshot` | Get accessibility tree (actions do not return a tree, so snapshot only when you need refs) |
| `browser_click` | Click element by ref from snapshot |
| `browser_type` | Type text into element by ref |
| `browser_press_key` | Press a key (Enter, Escape, ArrowDown, etc) |
| `browser_evaluate` | Evaluate JavaScript on page or element |
| `browser_wait_for` | Wait for text to appear, disappear, or URL to match |
| `browser_console` | Get or clear console messages |
| `browser_screenshot` | Capture PNG screenshot to path |

**Low-token contract:** Actions return one-line confirmations, not full trees. Call `browser_snapshot` only when you need element refs for subsequent actions.


## Examples

### List all projects

```json
{
  "name": "list_projects",
  "arguments": {}
}
```

### List agents for a specific project

```json
{
  "name": "list_agents",
  "arguments": {
    "project": 1
  }
}
```

### Get agent output

```json
{
  "name": "agent_output",
  "arguments": {
    "agent": 5,
    "lines": 500
  }
}
```

### Send a command to an agent

```json
{
  "name": "send_to_agent",
  "arguments": {
    "agent": 5,
    "text": "cargo test",
    "enter": true
  }
}
```

### Interrupt a running agent

```json
{
  "name": "interrupt_agent",
  "arguments": {
    "agent": 5
  }
}

### List browser tabs

```json
{
  "name": "browser_tabs",
  "arguments": {
    "action": "list"
  }
}
```

### Create a new browser tab

```json
{
  "name": "browser_tabs",
  "arguments": {
    "action": "new",
    "url": "http://localhost:5173",
    "headless": false
  }
}
```

### Navigate to a URL

```json
{
  "name": "browser_navigate",
  "arguments": {
    "url": "https://example.com"
  }
}
```

### Get page snapshot (accessibility tree)

```json
{
  "name": "browser_snapshot",
  "arguments": {
    "filter": "Sign in",
    "max": 100
  }
}
```

### Click an element by ref

```json
{
  "name": "browser_click",
  "arguments": {
    "ref": "e7"
  }
}
```

### Type text and submit

```json
{
  "name": "browser_type",
  "arguments": {
    "ref": "e3",
    "text": "username",
    "submit": true
  }
}
```
```

## Protocol

The server implements MCP (Model Context Protocol) over stdio using JSON-RPC 2.0. Each request and response is a single-line JSON object.

Supported JSON-RPC methods:
- `initialize` — Returns protocol version and server capabilities
- `ping` — Health check
- `tools/list` — Returns available tool descriptors
- `tools/call` — Invokes a tool with arguments
