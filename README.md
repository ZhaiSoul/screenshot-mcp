# Screenshot MCP

Windows screenshot **MCP Server** using **Windows Graphics Capture (WGC)** for OBS-like window capture, with **MCP Streamable HTTP** remote access support.

## Features

- **OBS-style Window Capture** — WGC reads GPU buffer directly, **zero interference** with any window state
- **HDR Compatible** — BGRA8 and FP16 formats with automatic tonemapping
- **Window Filtering** — Filter by process name, title (exact/contains/regex), with include/exclude modes
- **MCP Streamable HTTP** — SSE bidirectional channel for remote bot cross-network calls
- **Local stdio Mode** — Compatible with Claude Desktop / Cursor and other local AI agents
- **Get Active Window** — Query current foreground/focus window info
- **Multi-Monitor** — Auto-detect or specify monitor index

## Quick Start

```bash
cargo build --release
```

Build output: `target/release/screenshot-mcp.exe`

### Launch Modes

| Mode | Command | Description |
|------|---------|-------------|
| **Double-click** | Run the exe directly | Starts Streamable HTTP MCP Server (`http://0.0.0.0:3210`) |
| HTTP mode | `screenshot-mcp mcp --transport http --port 3210` | Remote access (default port 3210) |
| stdio mode | `screenshot-mcp mcp` | Local Claude Desktop integration |

> **Tip: Simply double-click `screenshot-mcp.exe` to start the server — no arguments needed.**

After startup you will see:
```
[screenshot-mcp] Listening on http://0.0.0.0:3210 (MCP Streamable HTTP)
[screenshot-mcp] Ready. SSE endpoint: http://0.0.0.0:3210/sse
[screenshot-mcp] Message endpoint: POST http://0.0.0.0:3210/message
```

---

## MCP Streamable HTTP Protocol

This server follows the [MCP Streamable HTTP Transport](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports#streamable-http) specification.

### Protocol Flow

```
┌──────────┐         GET /sse?sessionId=xxx          ┌──────────┐
│          │ ──────────────────────────────────────▶  │          │
│  Client  │    SSE long-poll (receive response stream)│  Server  │
│          │ ◀────────────────────────────────────── │          │
│          │     event: message                      │          │
│          │     data: {jsonrpc response}            │          │
│          │                                        │          │
│          │   POST /message                        │          │
│          │ ──────────────────────────────────────▶ │          │
│          │   {jsonrpc request}                     │          │
│          │ ◀──────── 200 OK + sessionId ──────────  │          │
└──────────┘                                        └──────────┘
```

### Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/sse` | GET | SSE event stream, returns JSON-RPC responses. Supports `?sessionId=xxx` to reuse a session |
| `/message` | POST | Send JSON-RPC request. Response pushed via SSE, HTTP returns `sessionId` |
| `/` | GET | Health check |

### Step 1: Establish SSE Connection

Connect to the SSE endpoint first to listen for responses:

```bash
GET http://YOUR_IP:3210/sse

# Response (SSE stream, keep-alive)
Content-Type: text/event-stream
Cache-Control: no-cache

: keepalive ping\n\n
```

> The `sessionId` is included in the response header or the first message — include it in subsequent requests.

### Step 2: Send Initialize Request

```bash
POST http://YOUR_IP:3210/message
Content-Type: application/json

{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

**HTTP Response** (returned immediately, includes sessionId):

```json
{
  "sessionId": "sess-1777477748503866700"
}
```

**SSE Response** (pushed asynchronously on the connected `/sse` endpoint):

```json
event: message
data: {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"screenshot-mcp","version":"0.1.0"}}}
```

### Step 3: Call Tools

#### Screenshot (Core Feature)

```bash
POST http://YOUR_IP:3210/message
Content-Type: application/json

{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "tools/call",
  "params": {
    "name": "screenshot",
    "arguments": {
      "filters": {
        "mode": "include",
        "rules": [
          { "type": "process_name", "value": "NZMClient" }
        ]
      },
      "format": "png"
    }
  }
}
```

**SSE Pushed Result** (base64-encoded PNG image):

```json
event: message
data: {"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"image","data":"iVBORw0KGgoAAAANSUhEUgAA...","mimeType":"image/png"}]}}
```

#### Get Active Window

```bash
POST http://YOUR_IP:3210/message
Content-Type: application/json

{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_active_window","arguments":{}}}
```

**SSE Result**:

```json
event: message
data: {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"active_window\":{\"title\":\"Visual Studio Code\",\"process_name\":\"Code.exe\",\"pid\":9112,\"position\":{\"x\":93,\"y\":148,\"width\":2026,\"height\":963},\"is_visible\":true}}"}]}}
```

#### List Windows / Monitors

```bash
POST http://YOUR_IP:3210/message
Content-Type: application/json

{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_windows","arguments":{}}}

POST http://YOUR_IP:3210/message
Content-Type: application/json

{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list_monitors","arguments":{}}}
```

### Available Tools

| Tool | Description | Key Parameters |
|------|-------------|----------------|
| `screenshot` | Take a screen/window screenshot (WGC, zero interference) | `filters`, `monitor_index`, `format` |
| `list_windows` | List all visible windows | `include_minimized` |
| `get_active_window` | Get the current foreground window | — |
| `list_monitors` | List all connected monitors | — |

### Filter Rule Format

```json
{
  "filters": {
    "mode": "include",       // "include" = show only matches, "exclude" = hide matches
    "rules": [
      { "type": "process_name",   "value": "NZMClient" },   // By process name
      { "type": "title_contains",  "value": "Chrome" },      // Title contains
      { "type": "title_exact",     "value": "Notepad" },     // Title exact match
      { "type": "title_regex",     "value": "^Edge.*$" }     // Regular expression
    ]
  }
}
```

### Local Integration (Claude Desktop)

Add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "screenshot": {
      "command": "C:\\path\\to\\screenshot-mcp.exe",
      "args": ["mcp"]
    }
  }
}
```

---

## CLI Debug Commands

In addition to the MCP interface, CLI tools are provided for quick testing:

```bash
# Full screen screenshot
screenshot-mcp screenshot --output full.png

# Capture specific process only (OBS-style, excludes other windows)
screenshot-mcp screenshot --filter-process "NZMClient" --output game.png

# Multiple conditions (OR relationship)
screenshot-mcp screenshot --filter-process "Code" --filter-title "Edge" --output both.png

# Exclude mode (hide specified windows)
screenshot-mcp screenshot --mode exclude --filter-process "NZMClient" --output no_game.png

# Show the current active window
screenshot-mcp active-window

# List all windows / monitors
screenshot-mcp list-windows
screenshot-mcp list-monitors
```

| Parameter | Description | Default |
|-----------|-------------|---------|
| `--monitor <N>` | Monitor index | `0` |
| `--output <PATH>` | Output file path | `screenshot.png` |
| `--format png\|jpeg` | Image format | `png` |
| `--mode include\|exclude` | Filter mode | `include` |
| `--filter-process` | Filter by process name (repeatable) | — |
| `--filter-title` | Filter by title contains | — |
| `--filter-title-exact` | Filter by title exact match | — |
| `--filter-title-regex` | Filter by title regex match | — |

---

## Technical Architecture

```
┌─────────────────────────────────────────────────────┐
│                   Remote AI Bot                      │
│                                                     │
│  ┌──────────┐  GET /sse    ┌──────────────────┐     │
│  │ SSE Listen│ ◀────────── │                  │     │
│  │ (Response │             │                  │     │
│  │  Stream)  │             │                  │     │
│  └──────────┘              │                  │     │
│                             │                  │     │
│  ┌──────────┐  POST /msg   │   Axum HTTP      │     │
│  │  Send     │ ───────────▶ │   (Tokio)        │     │
│  │  Request  │             │                  │     │
│  └──────────┘              │  CORS + SSE      │     │
│                             │  /sse + /message │     │
│  Port: 3210                 │  MCP Handler     │     │
└─────────────────────────────│──────────────────┼─────┘
                              │  screenshot       │
                              │  get_active_win   │
                              │  list_windows     │
                              │  list_monitors    │
                              ├──────────────────┤
                              │  WGC (windows-capture)│
                              │  ├── GPU buffer direct │
                              │  └── DXGI fallback     │
                              └────────────────────────┘
```

### Why WGC?

| Approach | DX Content | Window Interference | HDR Support | Overlay Penetration |
|----------|-----------|--------------------|----|-----------|
| **WGC** ✅ | ✅ | ❌ Zero interference | ✅ | ✅ Native |
| PrintWindow | ❌ Composited layer | ❌ | ⚠️ | ❌ |
| DXGI + SetWindowPos | ✅ | ❌ Changes z-order | ⚠️ | ⚠️ Gaps |

### Dependencies

| Crate | Purpose |
|-------|---------|
| `windows-capture 1.5` | Windows Graphics Capture API wrapper |
| `windows 0.61` | Win32/DirectX bindings |
| `axum 0.7` + `tower-http` | HTTP server (native SSE support) |
| `tokio 1` | Async runtime |
| `async-stream` | SSE stream generation |
| `image 0.25` | PNG/JPEG encoding |

**System Requirements:** Windows 10 1903+ (build 18362) — required by the WGC API.

## License

This project is licensed under the [MIT License](LICENSE).
