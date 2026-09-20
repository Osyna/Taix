# taix-mcp

MCP stdio server exposing TaiX projects, agents, and pane output.

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
```

## Protocol

The server implements MCP (Model Context Protocol) over stdio using JSON-RPC 2.0. Each request and response is a single-line JSON object.

Supported JSON-RPC methods:
- `initialize` — Returns protocol version and server capabilities
- `ping` — Health check
- `tools/list` — Returns available tool descriptors
- `tools/call` — Invokes a tool with arguments
