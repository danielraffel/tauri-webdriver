// tauri-webdriver: W3C WebDriver server for Tauri apps on macOS.
//
// Launches the Tauri app, discovers the plugin's HTTP port from stdout,
// and translates W3C WebDriver commands into plugin API calls.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

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

// --- CLI arguments ---

#[derive(Parser)]
#[command(name = "tauri-webdriver", about = "W3C WebDriver server for Tauri apps")]
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
}

// --- State types ---

struct ElementRef {
    selector: String,
    index: usize,
    using: String,
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
    id: String,
    plugin_url: String,
    process: tokio::process::Child,
    elements: HashMap<String, ElementRef>,
    client: reqwest::Client,
    timeouts: Timeouts,
}

struct AppState {
    session: Mutex<Option<Session>>,
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
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "session not created", msg)
    }
    fn unknown(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "unknown error", msg)
    }
    fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid argument", msg)
    }
    fn javascript_error(msg: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "javascript error",
            msg,
        )
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
    session.elements.get(eid).ok_or_else(|| W3cError::no_element(eid))
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
        "partial link text" => (
            "xpath".to_string(),
            format!("//a[contains(.,'{}')]", value),
        ),
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

fn check_session<'a>(session: &'a Option<Session>, sid: &str) -> Result<&'a Session, W3cError> {
    let s = session.as_ref().ok_or(W3cError::no_session())?;
    if s.id != sid {
        return Err(W3cError::no_session());
    }
    Ok(s)
}

fn check_session_mut<'a>(
    session: &'a mut Option<Session>,
    sid: &str,
) -> Result<&'a mut Session, W3cError> {
    let s = session.as_mut().ok_or(W3cError::no_session())?;
    if s.id != sid {
        return Err(W3cError::no_session());
    }
    Ok(s)
}

// --- Session handlers ---

async fn get_status(AxumState(state): AxumState<SharedState>) -> Json<Value> {
    let session = state.session.lock().await;
    w3c_value(json!({
        "ready": session.is_none(),
        "message": if session.is_none() { "ready" } else { "session active" }
    }))
}

async fn create_session(
    AxumState(state): AxumState<SharedState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), W3cError> {
    let mut session_guard = state.session.lock().await;
    if session_guard.is_some() {
        return Err(W3cError::session_not_created("A session already exists"));
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

    *session_guard = Some(Session {
        id: session_id.clone(),
        plugin_url,
        process: child,
        elements: HashMap::new(),
        client: reqwest::Client::new(),
        timeouts: Timeouts::default(),
    });

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
    let mut guard = state.session.lock().await;
    let session = guard.as_mut().ok_or(W3cError::no_session())?;
    if session.id != sid {
        return Err(W3cError::no_session());
    }
    let _ = session.process.kill().await;
    *guard = None;
    tracing::info!("Session {sid} deleted");
    Ok(w3c_value(json!(null)))
}

// --- Timeouts handlers ---

async fn get_timeouts(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let mut guard = state.session.lock().await;
    let session = check_session_mut(&mut guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let url = body
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| W3cError::bad_request("Missing url"))?;
    plugin_post(session, "/navigate/url", json!({"url": url})).await?;
    Ok(w3c_value(json!(null)))
}

async fn get_url(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let result = plugin_post(session, "/navigate/current", json!({})).await?;
    Ok(w3c_value(
        result.get("url").cloned().unwrap_or(json!("")),
    ))
}

async fn get_title(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let result = plugin_post(session, "/navigate/title", json!({})).await?;
    Ok(w3c_value(
        result.get("title").cloned().unwrap_or(json!("")),
    ))
}

async fn go_back(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/navigate/back", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

async fn go_forward(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/navigate/forward", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

async fn refresh(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/navigate/refresh", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

// --- Window handlers ---

async fn get_window_handle(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let result = plugin_post(session, "/window/handle", json!({})).await?;
    Ok(w3c_value(result))
}

async fn close_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let result = plugin_post(session, "/window/handles", json!({})).await?;
    Ok(w3c_value(result))
}

async fn get_window_rect(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

async fn set_window_rect(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/window/set-rect", body).await?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

async fn maximize_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/window/maximize", json!({})).await?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

async fn minimize_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/window/minimize", json!({})).await?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

async fn fullscreen_window(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/window/fullscreen", json!({})).await?;
    let result = plugin_post(session, "/window/rect", json!({})).await?;
    Ok(w3c_value(result))
}

// --- Element handlers ---

async fn find_element(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let mut guard = state.session.lock().await;
    let session = check_session_mut(&mut guard, &sid)?;
    let (using, value) = extract_locator(&body)?;
    let result =
        plugin_post(session, "/element/find", json!({"using": using, "value": value})).await?;

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
    let mut guard = state.session.lock().await;
    let session = check_session_mut(&mut guard, &sid)?;
    let (using, value) = extract_locator(&body)?;
    let result =
        plugin_post(session, "/element/find", json!({"using": using, "value": value})).await?;

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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    plugin_post(
        session,
        "/element/send-keys",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using, "text": text}),
    )
    .await?;
    Ok(w3c_value(json!(null)))
}

async fn get_element_text(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/text",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(
        result.get("text").cloned().unwrap_or(json!("")),
    ))
}

async fn get_element_tag(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/element/tag",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(
        result.get("tag").cloned().unwrap_or(json!("")),
    ))
}

async fn get_element_attribute(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid, name)): Path<(String, String, String)>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let script = body
        .get("script")
        .and_then(|v| v.as_str())
        .unwrap_or("");
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let script = body
        .get("script")
        .and_then(|v| v.as_str())
        .unwrap_or("");
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let result = plugin_post(session, "/cookie/get-all", json!({})).await?;
    Ok(w3c_value(
        result.get("cookies").cloned().unwrap_or(json!([])),
    ))
}

async fn get_named_cookie(
    AxumState(state): AxumState<SharedState>,
    Path((sid, name)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let cookie = body.get("cookie").cloned().unwrap_or(json!({}));
    plugin_post(session, "/cookie/add", json!({"cookie": cookie})).await?;
    Ok(w3c_value(json!(null)))
}

async fn delete_cookie(
    AxumState(state): AxumState<SharedState>,
    Path((sid, name)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/cookie/delete", json!({"name": name})).await?;
    Ok(w3c_value(json!(null)))
}

async fn delete_all_cookies(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/cookie/delete-all", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

// --- Action handlers ---

async fn perform_actions(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;

    // Walk through the actions and resolve any W3C element references in
    // pointer action origins before forwarding to the plugin.
    let mut resolved_body = body.clone();
    if let Some(actions) = resolved_body
        .get_mut("actions")
        .and_then(|a| a.as_array_mut())
    {
        for seq in actions.iter_mut() {
            if let Some(sub_actions) =
                seq.get_mut("actions").and_then(|a| a.as_array_mut())
            {
                for action in sub_actions.iter_mut() {
                    // Check if origin is a W3C element reference object.
                    if let Some(origin) = action.get("origin").cloned() {
                        if let Some(eid) =
                            origin.get(W3C_ELEMENT_KEY).and_then(|v| v.as_str())
                        {
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
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    plugin_post(session, "/actions/release", json!({})).await?;
    Ok(w3c_value(json!(null)))
}

// --- Screenshot handlers ---

async fn take_screenshot(
    AxumState(state): AxumState<SharedState>,
    Path(sid): Path<String>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let result = plugin_post(session, "/screenshot", json!({})).await?;
    Ok(w3c_value(
        result.get("data").cloned().unwrap_or(json!("")),
    ))
}

async fn element_screenshot(
    AxumState(state): AxumState<SharedState>,
    Path((sid, eid)): Path<(String, String)>,
) -> W3cResult {
    let guard = state.session.lock().await;
    let session = check_session(&guard, &sid)?;
    let elem = resolve_element(session, &eid)?;
    let result = plugin_post(
        session,
        "/screenshot/element",
        json!({"selector": elem.selector, "index": elem.index, "using": elem.using}),
    )
    .await?;
    Ok(w3c_value(
        result.get("data").cloned().unwrap_or(json!("")),
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
        session: Mutex::new(None),
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
        .route("/session/{sid}/back", post(go_back))
        .route("/session/{sid}/forward", post(go_forward))
        .route("/session/{sid}/refresh", post(refresh))
        // Window
        .route("/session/{sid}/window", get(get_window_handle))
        .route("/session/{sid}/window", delete(close_window))
        .route("/session/{sid}/window/handles", get(get_window_handles))
        .route("/session/{sid}/window/rect", get(get_window_rect))
        .route("/session/{sid}/window/rect", post(set_window_rect))
        .route("/session/{sid}/window/maximize", post(maximize_window))
        .route("/session/{sid}/window/minimize", post(minimize_window))
        .route("/session/{sid}/window/fullscreen", post(fullscreen_window))
        // Elements
        .route("/session/{sid}/element", post(find_element))
        .route("/session/{sid}/elements", post(find_elements))
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
        .route(
            "/session/{sid}/element/{eid}/rect",
            get(get_element_rect),
        )
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
        // Scripts
        .route("/session/{sid}/execute/sync", post(execute_sync))
        .route("/session/{sid}/execute/async", post(execute_async))
        // Cookies
        .route("/session/{sid}/cookie", get(get_all_cookies))
        .route("/session/{sid}/cookie", post(add_cookie))
        .route("/session/{sid}/cookie", delete(delete_all_cookies))
        .route("/session/{sid}/cookie/{name}", get(get_named_cookie))
        .route(
            "/session/{sid}/cookie/{name}",
            delete(delete_cookie),
        )
        // Actions
        .route("/session/{sid}/actions", post(perform_actions))
        .route("/session/{sid}/actions", delete(release_actions))
        // Screenshots
        .route("/session/{sid}/screenshot", get(take_screenshot))
        .route(
            "/session/{sid}/element/{eid}/screenshot",
            get(element_screenshot),
        )
        .with_state(state.clone());

    let shutdown_state = state;

    let addr = format!("{}:{}", cli.host, cli.port);
    tracing::info!("tauri-webdriver listening on {}", addr);

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

        // Kill any active session's app process
        let mut guard = shutdown_state.session.lock().await;
        if let Some(session) = guard.as_mut() {
            let _ = session.process.kill().await;
            tracing::info!("Killed app process on shutdown");
        }
        *guard = None;
    };

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .expect("WebDriver server error");
}
