# Screenshot MCP

[English](./README.md) | 中文

Windows 屏幕截图 **MCP Server**，使用 **Windows Graphics Capture (WGC)** 实现类似 OBS 的窗口捕获，支持 **MCP Streamable HTTP** 远程调用。

## 功能特性

- **OBS 式窗口捕获** — WGC 直接读取 GPU buffer，**零干扰**任何窗口状态
- **HDR 兼容** — BGRA8 和 FP16 格式，自动 tonemapping
- **窗口过滤** — 按进程名、标题（精确/包含/正则）过滤，include/exclude 双模式
- **MCP Streamable HTTP** — SSE 双向通道，远端机器人跨网络调用
- **本地 stdio 模式** — 兼容 Claude Desktop / Cursor 等本地 AI Agent
- **获取激活窗口** — 查询当前前台/焦点窗口信息
- **多显示器** — 自动检测或指定显示器索引

## 它能做什么

最常见的用法是可以在你的群机器人里使用，能够让大家随时能来视奸你在做什么。

当然，你肯定是不希望你的隐私内容也被泄露出去，所以可以让 AI **只能发送你特定类型的窗口**画面，例如游戏。

可以让 AI 先调用`list_windows`，获取正在运行的窗口，让它只能选择游戏类型的窗口画面然后发送截图。

## 快速开始

```bash
cargo build --release
```

编译产物：`target/release/screenshot-mcp.exe`

### 启动方式

| 方式 | 命令 | 说明 |
|------|------|------|
| **双击启动** | 直接运行 exe | 启动 Streamable HTTP MCP Server (`http://0.0.0.0:3210`) |
| HTTP 模式 | `screenshot-mcp mcp --transport http --port 3210` | 远程访问（默认端口 3210） |
| stdio 模式 | `screenshot-mcp mcp` | 本地 Claude Desktop 集成 |

> **推荐：直接双击 `screenshot-mcp.exe` 即可启动服务，无需任何参数。**

启动后会看到：
```
[screenshot-mcp] Listening on http://0.0.0.0:3210 (MCP Streamable HTTP)
[screenshot-mcp] Ready. SSE endpoint: http://0.0.0.0:3210/sse
[screenshot-mcp] Message endpoint: POST http://0.0.0.0:3210/message
```

---

## MCP Streamable HTTP 协议

本服务遵循 [MCP Streamable HTTP Transport](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports#streamable-http) 规范。

### 协议流程

```
┌──────────┐         GET /sse?sessionId=xxx          ┌──────────┐
│          │ ──────────────────────────────────────▶  │          │
│   客户端  │    SSE 长连接（接收响应事件流）             │  服务端   │
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

### 端点

| 端点 | 方法 | 说明 |
|------|------|------|
| `/sse` | GET | SSE 事件流，返回 JSON-RPC 响应。支持 `?sessionId=xxx` 参数复用会话 |
| `/message` | POST | 发送 JSON-RPC 请求。响应通过 SSE 推送，HTTP 返回 `sessionId` |
| `/` | GET | 健康检查 |

### 步骤一：建立 SSE 连接

客户端先连接 SSE 端点，监听响应：

```bash
GET http://YOUR_IP:3210/sse

# 响应（SSE 流，保持长连接）
Content-Type: text/event-stream
Cache-Control: no-cache

: keepalive ping\n\n
```

> 返回头中或首次消息中会包含 `sessionId`，后续请求需要带上。

### 步骤二：发送初始化请求

```bash
POST http://YOUR_IP:3210/message
Content-Type: application/json

{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

**HTTP 响应**（立即返回，包含 sessionId）：

```json
{
  "sessionId": "sess-1777477748503866700"
}
```

**SSE 响应**（在已连接的 `/sse` 端点上异步推送）：

```json
event: message
data: {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"screenshot-mcp","version":"0.1.0"}}}
```

### 步骤三：调用工具

#### 截图（核心功能）

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

**SSE 推送结果**（base64 编码的 PNG 图片）：

```json
event: message
data: {"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"image","data":"iVBORw0KGgoAAAANSUhEUgAA...","mimeType":"image/png"}]}}
```

#### 获取当前激活窗口

```bash
POST http://YOUR_IP:3210/message
Content-Type: application/json

{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_active_window","arguments":{}}}
```

**SSE 结果**：

```json
event: message
data: {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"active_window\":{\"title\":\"Visual Studio Code\",\"process_name\":\"Code.exe\",\"pid\":9112,\"position\":{\"x\":93,\"y\":148,\"width\":2026,\"height\":963},\"is_visible\":true}}"}]}}
```

#### 列出窗口 / 显示器

```bash
POST http://YOUR_IP:3210/message
Content-Type: application/json

{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_windows","arguments":{}}}

POST http://YOUR_IP:3210/message
Content-Type: application/json

{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list_monitors","arguments":{}}}
```

### 可用工具一览

| 工具名 | 说明 | 关键参数 |
|--------|------|----------|
| `screenshot` | 截取屏幕/窗口截图（WGC，零干扰） | `filters`, `monitor_index`, `format` |
| `list_windows` | 列出所有可见窗口 | `include_minimized` |
| `get_active_window` | 获取当前焦点窗口 | 无 |
| `list_monitors` | 列出所有显示器 | 无 |

### 过滤规则格式

```json
{
  "filters": {
    "mode": "include",       // "include" = 只显示匹配项, "exclude" = 隐藏匹配项
    "rules": [
      { "type": "process_name",   "value": "NZMClient" },   // 按进程名
      { "type": "title_contains",  "value": "Chrome" },      // 标题包含
      { "type": "title_exact",     "value": "Notepad" },     // 标题精确匹配
      { "type": "title_regex",     "value": "^Edge.*$" }     // 正则表达式
    ]
  }
}
```

### 本地集成（Claude Desktop）

在 `claude_desktop_config.json` 中添加：

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

## CLI 调试命令

除了 MCP 接口，还提供命令行工具用于快速测试：

```bash
# 全屏截图
screenshot-mcp screenshot --output full.png

# 只截取指定进程（OBS 风格，排除其他窗口）
screenshot-mcp screenshot --filter-process "NZMClient" --output game.png

# 多条件组合（OR 关系）
screenshot-mcp screenshot --filter-process "Code" --filter-title "Edge" --output both.png

# 排除模式（隐藏指定窗口）
screenshot-mcp screenshot --mode exclude --filter-process "NZMClient" --output no_game.png

# 查看当前激活的窗口
screenshot-mcp active-window

# 列出所有窗口 / 显示器
screenshot-mcp list-windows
screenshot-mcp list-monitors
```

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `--monitor <N>` | 显示器索引 | `0` |
| `--output <PATH>` | 输出文件路径 | `screenshot.png` |
| `--format png\|jpeg` | 图片格式 | `png` |
| `--mode include\|exclude` | 过滤模式 | `include` |
| `--filter-process` | 按进程名过滤（可重复） | - |
| `--filter-title` | 按标题包含过滤 | - |
| `--filter-title-exact` | 按标题精确匹配 | - |
| `--filter-title-regex` | 按标题正则匹配 | - |

---

## 技术架构

```
┌─────────────────────────────────────────────────────┐
│                   远端 AI 机器人                      │
│                                                     │
│  ┌──────────┐  GET /sse    ┌──────────────────┐     │
│  │ SSE 监听  │ ◀────────── │                  │     │
│  │ (响应流)  │             │                  │     │
│  └──────────┘              │                  │     │
│                             │                  │     │
│  ┌──────────┐  POST /msg   │   Axum HTTP      │     │
│  │ 发送请求  │ ───────────▶ │   (Tokio)        │     │
│  └──────────┘              │                  │     │
│                             │  CORS + SSE      │     │
│  端口: 3210                 │  /sse + /message │     │
└─────────────────────────────│──────────────────┼─────┘
                              │  MCP Handler     │
                              ├──────────────────┤
                              │  screenshot       │
                              │  get_active_win   │
                              │  list_windows     │
                              │  list_monitors    │
                              ├──────────────────┤
                              │  WGC (windows-capture)│
                              │  ├── GPU buffer 直接读  │
                              │  └── DXGI 回退          │
                              └────────────────────────┘
```

### 为什么用 WGC？

| 方案 | 能读 DX 内容 | 干扰窗口状态 | HDR 支持 | 叠加窗穿透 |
|------|------------|------------|---------|-----------|
| **WGC** ✅ | ✅ | ❌ 零干扰 | ✅ | ✅ 天然穿透 |
| PrintWindow | ❌ 返回合成层 | ❌ | ⚠️ | ❌ |
| DXGI + SetWindowPos | ✅ | ❌ 会改变 z-order | ⚠️ | ⚠️ 有空洞 |

### 依赖

| 包 | 用途 |
|----|------|
| `windows-capture 1.5` | Windows Graphics Capture API 封装 |
| `windows 0.61` | Win32/DirectX 绑定 |
| `axum 0.7` + `tower-http` | HTTP 服务器（原生 SSE 支持） |
| `tokio 1` | 异步运行时 |
| `async-stream` | SSE 流生成 |
| `image 0.25` | PNG/JPEG 编码 |

系统要求：**Windows 10 1903+ (build 18362)**，WGC API 需要。

## 许可证

本项目基于 [MIT 许可证](LICENSE) 开源。
