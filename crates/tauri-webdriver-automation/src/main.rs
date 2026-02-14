// tauri-webdriver-automation: W3C WebDriver server for Tauri apps on macOS.
//
// Launches the Tauri app, discovers the plugin's HTTP port from stdout,
// and translates W3C WebDriver commands into plugin API calls.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;

use axum::extract::{Path, State as AxumState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use clap::Parser;
use serde_json::{json, Value};
use tokio::io::AsyncBufReadExt;
use tokio::sync::Mutex;

const W3C_ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
const W3C_SHADOW_KEY: &str = "shadow-6066-11e4-a52e-4f735466cecf";

// --- CLI arguments ---

#[derive(Parser)]
#[command(name = "tauri-wd", about = "W3C WebDriver server for Tauri apps")]
struct Cli {
    /// WebDriver server port
    #[arg(long, default_value = "4444")]
    port: u16,

    /// WebDriver server host
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Log level: error, warn, info, debug, trace
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Maximum concurrent sessions (0 = unlimited)
    #[arg(long, default_value = "0")]
    max_sessions: usize,
}

// --- State types ---

struct ElementRef {
    selector: String,
    index: usize,
    using: String,
}

struct ShadowRef {
    host_selector: String,
    host_index: usize,
    host_using: String,
}

struct Timeouts {
    script: u64,    // ms, default 30000
    page_load: u64, // ms, default 300000
    implicit: u64,  // ms, default 0
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            script: 30000,
            page_load: 300000,
            implicit: 0,
        }
    }
}

struct Session {
    plugin_url: String,
    process: tokio::process::Child,
    elements: HashMap<String, ElementRef>,
    shadows: HashMap<String, ShadowRef>,
    client: reqwest::Client,
    timeouts: Timeouts,
}

struct AppState {
    sessions: Mutex<HashMap<String, Session>>,
    max_sessions: usize,
}

type SharedState = Arc<AppState>;

// --- W3C error handling ---

struct W3cError {
    status: StatusCode,
    error: String,
    message: String,
}

impl W3cError {
    fn new(status: StatusCode, error: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            error: error.to_string(),
            message: message.into(),
        }
    }
    fn no_session() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "invalid session id",
            "No active session",
        )
    }
    fn no_element(id: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "no such element",
            format!("Element {id} not found"),
        )
    }
    fn session_not_created(msg: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session not created",
            msg,
        )
    }
    fn unknown(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "unknown error", msg)
    }
    fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid argument", msg)
    }
    fn javascript_error(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "javascript error", msg)
    }
}

impl IntoResponse for W3cError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "value": {
                    "error": self.error,
                    "message": self.message,
                    "stacktrace": ""
                }
            })),
        )
            .into_response()
    }
}

type W3cResult = Result<Json<Value>, W3cError>;

// --- Helpers ---

fn w3c_value(val: Value) -> Json<Value> {
    Json(json!({"value": val}))
}

async fn plugin_post(session: &Session, path: &str, body: Value) -> Result<Value, W3cError> {
    let url = format!("{}{}", session.plugin_url, path);
    let resp = session
        .client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| W3cError::unknown(format!("plugin request failed: {e}")))?;

    let status = resp.status();
    let val: Value = resp
        .json()
        .await
        .map_err(|e| W3cError::unknown(format!("plugin response parse failed: {e}")))?;

    if !status.is_success() {
        let msg = val
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("plugin error");
        return Err(W3cError::unknown(msg));
    }

    Ok(val)
}

fn resolve_element<'a>(session: &'a Session, eid: &str) -> Result<&'a ElementRef, W3cError> {
    session
        .elements
        .get(eid)
        .ok_or_else(|| W3cError::no_element(eid))
}

fn extract_locator(body: &Value) -> Result<(String, String), W3cError> {
    let strategy = body
        .get("using")
        .and_then(|v| v.as_str())
        .ok_or_else(|| W3cError::bad_request("Missing 'using'"))?;
    let value = body
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| W3cError::bad_request("Missing 'value'"))?;

    let (using, actual_value) = match strategy {
        "css selector" => ("css".to_string(), value.to_string()),
        "tag name" => ("css".to_string(), value.to_string()),
        "xpath" => ("xpath".to_string(), value.to_string()),
        "link text" => (
            "xpath".to_string(),
            format!("//a[normalize-space()='{}']", value),
        ),
        "partial link text" => ("xpath".to_string(), format!("//a[contains(.,'{}')]", value)),
        other => {
            return Err(W3cError::bad_request(format!(
                "Unsupported locator strategy: {other}"
            )))
        }
    };

    Ok((using, actual_value))
}

fn store_element(session: &mut Session, elem: &Value) -> String {
    let selector = elem
        .get("selector")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let index = elem.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
    let using = elem
        .get("using")
        .and_then(|u| u.as_str())
        .unwrap_or("css")
        .to_string();

    // Return existing ID if we already mapped this exact element.
    for (eid, eref) in &session.elements {
        if eref.selector == selector && eref.index == index && eref.using == using {
            return eid.clone();
        }
    }

    let eid = uuid::Uuid::new_v4().to_string();
    session.elements.insert(
        eid.clone(),
        ElementRef {
            selector,
            index,
            using,
        },
    );
    eid
}

fn get_session<'a>(
    sessions: &'a HashMap<String, Session>,
    sid: &str,
) -> Result<&'a Session, W3cError> {
    sessions.get(sid).ok_or(W3cError::no_session())
}

fn get_session_mut<'a>(
    sessions: &'a mut HashMap<String, Session>,
    sid: &str,
) -> Result<&'a mut Session, W3cError> {
    sessions.get_mut(sid).ok_or(W3cError::no_session())
}

// --- Session handlers ---

async fn get_status(AxumState(state): AxumState<SharedState>) -> Json<Value> {
    let sessions = state.sessions.lock().await;
    let count = sessions.len();
    let ready = state.max_sessions == 0 || count < state.max_sessions;
    w3c_value(json!({
        "ready": ready,
        "message": if count == 0 {
            "ready".to_string()
        } else if ready {
            format!("{count} session(s) active, accepting more")
        } else {
            format!("{count} session(s) active, at capacity")
        }
    }))
}

async fn create_session(
    AxumState(state): AxumState<SharedState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), W3cError> {
    let mut sessions = state.sessions.lock().await;
    if state.max_sessions > 0 && sessions.len() >= state.max_sessions {
        return Err(W3cError::session_not_created(
            "Maximum number of sessions reached",
        ));
    }

    // Extract binary path from capabilities.
    // Accept both "binary" and "application" as capability keys.
    let binary = body
        .pointer("/capabilities/alwaysMatch/tauri:options/binary")
        .or_else(|| body.pointer("/capabilities/alwaysMatch/tauri:options/application"))
        .or_else(|| body.pointer("/capabilities/firstMatch/0/tauri:options/binary"))
        .or_else(|| body.pointer("/capabilities/firstMatch/0/tauri:options/application"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            W3cError::session_not_created(
                "Missing tauri:options.binary (or application) in capabilities",
            )
        })?
        .to_string();

    // Launch the Tauri app.
    let mut child = tokio::process::Command::new(&binary)
        .env("TAURI_WEBVIEW_AUTOMATION", "true")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| W3cError::session_not_created(format!("Failed to launch {binary}: {e}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| W3cError::session_not_created("Failed to capture app stdout"))?;

    // Watch stdout for the plugin port announcement.
    let mut reader = tokio::io::BufReader::new(stdout).lines();
    let mut port: Option<u16> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    loop {
        match tokio::time::timeout_at(deadline, reader.next_line()).await {
            Ok(Ok(Some(line))) => {
                tracing::debug!("app stdout: {}", line);
                if let Some(rest) = line.strip_prefix("[webdriver] listening on port ") {
                    if let Ok(p) = rest.trim().parse::<u16>() {
                        port = Some(p);
                        break;
                    }
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                return Err(W3cError::session_not_created(format!(
                    "IO error reading app stdout: {e}"
                )));
            }
            Err(_) => break,
        }
    }

    let port = port
        .ok_or_else(|| W3cError::session_not_created("App did not report plugin port in time"))?;

    // Drain remaining stdout in background so the app doesn't block.
    tokio::spawn(async move {
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::trace!("app: {}", line);
        }
    });

    let session_id = uuid::Uuid::new_v4().to_string();
    let plugin_url = format!("http://127.0.0.1:{port}");
    tracing::info!("Session {session_id} created, plugin at {plugin_url}");

    sessions.insert(
        session_id.clone(),
        Session {
            plugin_url,
            process: child,
            elements: HashMap::new(),
            shadows: HashMap::new(),
            client: reqwest::Client::new(),
            timeouts: Timeouts::default(),
        },
    );

    Ok((
        StatusCode::OK,
        w3c_value(json!({
            "sessionId": session_id,
            "capabilities": {
                "browserName": "tauri",
                "platformName": "mac",
                "tauri:options": { "binary": binary }
            }
        })),
    ))
}

async fn delete_session(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let mut sessions = state.sessions.lock().await;
    let mut session = sessions.remove(&sid).ok_or(W3cError::no_session())?;
    let _ = session.process.kill().await;
    tracing::info!("Session {sid} deleted");
    Ok(w3c_value(json!(null)))
}

// --- Timeouts handlers ---

async fn get_timeouts(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    Ok(w3c_value(json!({
        "script": session.timeouts.script,
        "pageLoad": session.timeouts.page_load,
        "implicit": session.timeouts.implicit
    })))
}

async fn set_timeouts(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let mut guard = state.sessions.lock().await;
    let session = get_session_mut(&mut guard, &sid)?;
    if let Some(v) = body.get("script").and_then(|v| v.as_u64()) {
        session.timeouts.script = v;
    }
    if let Some(v) = body.get("pageLoad").and_then(|v| v.as_u64()) {
        session.timeouts.page_load = v;
    }
    if let Some(v) = body.get("implicit").and_then(|v| v.as_u64()) {
        session.timeouts.implicit = v;
    }
    Ok(w3c_value(json!(null)))
}

// --- Navigation handlers ---

async fn navigate_to(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let url = body
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| W3cError::bad_request("Missing url"))?;
    plugin_post(session, "/navigate/url", json!({"url": url})).await?;
    Ok(w3c_value(json!(null)))
}

async fn get_url(AxumState(state): AxumState<SharedState>, Path(sid): Path<String>) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/navigate/current", json!({})).await?;
    Ok(w3c_value(result.get("url").cloned().unwrap_or(json!(""))))
}

async fn get_title(AxumState(state): AxumState<SharedState>, Path(sid): Path<String>) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/navigate/title", json!({})).await?;
    Ok(w3c_value(result.get("title").cloned().unwrap_or(json!(""))))
}

async fn go_back(AxumState(state): AxumState<SharedState>, Path(sid): Path<String>) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/navigate/back", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

async fn go_forward(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/navigate/forward", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

async fn refresh(AxumState(state): AxumState<SharedState>, Path(sid): Path<String>) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/navigate/refresh", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

// --- Window handlers ---

async fn get_window_handle(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/window/handle", json!({})).await?;
    Ok(w3c_value(result))
}

async fn close_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let handle = plugin_post(session, "/window/handle", json!({})).await?;
    let label = handle.as_str().unwrap_or("main");
    plugin_post(session, "/window/close", json!({"label": label})).await?;
    let handles = plugin_post(session, "/window/handles", json!({})).await?;
    Ok(w3c_value(handles))
}

async fn get_window_handles(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/window/handles", json!({})).await?;
    Ok(w3c_value(result))
}

async fn get_window_rect(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

async fn set_window_rect(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/window/set-rect", body).await?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

async fn maximize_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/window/maximize", json!({})).await?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

async fn minimize_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/window/minimize", json!({})).await?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

async fn fullscreen_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/window/fullscreen", json!({})).await?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

// --- New window handler ---

async fn new_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/window/new", body).await?;
    let handle = result.get("handle").cloned().unwrap_or(json!(""));
    let type_val = result.get("type").cloned().unwrap_or(json!("window"));
    Ok(w3c_value(json!({"handle": handle, "type": type_val})))
}

// --- Element handlers ---

async fn find_element(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let mut guard = state.sessions.lock().await;
    let session = get_session_mut(&mut guard, &sid)?;
    let (using, value) = extract_locator(&body)?;
    let result = plugin_post(
        session,
        "/element/find",
        json!({"using": using, "value": value}),
    )
    .await?;

    let elements = result
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| {
            W3cError::new(
                StatusCode::NOT_FOUND,
                "no such element",
                format!("No element found with {using}: {value}"),
            )
        })?;

    if elements.is_empty() {
        return Err(W3cError::new(
            StatusCode::NOT_FOUND,
            "no such element",
            format!("No element found with {using}: {value}"),
        ));
    }

    let eid = store_element(session, &elements[0]);
    Ok(w3c_value(json!({W3C_ELEMENT_KEY: eid})))
}

async fn find_elements(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let mut guard = state.sessions.lock().await;
    let session = get_session_mut(&mut guard, &sid)?;
    let (using, value) = extract_locator(&body)?;
    let result = plugin_post(
        session,
        "/element/find",
        json!({"using": using, "value": value}),
    )
    .await?;

    let empty = vec![];
    let elements = result
        .get("elements")
        .and_then(|e| e.as_array())
        .unwrap_or(&empty);

    let mapped: Vec<Value> = elements
        .iter()
        .map(|elem| {
            let eid = store_element(session, elem);
            json!({W3C_ELEMENT_KEY: eid})
        })
        .collect();

    Ok(w3c_value(json!(mapped)))
}

async fn click_element(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    plugin_post(
        session,
        "/element/click",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(json!(null)))
}

async fn clear_element(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    plugin_post(
        session,
        "/element/clear",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(json!(null)))
}

async fn send_keys(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");

    // Check if this is a file input by querying its tag and type attribute.
    let tag_result = plugin_post(
        session,
        "/element/tag",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    let tag = tag_result.get("tag").and_then(|v| v.as_str()).unwrap_or("");

    if tag.eq_ignore_ascii_case("input") {
        let attr_result = plugin_post(
            session,
            "/element/attribute",
            json!({"selector": elem.selector, "index": elem.index, "using": elem.using, "name": "type"}),
        )
        .await?;
        let input_type = attr_result
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if input_type.eq_ignore_ascii_case("file") {
            // W3C spec: text contains newline-separated file paths.
            let paths: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
            let mut files = Vec::new();
            for path in &paths {
                let data = tokio::fs::read(path)
                    .await
                    .map_err(|e| W3cError::bad_request(format!("Cannot read file {path}: {e}")))?;
                let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                let name = std::path::Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();
                let mime = mime_from_extension(path);
                files.push(json!({"name": name, "data": encoded, "mime": mime}));
            }
            plugin_post(
                session,
                "/element/set-files",
                json!({"selector": elem.selector, "index": elem.index, "using": elem.using, "files": files}),
            )
            .await?;
            return Ok(w3c_value(json!(null)));
        }
    }

    plugin_post(
        session,
        "/element/send-keys",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using, "text": text}),
    )
    .await?;
    Ok(w3c_value(json!(null)))
}

fn mime_from_extension(path: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
    .to_string()
}

async fn get_element_text(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/text",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(result.get("text").cloned().unwrap_or(json!(""))))
}

async fn get_element_tag(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/tag",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(result.get("tag").cloned().unwrap_or(json!(""))))
}

async fn get_element_attribute(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid, name)): Path<(String, String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/attribute",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using, "name": name}),
    )
    .await?;
    Ok(w3c_value(
        result.get("value").cloned().unwrap_or(Value::Null),
    ))
}

async fn get_element_property(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid, name)): Path<(String, String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/property",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using, "name": name}),
    )
    .await?;
    Ok(w3c_value(
        result.get("value").cloned().unwrap_or(Value::Null),
    ))
}

async fn get_element_css(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid, name)): Path<(String, String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    // CSS values use the property endpoint with a computed-style JS property.
    let result = plugin_post(
        session,
        "/element/property",
        json!({
            "selector": elem.selector,
            "index": elem.index,
            "using": elem.using,
            "name": format!("__css__{name}")
        }),
    )
    .await;
    // Fallback: if the plugin doesn't support __css__ convention, return empty.
    let val = match result {
        Ok(v) => v.get("value").cloned().unwrap_or(json!("")),
        Err(_) => json!(""),
    };
    Ok(w3c_value(val))
}

async fn get_element_rect(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/rect",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(result))
}

async fn is_element_enabled(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/enabled",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(
        result.get("enabled").cloned().unwrap_or(json!(true)),
    ))
}

async fn is_element_selected(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/selected",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(
        result.get("selected").cloned().unwrap_or(json!(false)),
    ))
}

async fn is_element_displayed(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/displayed",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(
        result.get("displayed").cloned().unwrap_or(json!(true)),
    ))
}

// --- Script handlers ---

async fn execute_sync(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let script = body.get("script").and_then(|v| v.as_str()).unwrap_or("");
    let args = body.get("args").cloned().unwrap_or(json!([]));
    let result = plugin_post(
        session,
        "/script/execute",
        json!({"script": script, "args": args}),
    )
    .await
    .map_err(|e| W3cError::javascript_error(e.message))?;
    Ok(w3c_value(
        result.get("value").cloned().unwrap_or(Value::Null),
    ))
}

async fn execute_async(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let script = body.get("script").and_then(|v| v.as_str()).unwrap_or("");
    let args = body.get("args").cloned().unwrap_or(json!([]));
    let result = plugin_post(
        session,
        "/script/execute-async",
        json!({"script": script, "args": args}),
    )
    .await
    .map_err(|e| W3cError::javascript_error(e.message))?;
    Ok(w3c_value(
        result.get("value").cloned().unwrap_or(Value::Null),
    ))
}

// --- Cookie handlers ---

async fn get_all_cookies(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/cookie/get-all", json!({})).await?;
    Ok(w3c_value(
        result.get("cookies").cloned().unwrap_or(json!([])),
    ))
}

async fn get_named_cookie(
    AxumState(state): AxumState<SharedState>,
    Path((sid, name)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/cookie/get", json!({"name": name})).await?;
    let cookie = result.get("cookie").cloned().unwrap_or(Value::Null);
    if cookie.is_null() {
        return Err(W3cError::new(
            StatusCode::NOT_FOUND,
            "no such cookie",
            format!("Cookie '{name}' not found"),
        ));
    }
    Ok(w3c_value(cookie))
}

async fn add_cookie(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let cookie = body.get("cookie").cloned().unwrap_or(json!({}));
    plugin_post(session, "/cookie/add", json!({"cookie": cookie})).await?;
    Ok(w3c_value(json!(null)))
}

async fn delete_cookie(
    AxumState(state): AxumState<SharedState>,
    Path((sid, name)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/cookie/delete", json!({"name": name})).await?;
    Ok(w3c_value(json!(null)))
}

async fn delete_all_cookies(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/cookie/delete-all", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

// --- Action handlers ---

async fn perform_actions(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;

    // Walk through the actions and resolve any W3C element references in
    // pointer action origins before forwarding to the plugin.
    let mut resolved_body = body.clone();
    if let Some(actions) = resolved_body
        .get_mut("actions")
        .and_then(|a| a.as_array_mut())
    {
        for seq in actions.iter_mut() {
            if let Some(sub_actions) = seq.get_mut("actions").and_then(|a| a.as_array_mut()) {
                for action in sub_actions.iter_mut() {
                    // Check if origin is a W3C element reference object.
                    if let Some(origin) = action.get("origin").cloned() {
                        if let Some(eid) = origin.get(W3C_ELEMENT_KEY).and_then(|v| v.as_str()) {
                            if let Some(elem_ref) = session.elements.get(eid) {
                                // Replace element UUID with selector/index for the plugin.
                                action["origin"] = json!({
                                    W3C_ELEMENT_KEY: {
                                        "selector": elem_ref.selector,
                                        "index": elem_ref.index,
                                        "using": elem_ref.using
                                    }
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    plugin_post(session, "/actions/perform", resolved_body).await?;
    Ok(w3c_value(json!(null)))
}

async fn release_actions(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/actions/release", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

// --- Alert/Dialog handlers ---

async fn dismiss_alert(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/alert/dismiss", json!({}))
        .await
        .map_err(|e| {
            if e.message.contains("no such alert") {
                W3cError::new(StatusCode::NOT_FOUND, "no such alert", &e.message)
            } else {
                e
            }
        })?;
    Ok(w3c_value(json!(null)))
}

async fn accept_alert(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/alert/accept", json!({}))
        .await
        .map_err(|e| {
            if e.message.contains("no such alert") {
                W3cError::new(StatusCode::NOT_FOUND, "no such alert", &e.message)
            } else {
                e
            }
        })?;
    Ok(w3c_value(json!(null)))
}

async fn get_alert_text(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/alert/text", json!({}))
        .await
        .map_err(|e| {
            if e.message.contains("no such alert") {
                W3cError::new(StatusCode::NOT_FOUND, "no such alert", &e.message)
            } else {
                e
            }
        })?;
    Ok(w3c_value(result.get("text").cloned().unwrap_or(json!(""))))
}

async fn send_alert_text(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    plugin_post(session, "/alert/send-text", json!({"text": text}))
        .await
        .map_err(|e| {
            if e.message.contains("no such alert") {
                W3cError::new(StatusCode::NOT_FOUND, "no such alert", &e.message)
            } else {
                e
            }
        })?;
    Ok(w3c_value(json!(null)))
}

// --- Screenshot handlers ---

async fn take_screenshot(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/screenshot", json!({})).await?;
    Ok(w3c_value(result.get("data").cloned().unwrap_or(json!(""))))
}

async fn element_screenshot(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/screenshot/element",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(result.get("data").cloned().unwrap_or(json!(""))))
}

// --- Print handler ---

async fn print_page(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/print", body).await?;
    Ok(w3c_value(result.get("data").cloned().unwrap_or(json!(""))))
}

// --- Shadow DOM handlers ---

async fn get_shadow_root(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let mut guard = state.sessions.lock().await;
    let session = get_session_mut(&mut guard, &sid)?;
    let elem = session
        .elements
        .get(&eid)
        .ok_or_else(|| W3cError::no_element(&eid))?;
    let host_selector = elem.selector.clone();
    let host_index = elem.index;
    let host_using = elem.using.clone();
    let result = plugin_post(
        session,
        "/element/shadow",
        json!({"selector": host_selector, "index": host_index, "using": host_using}),
    )
    .await?;
    let has_shadow = result
        .get("hasShadow")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !has_shadow {
        return Err(W3cError::new(
            StatusCode::NOT_FOUND,
            "no such shadow root",
            format!("Element {eid} does not have a shadow root"),
        ));
    }
    let shadow_id = uuid::Uuid::new_v4().to_string();
    session.shadows.insert(
        shadow_id.clone(),
        ShadowRef {
            host_selector,
            host_index,
            host_using,
        },
    );
    Ok(w3c_value(json!({W3C_SHADOW_KEY: shadow_id})))
}

async fn find_in_shadow(
    AxumState(state): AxumState<SharedState>,
    Path((sid, shadow_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> W3cResult {
    let mut guard = state.sessions.lock().await;
    let session = get_session_mut(&mut guard, &sid)?;
    let shadow = session.shadows.get(&shadow_id).ok_or_else(|| {
        W3cError::new(
            StatusCode::NOT_FOUND,
            "no such shadow root",
            format!("Shadow root {shadow_id} not found"),
        )
    })?;
    let host_selector = shadow.host_selector.clone();
    let host_index = shadow.host_index;
    let host_using = shadow.host_using.clone();
    let (using, value) = extract_locator(&body)?;
    let result = plugin_post(
        session,
        "/shadow/find",
        json!({
            "host_selector": host_selector,
            "host_index": host_index,
            "host_using": host_using,
            "using": using,
            "value": value
        }),
    )
    .await?;

    let elements = result
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| {
            W3cError::new(
                StatusCode::NOT_FOUND,
                "no such element",
                format!("No element found in shadow with {using}: {value}"),
            )
        })?;

    if elements.is_empty() {
        return Err(W3cError::new(
            StatusCode::NOT_FOUND,
            "no such element",
            format!("No element found in shadow with {using}: {value}"),
        ));
    }

    let child_eid = store_element(session, &elements[0]);
    Ok(w3c_value(json!({W3C_ELEMENT_KEY: child_eid})))
}

async fn find_all_in_shadow(
    AxumState(state): AxumState<SharedState>,
    Path((sid, shadow_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> W3cResult {
    let mut guard = state.sessions.lock().await;
    let session = get_session_mut(&mut guard, &sid)?;
    let shadow = session.shadows.get(&shadow_id).ok_or_else(|| {
        W3cError::new(
            StatusCode::NOT_FOUND,
            "no such shadow root",
            format!("Shadow root {shadow_id} not found"),
        )
    })?;
    let host_selector = shadow.host_selector.clone();
    let host_index = shadow.host_index;
    let host_using = shadow.host_using.clone();
    let (using, value) = extract_locator(&body)?;
    let result = plugin_post(
        session,
        "/shadow/find",
        json!({
            "host_selector": host_selector,
            "host_index": host_index,
            "host_using": host_using,
            "using": using,
            "value": value
        }),
    )
    .await?;

    let empty = vec![];
    let elements = result
        .get("elements")
        .and_then(|e| e.as_array())
        .unwrap_or(&empty);

    let mapped: Vec<Value> = elements
        .iter()
        .map(|elem| {
            let child_eid = store_element(session, elem);
            json!({W3C_ELEMENT_KEY: child_eid})
        })
        .collect();

    Ok(w3c_value(json!(mapped)))
}

// --- Frame handlers ---

async fn switch_to_frame(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;

    let frame_id = body.get("id").cloned().unwrap_or(Value::Null);

    if frame_id.is_null() {
        // Switch to top-level
        plugin_post(session, "/frame/switch", json!({"id": null})).await?;
        return Ok(w3c_value(json!(null)));
    }

    if let Some(idx) = frame_id.as_u64() {
        // Switch by index
        plugin_post(session, "/frame/switch", json!({"id": idx})).await?;
        return Ok(w3c_value(json!(null)));
    }

    // Switch by element reference
    if let Some(eid) = frame_id.get(W3C_ELEMENT_KEY).and_then(|v| v.as_str()) {
        let elem = resolve_element(session, eid)?;
        plugin_post(
            session,
            "/frame/switch",
            json!({"id": {"selector": elem.selector, "index": elem.index}}),
        )
        .await?;
        return Ok(w3c_value(json!(null)));
    }

    Err(W3cError::bad_request("Invalid frame id"))
}

async fn switch_to_parent_frame(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    plugin_post(session, "/frame/parent", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

// --- Switch To Window handler ---

async fn switch_to_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let handle = body
        .get("handle")
        .and_then(|v| v.as_str())
        .ok_or_else(|| W3cError::bad_request("Missing 'handle'"))?;
    plugin_post(session, "/window/set-current", json!({"label": handle}))
        .await
        .map_err(|_| {
            W3cError::new(
                StatusCode::NOT_FOUND,
                "no such window",
                format!("Window '{handle}' not found"),
            )
        })?;
    Ok(w3c_value(json!(null)))
}

// --- Find element from element (scoped search) handlers ---

async fn find_element_from_element(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> W3cResult {
    let mut guard = state.sessions.lock().await;
    let session = get_session_mut(&mut guard, &sid)?;
    let parent = session
        .elements
        .get(&eid)
        .ok_or_else(|| W3cError::no_element(&eid))?;
    let parent_selector = parent.selector.clone();
    let parent_index = parent.index;
    let parent_using = parent.using.clone();
    let (using, value) = extract_locator(&body)?;
    let result = plugin_post(
        session,
        "/element/find-from",
        json!({
            "parent_selector": parent_selector,
            "parent_index": parent_index,
            "parent_using": parent_using,
            "using": using,
            "value": value
        }),
    )
    .await?;

    let elements = result
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| {
            W3cError::new(
                StatusCode::NOT_FOUND,
                "no such element",
                format!("No child element found with {using}: {value}"),
            )
        })?;

    if elements.is_empty() {
        return Err(W3cError::new(
            StatusCode::NOT_FOUND,
            "no such element",
            format!("No child element found with {using}: {value}"),
        ));
    }

    let child_eid = store_element(session, &elements[0]);
    Ok(w3c_value(json!({W3C_ELEMENT_KEY: child_eid})))
}

async fn find_elements_from_element(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> W3cResult {
    let mut guard = state.sessions.lock().await;
    let session = get_session_mut(&mut guard, &sid)?;
    let parent = session
        .elements
        .get(&eid)
        .ok_or_else(|| W3cError::no_element(&eid))?;
    let parent_selector = parent.selector.clone();
    let parent_index = parent.index;
    let parent_using = parent.using.clone();
    let (using, value) = extract_locator(&body)?;
    let result = plugin_post(
        session,
        "/element/find-from",
        json!({
            "parent_selector": parent_selector,
            "parent_index": parent_index,
            "parent_using": parent_using,
            "using": using,
            "value": value
        }),
    )
    .await?;

    let empty = vec![];
    let elements = result
        .get("elements")
        .and_then(|e| e.as_array())
        .unwrap_or(&empty);

    let mapped: Vec<Value> = elements
        .iter()
        .map(|elem| {
            let child_eid = store_element(session, elem);
            json!({W3C_ELEMENT_KEY: child_eid})
        })
        .collect();

    Ok(w3c_value(json!(mapped)))
}

// --- Computed ARIA role + label handlers ---

async fn get_computed_role(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/computed-role",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(
        result.get("role").cloned().unwrap_or(json!("generic")),
    ))
}

async fn get_computed_label(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/computed-label",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(result.get("label").cloned().unwrap_or(json!(""))))
}

// --- Active element handler ---

async fn get_active_element(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let mut guard = state.sessions.lock().await;
    let session = get_session_mut(&mut guard, &sid)?;
    let result = plugin_post(session, "/element/active", json!({})).await?;
    let elem = result.get("element").cloned().unwrap_or(Value::Null);
    if elem.is_null() {
        return Err(W3cError::new(
            StatusCode::NOT_FOUND,
            "no such element",
            "No element is focused",
        ));
    }
    let eid = store_element(session, &elem);
    Ok(w3c_value(json!({W3C_ELEMENT_KEY: eid})))
}

// --- Page source handler ---

async fn get_page_source(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.sessions.lock().await;
    let session = get_session(&guard, &sid)?;
    let result = plugin_post(session, "/source", json!({})).await?;
    Ok(w3c_value(
        result.get("source").cloned().unwrap_or(json!("")),
    ))
}

// --- Main ---

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cli.log_level)),
        )
        .init();

    let state: SharedState = Arc::new(AppState {
        sessions: Mutex::new(HashMap::new()),
        max_sessions: cli.max_sessions,
    });

    let router = Router::new()
        // Session
        .route("/status", get(get_status))
        .route("/session", post(create_session))
        .route("/session/{sid}", delete(delete_session))
        // Timeouts
        .route("/session/{sid}/timeouts", get(get_timeouts))
        .route("/session/{sid}/timeouts", post(set_timeouts))
        // Navigation
        .route("/session/{sid}/url", post(navigate_to))
        .route("/session/{sid}/url", get(get_url))
        .route("/session/{sid}/title", get(get_title))
        .route("/session/{sid}/source", get(get_page_source))
        .route("/session/{sid}/back", post(go_back))
        .route("/session/{sid}/forward", post(go_forward))
        .route("/session/{sid}/refresh", post(refresh))
        // Window
        .route("/session/{sid}/window", get(get_window_handle))
        .route("/session/{sid}/window", post(switch_to_window))
        .route("/session/{sid}/window", delete(close_window))
        .route("/session/{sid}/window/handles", get(get_window_handles))
        .route("/session/{sid}/window/rect", get(get_window_rect))
        .route("/session/{sid}/window/rect", post(set_window_rect))
        .route("/session/{sid}/window/maximize", post(maximize_window))
        .route("/session/{sid}/window/minimize", post(minimize_window))
        .route("/session/{sid}/window/fullscreen", post(fullscreen_window))
        .route("/session/{sid}/window/new", post(new_window))
        // Frames
        .route("/session/{sid}/frame", post(switch_to_frame))
        .route("/session/{sid}/frame/parent", post(switch_to_parent_frame))
        // Elements
        .route("/session/{sid}/element", post(find_element))
        .route("/session/{sid}/elements", post(find_elements))
        .route("/session/{sid}/element/active", get(get_active_element))
        .route(
            "/session/{sid}/element/{eid}/element",
            post(find_element_from_element),
        )
        .route(
            "/session/{sid}/element/{eid}/elements",
            post(find_elements_from_element),
        )
        .route("/session/{sid}/element/{eid}/click", post(click_element))
        .route("/session/{sid}/element/{eid}/clear", post(clear_element))
        .route("/session/{sid}/element/{eid}/value", post(send_keys))
        .route("/session/{sid}/element/{eid}/text", get(get_element_text))
        .route("/session/{sid}/element/{eid}/name", get(get_element_tag))
        .route(
            "/session/{sid}/element/{eid}/attribute/{name}",
            get(get_element_attribute),
        )
        .route(
            "/session/{sid}/element/{eid}/property/{name}",
            get(get_element_property),
        )
        .route(
            "/session/{sid}/element/{eid}/css/{name}",
            get(get_element_css),
        )
        .route("/session/{sid}/element/{eid}/rect", get(get_element_rect))
        .route(
            "/session/{sid}/element/{eid}/enabled",
            get(is_element_enabled),
        )
        .route(
            "/session/{sid}/element/{eid}/selected",
            get(is_element_selected),
        )
        .route(
            "/session/{sid}/element/{eid}/displayed",
            get(is_element_displayed),
        )
        .route(
            "/session/{sid}/element/{eid}/computedrole",
            get(get_computed_role),
        )
        .route(
            "/session/{sid}/element/{eid}/computedlabel",
            get(get_computed_label),
        )
        .route("/session/{sid}/element/{eid}/shadow", get(get_shadow_root))
        .route("/session/{sid}/shadow/{sid2}/element", post(find_in_shadow))
        .route(
            "/session/{sid}/shadow/{sid2}/elements",
            post(find_all_in_shadow),
        )
        // Scripts
        .route("/session/{sid}/execute/sync", post(execute_sync))
        .route("/session/{sid}/execute/async", post(execute_async))
        // Cookies
        .route("/session/{sid}/cookie", get(get_all_cookies))
        .route("/session/{sid}/cookie", post(add_cookie))
        .route("/session/{sid}/cookie", delete(delete_all_cookies))
        .route("/session/{sid}/cookie/{name}", get(get_named_cookie))
        .route("/session/{sid}/cookie/{name}", delete(delete_cookie))
        // Alerts
        .route("/session/{sid}/alert/dismiss", post(dismiss_alert))
        .route("/session/{sid}/alert/accept", post(accept_alert))
        .route("/session/{sid}/alert/text", get(get_alert_text))
        .route("/session/{sid}/alert/text", post(send_alert_text))
        // Actions
        .route("/session/{sid}/actions", post(perform_actions))
        .route("/session/{sid}/actions", delete(release_actions))
        // Print
        .route("/session/{sid}/print", post(print_page))
        // Screenshots
        .route("/session/{sid}/screenshot", get(take_screenshot))
        .route(
            "/session/{sid}/element/{eid}/screenshot",
            get(element_screenshot),
        )
        .with_state(state.clone());

    let shutdown_state = state;

    let addr = format!("{}:{}", cli.host, cli.port);
    tracing::info!("tauri-wd listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind WebDriver server");
    let shutdown = async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to create SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => { tracing::info!("Received SIGINT, shutting down"); }
                _ = sigterm.recv() => { tracing::info!("Received SIGTERM, shutting down"); }
            }
        }
        #[cfg(not(unix))]
        {
            ctrl_c.await.ok();
            tracing::info!("Received SIGINT, shutting down");
        }

        // Kill all active sessions' app processes
        let mut sessions = shutdown_state.sessions.lock().await;
        for (sid, session) in sessions.iter_mut() {
            let _ = session.process.kill().await;
            tracing::info!("Killed app process for session {sid} on shutdown");
        }
        sessions.clear();
    };

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .expect("WebDriver server error");
}
