use crate::capture::{self, ImageFormat};
use crate::window::{self, FilterMode, FilterRule, FilterType, WindowFilter};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse},
    routing::{get, post},
    Json,
};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::broadcast;

const PROTOCOL_VERSION: &str = "2024-11-05";

// ── JSON-RPC types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

// ── App state (SSE sessions) ──────────────────────────────────────────

struct AppState {
    sessions: tokio::sync::RwLock<HashMap<String, broadcast::Sender<Vec<u8>>>>,
}

impl AppState {
    fn new() -> Self {
        Self { sessions: tokio::sync::RwLock::new(HashMap::new()) }
    }

    async fn create_session(&self) -> String {
        let sid = format!("sess-{}", nanos_id());
        let (tx, _) = broadcast::channel(64);
        self.sessions.write().await.insert(sid.clone(), tx);
        sid
    }

    async fn get_tx(&self, sid: &str) -> Option<broadcast::Sender<Vec<u8>>> {
        self.sessions.read().await.get(sid).cloned()
    }
}

fn nanos_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

// ══════════════════════════════════════════════════════════════════════
//  Entry points
// ══════════════════════════════════════════════════════════════════════

/// Stdio MCP server (local mode)
pub fn run_server() -> Result<(), String> {
    tracing::info!("MCP server starting (stdio)");
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut r = stdin.lock();
    let mut w = stdout.lock();
    loop {
        match read_msg(&mut r) {
            Ok(Some(req)) => {
                if let Some(resp) = handle_sync(&req) {
                    write_msg(&mut w, &resp)?;
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::error!("Stdio error: {}", e);
                break;
            }
        }
    }
    Ok(())
}

/// Streamable HTTP MCP server (remote mode)
pub async fn run_http_server(host: &str, port: u16) -> Result<(), Infallible> {
    let state = Arc::new(AppState::new());
    let addr: SocketAddr = format!("{}:{}", host, port).parse().expect("Invalid address");
    tracing::info!("MCP Streamable HTTP on http://{}", addr);
    eprintln!("[screenshot-mcp] Listening on http://{} (MCP Streamable HTTP)", addr);

    let cors = tower_http::cors::CorsLayer::permissive();

    let app = axum::Router::new()
        .route("/sse", get(sse_handler))
        .route("/message", post(message_handler))
        .route("/", get(health_check).post(message_handler))  // root accepts both GET (health) + POST (message)
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await.expect("Bind failed");
    eprintln!("[screenshot-mcp] Ready. SSE endpoint: http://{}/sse", addr);
    eprintln!("[screenshot-mcp] Message endpoint: POST http://{}/message", addr);
    axum::serve(listener, app).await.ok();
    Ok(())
}

// ── SSE endpoint (/sse?sessionId=...) ────────────────────────────────

async fn sse_handler(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Always ensure we have a valid session
    // If sessionId provided and valid → reuse it; otherwise create new
    let sid = params.get("sessionId").cloned().unwrap_or_default();

    let final_sid = if !sid.is_empty() && state.get_tx(&sid).await.is_some() {
        sid
    } else {
        state.create_session().await
    };

    let rx = match state.get_tx(&final_sid).await {
        Some(tx) => tx.subscribe(),
        None => {
            // Should never happen but handle gracefully
            let (tx, _) = broadcast::channel(1);
            tx.subscribe()
        }
    };

    Sse::new(sse_rx_stream(rx)).keep_alive(
        KeepAlive::new().interval(std::time::Duration::from_secs(15))
    )
}

// ── Message endpoint (POST /message) ─────────────────────────────────

async fn message_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    tracing::debug!("Message: {}", req.method);

    // Create or use session
    let sid = state.create_session().await;

    // Process request synchronously
    match handle_sync(&req) {
        Some(resp) => {
            // Send response via SSE channel (for session-based listeners)
            let payload = serde_json::to_string(&resp).unwrap_or_default();
            if let Some(tx) = state.get_tx(&sid).await {
                let _ = tx.send(payload.clone().into_bytes());
            }
            // Return complete JSON-RPC response in HTTP body
            // (Streamable HTTP spec: POST must respond with JSON-RPC envelope)
            let resp_builder = axum::response::Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header("Mcp-Session-Id", &sid);
            
            let body_bytes = serde_json::to_vec(&resp).unwrap_or_default();
            resp_builder.body(axum::body::Body::from(body_bytes)).expect("Failed to build response")
        }
        None => {
            // Notification → no response body
            StatusCode::NO_CONTENT.into_response()
        }
    }
}

// ── Health check ──────────────────────────────────────────────────────

async fn health_check() -> Json<Value> {
    Json(json!({
        "name": "screenshot-mcp",
        "version": "0.1.0",
        "protocol": "mcp-streamable-http",
        "status": "ok"
    }))
}

// ══════════════════════════════════════════════════════════════════════
//  SSE stream helpers
// ══════════════════════════════════════════════════════════════════════

fn sse_rx_stream(mut rx: broadcast::Receiver<Vec<u8>>) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        while let Ok(msg) = rx.recv().await {
            let text = String::from_utf8_lossy(&msg);
            yield Ok(Event::default().data(text));
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
//  Request handler (shared between stdio + HTTP)
// ══════════════════════════════════════════════════════════════════════

/// Handle a JSON-RPC request. Returns None for notifications (no response).
fn handle_sync(req: &JsonRpcRequest) -> Option<Value> {
    match req.method.as_str() {
        "initialize" => Some(init_ok(req.id.clone())),
        "notifications/initialized" | "notifications/cancelled" => None,
        "tools/list" => Some(tools_list_resp(req.id.clone())),
        "tools/call" => Some(tool_call_resp(req)),
        "ping" => Some(pong_resp(req.id.clone())),
        other => Some(err_resp(req.id.clone(), -32601, &format!("Unknown method: {}", other))),
    }
}

fn init_ok(id: Option<Value>) -> Value {
    ok_resp(id, json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {},
        },
        "serverInfo": {"name": "screenshot-mcp", "version": "0.1.0"},
    }))
}

fn pong_resp(id: Option<Value>) -> Value {
    ok_resp(id, json!({}))
}

fn tools_list_resp(id: Option<Value>) -> Value {
    ok_resp(id, json!({"tools": [
        {
            "name": "screenshot",
            "description": "OBS-style window screenshot via Windows Graphics Capture API (WGC). Zero interference with windows.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "monitor_index": {"type": "integer", "default": 0, "description": "Monitor index to capture"},
                    "filters": {
                        "type": "object",
                        "properties": {
                            "mode": {"enum": ["include", "exclude"], "default": "include"},
                            "rules": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "type": {"enum": ["title_exact", "title_contains", "title_regex", "process_name"]},
                                        "value": {"type": "string"}
                                    },
                                    "required": ["type", "value"]
                                }
                            }
                        }
                    },
                    "format": {"enum": ["png", "jpeg"], "default": "png"}
                }
            }
        },
        {
            "name": "list_windows",
            "description": "List all visible windows on the desktop",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "include_minimized": {"type": "boolean", "default": false}
                }
            }
        },
        {
            "name": "get_active_window",
            "description": "Get the currently active/foreground window information",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "list_monitors",
            "description": "List all connected monitors/displays",
            "inputSchema": {"type": "object", "properties": {}}
        },
    ]}))
}

fn tool_call_resp(req: &JsonRpcRequest) -> Value {
    let params_obj = obj_val(&req.params);
    let tool_name = params_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");

    let result = match tool_name {
        "screenshot" => do_screenshot_tool(req),
        "list_windows" => do_list_windows_tool(req),
        "get_active_window" => do_active_window_tool(req),
        "list_monitors" => do_list_monitors_tool(req),
        other => err_resp(req.id.clone(), -32602, &format!("Unknown tool: {}", other)),
    };
    result
}

// ── Tool implementations ──────────────────────────────────────────────

fn do_screenshot_tool(req: &JsonRpcRequest) -> Value {
    let args = obj_val(&req.params).get("arguments").cloned().unwrap_or(json!({}));
    let monitor_idx = args.get("monitor_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let fmt_str = args.get("format").and_then(|v| v.as_str()).unwrap_or("png");
    let img_fmt = match fmt_str {
        "jpeg" | "jpg" => ImageFormat::Jpeg,
        _ => ImageFormat::Png,
    };

    let filters = parse_filters_from_args(&args);

    match capture::take_screenshot(monitor_idx, filters.as_ref(), img_fmt) {
        Ok(data) => {
            let b64 = capture::image_to_base64(&data);
            let mime = capture::format_mime_type(img_fmt);
            ok_resp(req.id.clone(), json!({
                "content": [{
                    "type": "image",
                    "data": b64,
                    "mimeType": mime,
                }]
            }))
        }
        Err(e) => err_resp(req.id.clone(), -32000, &e.to_string()),
    }
}

fn do_list_windows_tool(req: &JsonRpcRequest) -> Value {
    let args = obj_val(&req.params).get("arguments").cloned().unwrap_or(json!({}));
    let include_min = args.get("include_minimized").and_then(|v| v.as_bool()).unwrap_or(false);

    let wins = window::enumerate_windows(include_min);
    let win_list: Vec<Value> = wins.iter().map(|w| json!({
        "title": w.title,
        "process_name": w.process_name,
        "pid": w.pid,
        "position": {
            "x": w.rect.left,
            "y": w.rect.top,
            "width": w.rect.width(),
            "height": w.rect.height(),
        }
    })).collect();

    ok_resp(req.id.clone(), json!({
        "content": [{"type": "text", "text": json!({"windows": win_list}).to_string()}]
    }))
}

fn do_active_window_tool(req: &JsonRpcRequest) -> Value {
    match window::get_active_window() {
        Some(w) => {
            let info = json!({
                "title": w.title,
                "process_name": w.process_name,
                "pid": w.pid,
                "position": {
                    "x": w.rect.left,
                    "y": w.rect.top,
                    "width": w.rect.width(),
                    "height": w.rect.height(),
                },
                "is_visible": w.is_visible,
            });
            ok_resp(req.id.clone(), json!({
                "content": [{"type": "text", "text": json!({"active_window": info}).to_string()}]
            }))
        }
        None => err_resp(req.id.clone(), -32001, "No active window found"),
    }
}

fn do_list_monitors_tool(req: &JsonRpcRequest) -> Value {
    let monitors = window::enumerate_monitors();
    let mon_list: Vec<Value> = monitors.iter().map(|m| json!({
        "index": m.index,
        "name": m.name,
        "is_primary": m.is_primary,
        "position": {
            "x": m.rect.left,
            "y": m.rect.top,
            "width": m.rect.width(),
            "height": m.rect.height(),
        }
    })).collect();

    ok_resp(req.id.clone(), json!({
        "content": [{"type": "text", "text": json!({"monitors": mon_list}).to_string()}]
    }))
}

// ══════════════════════════════════════════════════════════════════════
//  Stdio transport I/O
// ══════════════════════════════════════════════════════════════════════

/// Read one JSON-RPC message from stdin (Content-Length header protocol).
fn read_msg(r: &mut impl BufRead) -> Result<Option<JsonRpcRequest>, String> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break; // End of headers
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(val.trim().parse::<usize>().map_err(|e: std::num::ParseIntError| e.to_string())?);
        }
    }

    let len = content_length.ok_or("Missing Content-Length header")?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8(buf).map_err(|e| e.to_string())?;
    let req: JsonRpcRequest = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(Some(req))
}

/// Write one JSON-RPC response to stdout.
fn write_msg(w: &mut impl Write, resp: &Value) -> Result<(), String> {
    let body = serde_json::to_string(resp).map_err(|e| e.to_string())?;
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)
        .map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())?;
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════
//  Helpers
// ══════════════════════════════════════════════════════════════════════

fn parse_filters_from_args(args: &Value) -> Option<WindowFilter> {
    let filters_obj = args.get("filters")?;

    let mode_str = filters_obj.get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("include");
    let mode = match mode_str {
        "exclude" => FilterMode::Exclude,
        _ => FilterMode::Include,
    };

    let rules: Vec<FilterRule> = filters_obj
        .get("rules")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let t = item.get("type")?.as_str()?;
                    let v = item.get("value")?.as_str()?.to_string();
                    Some(match t {
                        "title_exact" => FilterRule { rule_type: FilterType::TitleExact, value: v },
                        "title_contains" => FilterRule { rule_type: FilterType::TitleContains, value: v },
                        "title_regex" => FilterRule { rule_type: FilterType::TitleRegex, value: v },
                        "process_name" => FilterRule { rule_type: FilterType::ProcessName, value: v },
                        _ => return None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if rules.is_empty() {
        None
    } else {
        Some(WindowFilter { mode, rules })
    }
}

#[inline]
fn ok_resp(id: Option<Value>, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

#[inline]
fn err_resp(id: Option<Value>, code: i32, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

#[inline]
fn obj_val(v: &Value) -> Value {
    if let Value::Object(_) = v {
        v.clone()
    } else {
        json!({})
    }
}
