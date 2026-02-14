// HTTP server for the tauri-plugin-webdriver-automation plugin.
// Binds to 127.0.0.1 on a random port and exposes endpoints for
// window management, element interaction, script execution, and navigation.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{Manager, Runtime};

use crate::{window_by_label, WebDriverState};

// --- Server state ---

struct FrameRef {
    selector: String,
    index: usize,
}

struct ServerState<R: Runtime> {
    app: tauri::AppHandle<R>,
    current_window_label: std::sync::Mutex<Option<String>>,
    frame_stack: std::sync::Mutex<Vec<FrameRef>>,
}

type SharedState<R> = Arc<ServerState<R>>;

/// Build a JS snippet that navigates into the current iframe stack.
/// Returns the JS code that sets `__doc` to the correct frame document,
/// or an empty string if we're at the top level.
fn build_frame_prefix<R: Runtime>(state: &SharedState<R>) -> String {
    let stack = state.frame_stack.lock().expect("lock poisoned");
    if stack.is_empty() {
        return String::new();
    }
    let mut js = "var __doc=document;".to_string();
    for fr in stack.iter() {
        let sel_json = serde_json::to_string(&fr.selector).unwrap();
        js.push_str(&format!(
            "var __f=__doc.querySelectorAll({sel_json})[{idx}];\
             if(!__f)throw new Error('frame not found');\
             __doc=__f.contentDocument;\
             if(!__doc)throw new Error('cannot access frame document');",
            sel_json = sel_json,
            idx = fr.index,
        ));
    }
    js
}

/// Returns true if the frame stack is non-empty.
fn in_frame<R: Runtime>(state: &SharedState<R>) -> bool {
    !state.frame_stack.lock().expect("lock poisoned").is_empty()
}

// --- Error handling ---

enum ApiError {
    NotFound(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(json!({"error": msg}))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

// --- JS evaluation helpers ---

async fn eval_js<R: Runtime>(
    state: &SharedState<R>,
    script: &str,
) -> Result<Value, ApiError> {
    let label = state
        .current_window_label
        .lock()
        .expect("lock poisoned")
        .clone();
    let window = window_by_label(&state.app, label.as_deref())
        .ok_or_else(|| ApiError::NotFound("no such window".into()))?;

    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let ws = state.app.state::<WebDriverState>();
        ws.pending_scripts
            .lock()
            .expect("lock poisoned")
            .insert(id.clone(), tx);
    }

    // Build frame prefix to navigate into current iframe context.
    let frame_prefix = build_frame_prefix(state);
    let is_framed = in_frame(state);

    // Wrap user script: execute it, send result back via IPC.
    // When inside a frame, pass the frame document as a `document` parameter
    // to the inner function, which shadows the global `document` without
    // hoisting issues that `var document=...` would cause.
    let wrapped = if is_framed {
        format!(
            concat!(
                "(function(){{try{{{frame_prefix}",
                "var __r=(function(document){{{script}}}).call(null,__doc);",
                "window.__WEBDRIVER__.resolve(\"{id}\",__r)",
                "}}catch(__e){{window.__WEBDRIVER__.resolve(\"{id}\",",
                "{{error:__e.name,message:__e.message,stacktrace:__e.stack||\"\"}})",
                "}}}})()"
            ),
            frame_prefix = frame_prefix,
            script = script,
            id = id,
        )
    } else {
        format!(
            concat!(
                "(function(){{try{{var __r=(function(){{{script}}})();",
                "window.__WEBDRIVER__.resolve(\"{id}\",__r)",
                "}}catch(__e){{window.__WEBDRIVER__.resolve(\"{id}\",",
                "{{error:__e.name,message:__e.message,stacktrace:__e.stack||\"\"}})",
                "}}}})()"
            ),
            script = script,
            id = id,
        )
    };

    window
        .eval(&wrapped)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    match tokio::time::timeout(Duration::from_secs(30), rx).await {
        Ok(Ok(value)) => {
            // If the JS threw, it comes back as {error, message, stacktrace}.
            if let Some(obj) = value.as_object() {
                if obj.contains_key("error") && obj.contains_key("message") {
                    let msg = obj
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("script error");
                    return Err(ApiError::Internal(msg.to_string()));
                }
            }
            Ok(value)
        }
        Ok(Err(_)) => Err(ApiError::Internal("result channel closed".into())),
        Err(_) => {
            let ws = state.app.state::<WebDriverState>();
            ws.pending_scripts
                .lock()
                .expect("lock poisoned")
                .remove(&id);
            Err(ApiError::Internal("script timed out".into()))
        }
    }
}

/// Evaluate JS that operates on a located element.
async fn eval_on_element<R: Runtime>(
    state: &SharedState<R>,
    selector: &str,
    index: usize,
    using: Option<&str>,
    body: &str,
) -> Result<Value, ApiError> {
    let script = if using == Some("shadow") {
        // Shadow DOM element: look up from the shadow cache by ID
        let sel_json = serde_json::to_string(selector).unwrap();
        format!(
            "var el=window.__WEBDRIVER__.findElementInShadow({sel_json});\
             if(!el)throw new Error(\"shadow element not found or stale\");\
             {body}"
        )
    } else {
        let sel_json = serde_json::to_string(selector).unwrap();
        // When inside a frame context, eval_js passes the frame document as
        // the `document` parameter, so we use document.querySelectorAll directly
        // instead of the findElement helper (which uses the top-level document).
        if using == Some("xpath") {
            format!(
                "var __xr=document.evaluate({sel_json},document,null,\
                 XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null);\
                 var el=__xr.snapshotItem({index});\
                 if(!el)throw new Error(\"element not found\");\
                 {body}"
            )
        } else {
            format!(
                "var el=document.querySelectorAll({sel_json})[{index}];\
                 if(!el)throw new Error(\"element not found\");\
                 {body}"
            )
        }
    };
    eval_js(state, &script).await
}

// --- Request body types ---

#[derive(Deserialize)]
struct LabelReq {
    label: Option<String>,
}

#[derive(Deserialize)]
struct CloseReq {
    label: String,
}

#[derive(Deserialize)]
struct SetRectReq {
    label: Option<String>,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
}

#[derive(Deserialize)]
struct FindReq {
    using: String,
    value: String,
}

#[derive(Deserialize)]
struct ElemReq {
    selector: String,
    index: usize,
    #[serde(default)]
    using: Option<String>,
}

#[derive(Deserialize)]
struct ElemAttrReq {
    selector: String,
    index: usize,
    name: String,
    #[serde(default)]
    using: Option<String>,
}

#[derive(Deserialize)]
struct SendKeysReq {
    selector: String,
    index: usize,
    text: String,
    #[serde(default)]
    using: Option<String>,
}

#[derive(Deserialize)]
struct FileInfo {
    name: String,
    data: String, // base64-encoded file content
    #[serde(default = "default_mime")]
    mime: String,
}

fn default_mime() -> String {
    "application/octet-stream".to_string()
}

#[derive(Deserialize)]
struct SetFilesReq {
    selector: String,
    index: usize,
    files: Vec<FileInfo>,
    #[serde(default)]
    using: Option<String>,
}

#[derive(Deserialize)]
struct ScriptReq {
    script: String,
    #[serde(default)]
    args: Vec<Value>,
}

#[derive(Deserialize)]
struct NavReq {
    url: String,
}

#[derive(Deserialize)]
struct CookieNameReq {
    name: String,
}

#[derive(Deserialize)]
struct CookieAddReq {
    cookie: CookieData,
}

#[derive(Deserialize)]
struct CookieData {
    name: String,
    value: String,
    #[serde(default = "default_path")]
    path: String,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    secure: bool,
    #[serde(rename = "httpOnly", default)]
    http_only: bool,
    #[serde(default)]
    expiry: Option<u64>,
}

fn default_path() -> String {
    "/".to_string()
}

// --- Window handlers ---

async fn window_handle<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let label = state
        .current_window_label
        .lock()
        .expect("lock poisoned")
        .clone();
    let window = window_by_label(&state.app, label.as_deref())
        .ok_or(ApiError::NotFound("no window".into()))?;
    Ok(Json(json!(window.label())))
}

async fn window_handles<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let labels: Vec<String> = state.app.webview_windows().keys().cloned().collect();
    Ok(Json(json!(labels)))
}

async fn window_close<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<CloseReq>,
) -> ApiResult {
    let window = state
        .app
        .get_webview_window(&body.label)
        .ok_or_else(|| ApiError::NotFound(format!("window '{}' not found", body.label)))?;
    window
        .close()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    // Clear current_window_label if it matches the closed window
    let mut label = state.current_window_label.lock().expect("lock poisoned");
    if label.as_deref() == Some(&body.label) {
        *label = None;
    }
    // Reset frame stack since we may have been in a frame of the closed window
    state.frame_stack.lock().expect("lock poisoned").clear();
    Ok(Json(json!(true)))
}

async fn window_rect<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<LabelReq>,
) -> ApiResult {
    let window = window_by_label(&state.app, body.label.as_deref())
        .ok_or(ApiError::NotFound("no window".into()))?;

    let scale = window
        .scale_factor()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let pos = window
        .outer_position()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let size = window
        .outer_size()
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(json!({
        "x": pos.x as f64 / scale,
        "y": pos.y as f64 / scale,
        "width": size.width as f64 / scale,
        "height": size.height as f64 / scale,
    })))
}

async fn window_set_rect<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<SetRectReq>,
) -> ApiResult {
    let window = window_by_label(&state.app, body.label.as_deref())
        .ok_or(ApiError::NotFound("no window".into()))?;

    if let (Some(x), Some(y)) = (body.x, body.y) {
        window
            .set_position(tauri::LogicalPosition::new(x, y))
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    if let (Some(w), Some(h)) = (body.width, body.height) {
        window
            .set_size(tauri::LogicalSize::new(w, h))
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    Ok(Json(json!(true)))
}

async fn window_fullscreen<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<LabelReq>,
) -> ApiResult {
    let window = window_by_label(&state.app, body.label.as_deref())
        .ok_or(ApiError::NotFound("no window".into()))?;
    window
        .set_fullscreen(true)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!(true)))
}

async fn window_minimize<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<LabelReq>,
) -> ApiResult {
    let window = window_by_label(&state.app, body.label.as_deref())
        .ok_or(ApiError::NotFound("no window".into()))?;
    window
        .minimize()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!(true)))
}

async fn window_maximize<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<LabelReq>,
) -> ApiResult {
    let window = window_by_label(&state.app, body.label.as_deref())
        .ok_or(ApiError::NotFound("no window".into()))?;
    window
        .maximize()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!(true)))
}

async fn window_insets<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<LabelReq>,
) -> ApiResult {
    let window = window_by_label(&state.app, body.label.as_deref())
        .ok_or(ApiError::NotFound("no window".into()))?;

    let scale = window
        .scale_factor()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let outer_pos = window
        .outer_position()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let inner_pos = window
        .inner_position()
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let top = (inner_pos.y - outer_pos.y) as f64 / scale;
    let left = (inner_pos.x - outer_pos.x) as f64 / scale;

    Ok(Json(json!({
        "top": top,
        "bottom": 0.0,
        "x": left,
        "y": top,
    })))
}

// --- New window handler ---

#[derive(Deserialize)]
struct WindowNewReq {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    type_hint: Option<String>,
}

async fn window_new<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<WindowNewReq>,
) -> ApiResult {
    let label = format!("wd-{}", uuid::Uuid::new_v4());

    let window = tauri::WebviewWindowBuilder::new(
        &state.app,
        &label,
        tauri::WebviewUrl::default(),
    )
    .inner_size(800.0, 600.0)
    .build()
    .map_err(|e| ApiError::Internal(format!("failed to create window: {e}")))?;

    // Wait briefly for the window to initialize
    tokio::time::sleep(Duration::from_millis(200)).await;

    let _ = window.set_focus();

    Ok(Json(json!({"handle": label, "type": "window"})))
}

// --- Element handlers ---

async fn element_find<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<FindReq>,
) -> ApiResult {
    let val_json = serde_json::to_string(&body.value).unwrap();

    let script = if body.using == "xpath" {
        format!(
            "var r=document.evaluate({v},document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null);\
             var a=[];for(var i=0;i<r.snapshotLength;i++)a.push({{selector:{v},index:i,using:\"xpath\"}});\
             return a",
            v = val_json,
        )
    } else {
        format!(
            "var els=document.querySelectorAll({v});\
             var a=[];for(var i=0;i<els.length;i++)a.push({{selector:{v},index:i}});\
             return a",
            v = val_json,
        )
    };

    let result = eval_js(&state, &script).await?;
    Ok(Json(json!({"elements": result})))
}

async fn element_text<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "return el.textContent||''",
    )
    .await?;
    Ok(Json(json!({"text": result})))
}

async fn element_attribute<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemAttrReq>,
) -> ApiResult {
    let name_json = serde_json::to_string(&body.name).unwrap();
    let js = format!("return el.getAttribute({name_json})");
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        &js,
    )
    .await?;
    Ok(Json(json!({"value": result})))
}

async fn element_property<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemAttrReq>,
) -> ApiResult {
    let name_json = serde_json::to_string(&body.name).unwrap();
    let js = format!("return el[{name_json}]");
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        &js,
    )
    .await?;
    Ok(Json(json!({"value": result})))
}

async fn element_tag<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "return el.tagName.toLowerCase()",
    )
    .await?;
    Ok(Json(json!({"tag": result})))
}

async fn element_rect<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "var r=el.getBoundingClientRect();return{x:r.x,y:r.y,width:r.width,height:r.height}",
    )
    .await?;
    Ok(Json(result))
}

async fn element_click<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "el.scrollIntoView({block:'center',inline:'center'});el.focus();el.click();return null",
    )
    .await?;
    Ok(Json(json!(null)))
}

async fn element_clear<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "el.focus();el.value='';el.dispatchEvent(new Event('input',{bubbles:true}));\
         el.dispatchEvent(new Event('change',{bubbles:true}));return null",
    )
    .await?;
    Ok(Json(json!(null)))
}

async fn element_send_keys<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<SendKeysReq>,
) -> ApiResult {
    let text_json = serde_json::to_string(&body.text).unwrap();
    let js = format!(
        "el.focus();el.value+={text_json};\
         el.dispatchEvent(new Event('input',{{bubbles:true}}));\
         el.dispatchEvent(new Event('change',{{bubbles:true}}));return null"
    );
    eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        &js,
    )
    .await?;
    Ok(Json(json!(null)))
}

async fn element_set_files<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<SetFilesReq>,
) -> ApiResult {
    // Build a JS array of {name, data, mime} objects to pass into the webview.
    let files_json = serde_json::to_string(&body.files.iter().map(|f| {
        json!({"name": f.name, "data": f.data, "mime": f.mime})
    }).collect::<Vec<_>>()).unwrap();

    let js = format!(
        "if(el.tagName!=='INPUT'||el.type!=='file')throw new Error('element is not a file input');\
         var _files={files_json};\
         var dt=new DataTransfer();\
         for(var i=0;i<_files.length;i++){{\
           var raw=atob(_files[i].data);\
           var bytes=new Uint8Array(raw.length);\
           for(var j=0;j<raw.length;j++)bytes[j]=raw.charCodeAt(j);\
           dt.items.add(new File([bytes],_files[i].name,{{type:_files[i].mime}}));\
         }}\
         el.files=dt.files;\
         el.dispatchEvent(new Event('input',{{bubbles:true}}));\
         el.dispatchEvent(new Event('change',{{bubbles:true}}));\
         return null"
    );
    eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        &js,
    )
    .await?;
    Ok(Json(json!(null)))
}

async fn element_displayed<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "var s=window.getComputedStyle(el);\
         return s.display!=='none'&&s.visibility!=='hidden'&&s.opacity!=='0'",
    )
    .await?;
    Ok(Json(json!({"displayed": result})))
}

async fn element_enabled<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "return !el.disabled",
    )
    .await?;
    Ok(Json(json!({"enabled": result})))
}

async fn element_selected<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "return el.selected||el.checked||false",
    )
    .await?;
    Ok(Json(json!({"selected": result})))
}

// --- Script handlers ---

async fn script_execute<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ScriptReq>,
) -> ApiResult {
    let args_json = serde_json::to_string(&body.args).unwrap();
    let script = format!(
        "var __args={args_json};return (function(){{{}}}).apply(null,__args)",
        body.script
    );
    let result = eval_js(&state, &script).await?;
    Ok(Json(json!({"value": result})))
}

async fn script_execute_async<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ScriptReq>,
) -> ApiResult {
    let label = state
        .current_window_label
        .lock()
        .expect("lock poisoned")
        .clone();
    let window = window_by_label(&state.app, label.as_deref())
        .ok_or(ApiError::NotFound("no window".into()))?;

    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let ws = state.app.state::<WebDriverState>();
        ws.pending_scripts
            .lock()
            .expect("lock poisoned")
            .insert(id.clone(), tx);
    }

    let args_json = serde_json::to_string(&body.args).unwrap();
    let script = format!(
        "(function(){{var __args={args_json};\
         var __done=function(r){{window.__WEBDRIVER__.resolve(\"{id}\",r)}};\
         __args.push(__done);\
         try{{(function(){{{user_script}}}).apply(null,__args)}}\
         catch(__e){{window.__WEBDRIVER__.resolve(\"{id}\",\
         {{error:__e.name,message:__e.message,stacktrace:__e.stack||\"\"}})}}}})();",
        user_script = body.script,
        id = id,
    );

    window
        .eval(&script)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    match tokio::time::timeout(Duration::from_secs(30), rx).await {
        Ok(Ok(value)) => {
            if let Some(obj) = value.as_object() {
                if obj.contains_key("error") && obj.contains_key("message") {
                    let msg = obj
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("script error");
                    return Err(ApiError::Internal(msg.to_string()));
                }
            }
            Ok(Json(json!({"value": value})))
        }
        Ok(Err(_)) => Err(ApiError::Internal("result channel closed".into())),
        Err(_) => {
            let ws = state.app.state::<WebDriverState>();
            ws.pending_scripts
                .lock()
                .expect("lock poisoned")
                .remove(&id);
            Err(ApiError::Internal("async script timed out".into()))
        }
    }
}

// --- Navigation handlers ---

async fn navigate_url<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<NavReq>,
) -> ApiResult {
    let url_json = serde_json::to_string(&body.url).unwrap();
    eval_js(
        &state,
        &format!("window.location.href={url_json};return null"),
    )
    .await?;
    Ok(Json(json!(null)))
}

async fn navigate_current<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    // Always return the top-level URL, even when inside a frame context.
    let result = eval_js(&state, "return window.location.href").await?;
    Ok(Json(json!({"url": result})))
}

async fn navigate_title<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    // Always return the top-level document title, even when inside a frame.
    // Use window.document (not shadowed by frame prefix) to access the real document.
    let result = eval_js(&state, "return window.document.title").await?;
    Ok(Json(json!({"title": result})))
}

async fn navigate_back<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    eval_js(&state, "window.history.back();return null").await?;
    Ok(Json(json!(null)))
}

async fn navigate_forward<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    eval_js(&state, "window.history.forward();return null").await?;
    Ok(Json(json!(null)))
}

async fn navigate_refresh<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    eval_js(&state, "window.location.reload();return null").await?;
    Ok(Json(json!(null)))
}

// --- Alert/Dialog handlers ---

async fn alert_get_text<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let result = eval_js(
        &state,
        "var d=window.__WEBDRIVER__.__dialog;\
         if(!d.open)throw new Error('no such alert');\
         return d.text",
    )
    .await?;
    Ok(Json(json!({"text": result})))
}

async fn alert_dismiss<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    eval_js(
        &state,
        "var d=window.__WEBDRIVER__.__dialog;\
         if(!d.open)throw new Error('no such alert');\
         if(d.type==='confirm')d.response=false;\
         if(d.type==='prompt')d.response=null;\
         d.open=false;\
         return null",
    )
    .await?;
    Ok(Json(json!(null)))
}

async fn alert_accept<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    eval_js(
        &state,
        "var d=window.__WEBDRIVER__.__dialog;\
         if(!d.open)throw new Error('no such alert');\
         if(d.type==='confirm')d.response=true;\
         if(d.type==='prompt'&&d.response===null)d.response=d.defaultValue||'';\
         d.open=false;\
         return null",
    )
    .await?;
    Ok(Json(json!(null)))
}

#[derive(Deserialize)]
struct AlertTextReq {
    text: String,
}

async fn alert_send_text<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<AlertTextReq>,
) -> ApiResult {
    let text_json = serde_json::to_string(&body.text).unwrap();
    let script = format!(
        "var d=window.__WEBDRIVER__.__dialog;\
         if(!d.open)throw new Error('no such alert');\
         if(d.type!=='prompt')throw new Error('no such alert');\
         d.response={text_json};\
         return null"
    );
    eval_js(&state, &script).await?;
    Ok(Json(json!(null)))
}

// --- Screenshot handlers ---

/// Helper: run raw JS that manually calls __WEBDRIVER__.resolve(id, result).
/// Unlike eval_js, the script is NOT wrapped — the caller must call resolve().
async fn eval_js_callback<R: Runtime>(
    state: &SharedState<R>,
    script: &str,
) -> Result<Value, ApiError> {
    let label = state
        .current_window_label
        .lock()
        .expect("lock poisoned")
        .clone();
    let window = window_by_label(&state.app, label.as_deref())
        .ok_or_else(|| ApiError::NotFound("no such window".into()))?;

    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let ws = state.app.state::<WebDriverState>();
        ws.pending_scripts
            .lock()
            .expect("lock poisoned")
            .insert(id.clone(), tx);
    }

    let final_script = script.replace("__CALLBACK_ID__", &id);

    window
        .eval(&final_script)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    match tokio::time::timeout(Duration::from_secs(30), rx).await {
        Ok(Ok(value)) => {
            if let Some(obj) = value.as_object() {
                if obj.contains_key("error") && obj.contains_key("message") {
                    let msg = obj
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("script error");
                    return Err(ApiError::Internal(msg.to_string()));
                }
            }
            Ok(value)
        }
        Ok(Err(_)) => Err(ApiError::Internal("result channel closed".into())),
        Err(_) => {
            let ws = state.app.state::<WebDriverState>();
            ws.pending_scripts
                .lock()
                .expect("lock poisoned")
                .remove(&id);
            Err(ApiError::Internal("screenshot timed out".into()))
        }
    }
}

async fn screenshot<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let script = r#"(function(){try{
var el=document.documentElement;
var w=Math.max(el.scrollWidth,el.clientWidth);
var h=Math.max(el.scrollHeight,el.clientHeight);
var xml=new XMLSerializer().serializeToString(el);
var svg='<svg xmlns="http://www.w3.org/2000/svg" width="'+w+'" height="'+h+'">'
+'<foreignObject width="100%" height="100%">'+xml+'</foreignObject></svg>';
var c=document.createElement('canvas');c.width=w;c.height=h;
var ctx=c.getContext('2d');var img=new Image();
img.onload=function(){try{ctx.drawImage(img,0,0);
var d=c.toDataURL('image/png').split(',')[1];
window.__WEBDRIVER__.resolve("__CALLBACK_ID__",d)}
catch(e){window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{error:"SecurityError",message:e.message,stacktrace:""})}};
img.onerror=function(){window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{error:"ScreenshotError",message:"SVG render failed",stacktrace:""})};
img.src='data:image/svg+xml;charset=utf-8,'+encodeURIComponent(svg)
}catch(e){window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{error:e.name,message:e.message,stacktrace:e.stack||""})}})()"#;

    let result = eval_js_callback(&state, script).await?;
    Ok(Json(json!({"data": result})))
}

async fn screenshot_element<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let find_fn = if body.using.as_deref() == Some("xpath") {
        "findElementByXPath"
    } else {
        "findElement"
    };
    let sel_json = serde_json::to_string(&body.selector).unwrap();
    let script = format!(
        r#"(function(){{try{{
var tgt=window.__WEBDRIVER__.{find_fn}({sel_json},{index});
if(!tgt){{window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{{error:"NoSuchElement",message:"element not found",stacktrace:""}});return}}
var rect=tgt.getBoundingClientRect();
var el=document.documentElement;
var w=Math.max(el.scrollWidth,el.clientWidth);
var h=Math.max(el.scrollHeight,el.clientHeight);
var xml=new XMLSerializer().serializeToString(el);
var svg='<svg xmlns="http://www.w3.org/2000/svg" width="'+w+'" height="'+h+'">'
+'<foreignObject width="100%" height="100%">'+xml+'</foreignObject></svg>';
var fc=document.createElement('canvas');fc.width=w;fc.height=h;
var fctx=fc.getContext('2d');var img=new Image();
img.onload=function(){{try{{fctx.drawImage(img,0,0);
var c=document.createElement('canvas');
c.width=Math.ceil(rect.width);c.height=Math.ceil(rect.height);
var ctx=c.getContext('2d');
ctx.drawImage(fc,rect.x,rect.y,rect.width,rect.height,0,0,rect.width,rect.height);
var d=c.toDataURL('image/png').split(',')[1];
window.__WEBDRIVER__.resolve("__CALLBACK_ID__",d)}}
catch(e){{window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{{error:"SecurityError",message:e.message,stacktrace:""}})}}}};
img.onerror=function(){{window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{{error:"ScreenshotError",message:"SVG render failed",stacktrace:""}})}};
img.src='data:image/svg+xml;charset=utf-8,'+encodeURIComponent(svg)
}}catch(e){{window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{{error:e.name,message:e.message,stacktrace:e.stack||""}})}}}})()
"#,
        find_fn = find_fn,
        sel_json = sel_json,
        index = body.index,
    );

    let result = eval_js_callback(&state, &script).await?;
    Ok(Json(json!({"data": result})))
}

// --- Print to PDF handler ---

async fn print_page<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    // Render the page to a canvas (same SVG foreignObject approach as screenshots),
    // then wrap the PNG image data in a minimal PDF 1.4 structure.
    let script = r#"(function(){try{
var el=document.documentElement;
var w=Math.max(el.scrollWidth,el.clientWidth);
var h=Math.max(el.scrollHeight,el.clientHeight);
var xml=new XMLSerializer().serializeToString(el);
var svg='<svg xmlns="http://www.w3.org/2000/svg" width="'+w+'" height="'+h+'">'
+'<foreignObject width="100%" height="100%">'+xml+'</foreignObject></svg>';
var c=document.createElement('canvas');c.width=w;c.height=h;
var ctx=c.getContext('2d');var img=new Image();
img.onload=function(){try{ctx.drawImage(img,0,0);
var pngDataUrl=c.toDataURL('image/png');
var pngB64=pngDataUrl.split(',')[1];
var bin=atob(pngB64);var len=bin.length;
var imgW=w;var imgH=h;
var pageW=612;var pageH=792;
var scaleX=pageW/imgW;var scaleY=pageH/imgH;
var sc=Math.min(scaleX,scaleY);
var dw=Math.round(imgW*sc);var dh=Math.round(imgH*sc);
var objs=[];var offsets=[];
function addObj(s){offsets.push(objs.join('').length);objs.push(s)}
addObj('%PDF-1.4\n');
addObj('1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n');
addObj('2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n');
addObj('3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 '+pageW+' '+pageH+'] /Contents 5 0 R /Resources << /XObject << /Img 4 0 R >> >> >>\nendobj\n');
var imgStream='4 0 obj\n<< /Type /XObject /Subtype /Image /Width '+imgW+' /Height '+imgH+' /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /ASCIIHexDecode /Length '+(len*6+1)+' >>\nstream\n';
var hexParts=[];for(var i=0;i<len;i++){
var byte=bin.charCodeAt(i);
hexParts.push(('0'+byte.toString(16)).slice(-2))}
imgStream+=hexParts.join('')+'>\nendstream\nendobj\n';
addObj(imgStream);
var contentStr='q '+dw+' 0 0 '+dh+' 0 '+(pageH-dh)+' cm /Img Do Q';
addObj('5 0 obj\n<< /Length '+contentStr.length+' >>\nstream\n'+contentStr+'\nendstream\nendobj\n');
var body=objs.join('');
var xrefOff=body.length;
var xref='xref\n0 6\n0000000000 65535 f \n';
for(var j=1;j<offsets.length;j++){
xref+=('0000000000'+offsets[j]).slice(-10)+' 00000 n \n'}
xref+='trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n'+xrefOff+'\n%%EOF';
var pdf=body+xref;
var pdfB64=btoa(pdf);
window.__WEBDRIVER__.resolve("__CALLBACK_ID__",pdfB64)}
catch(e){window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{error:e.name,message:e.message,stacktrace:e.stack||""})}};
img.onerror=function(){window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{error:"PrintError",message:"SVG render failed",stacktrace:""})};
img.src='data:image/svg+xml;charset=utf-8,'+encodeURIComponent(svg)
}catch(e){window.__WEBDRIVER__.resolve("__CALLBACK_ID__",
{error:e.name,message:e.message,stacktrace:e.stack||""})}})()"#;

    let result = eval_js_callback(&state, script).await?;
    Ok(Json(json!({"data": result})))
}

// --- Cookie handlers ---

async fn cookie_get_all<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let script = r#"
var store = window.__WEBDRIVER__.cookies;
var cookies = [];
var keys = Object.keys(store);
for (var i = 0; i < keys.length; i++) {
    cookies.push(store[keys[i]]);
}
return cookies;
"#;
    let result = eval_js(&state, script).await?;
    Ok(Json(json!({"cookies": result})))
}

async fn cookie_get<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<CookieNameReq>,
) -> ApiResult {
    let name_json = serde_json::to_string(&body.name).unwrap();
    let script = format!(
        "var c=window.__WEBDRIVER__.cookies[{name_json}];\
         return c||null"
    );
    let result = eval_js(&state, &script).await?;
    Ok(Json(json!({"cookie": result})))
}

async fn cookie_add<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<CookieAddReq>,
) -> ApiResult {
    let c = &body.cookie;
    let name_json = serde_json::to_string(&c.name).unwrap();
    let value_json = serde_json::to_string(&c.value).unwrap();
    let path_json = serde_json::to_string(&c.path).unwrap();
    let domain_json = match &c.domain {
        Some(d) => serde_json::to_string(d).unwrap(),
        None => "window.location.hostname".to_string(),
    };
    let secure = c.secure;
    let http_only = c.http_only;
    let expiry_js = match c.expiry {
        Some(e) => format!("{e}"),
        None => "null".to_string(),
    };

    let script = format!(
        "window.__WEBDRIVER__.cookies[{name_json}]={{\
         name:{name_json},value:{value_json},path:{path_json},\
         domain:{domain_json},secure:{secure},httpOnly:{http_only},\
         expiry:{expiry_js},sameSite:\"Lax\"\
         }};return null"
    );

    eval_js(&state, &script).await?;
    Ok(Json(json!(null)))
}

async fn cookie_delete<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<CookieNameReq>,
) -> ApiResult {
    let name_json = serde_json::to_string(&body.name).unwrap();
    let script = format!(
        "delete window.__WEBDRIVER__.cookies[{name_json}];return null"
    );
    eval_js(&state, &script).await?;
    Ok(Json(json!(null)))
}

async fn cookie_delete_all<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let script = "var s=window.__WEBDRIVER__.cookies;\
         var k=Object.keys(s);for(var i=0;i<k.length;i++)delete s[k[i]];\
         return null";
    eval_js(&state, script).await?;
    Ok(Json(json!(null)))
}

// --- Action handlers ---

async fn actions_perform<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<Value>,
) -> ApiResult {
    let action_sequences = body
        .get("actions")
        .and_then(|a| a.as_array())
        .ok_or_else(|| ApiError::Internal("Missing 'actions' array".into()))?;

    // Determine the number of ticks (max length across all action sequences).
    let tick_count = action_sequences
        .iter()
        .filter_map(|seq| seq.get("actions").and_then(|a| a.as_array()).map(|a| a.len()))
        .max()
        .unwrap_or(0);

    // Process each tick across all input sources.
    for tick_idx in 0..tick_count {
        let mut js_parts: Vec<String> = Vec::new();
        let mut pause_ms: u64 = 0;

        for seq in action_sequences {
            let source_type = seq.get("type").and_then(|t| t.as_str()).unwrap_or("null");
            let actions_arr = match seq.get("actions").and_then(|a| a.as_array()) {
                Some(a) => a,
                None => continue,
            };
            let action = match actions_arr.get(tick_idx) {
                Some(a) => a,
                None => continue,
            };
            let action_type = action
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("pause");

            match (source_type, action_type) {
                ("key", "keyDown") => {
                    let key = action
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let key_json = serde_json::to_string(key).unwrap();
                    js_parts.push(format!(
                        "(function(){{var k={key_json};\
                         var code=k.length===1?'Key'+k.toUpperCase():k;\
                         var tgt=document.activeElement||document.body;\
                         tgt.dispatchEvent(new KeyboardEvent('keydown',\
                         {{key:k,code:code,bubbles:true,cancelable:true}}))}})();"
                    ));
                }
                ("key", "keyUp") => {
                    let key = action
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let key_json = serde_json::to_string(key).unwrap();
                    js_parts.push(format!(
                        "(function(){{var k={key_json};\
                         var code=k.length===1?'Key'+k.toUpperCase():k;\
                         var tgt=document.activeElement||document.body;\
                         tgt.dispatchEvent(new KeyboardEvent('keyup',\
                         {{key:k,code:code,bubbles:true,cancelable:true}}))}})();"
                    ));
                }
                ("pointer", "pointerMove") => {
                    let x = action.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let y = action.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let origin = action
                        .get("origin")
                        .and_then(|v| v.as_str())
                        .unwrap_or("viewport");

                    // If origin is an element object, resolve its center.
                    if let Some(origin_obj) =
                        action.get("origin").and_then(|v| v.as_object())
                    {
                        if let Some(elem) =
                            origin_obj.values().next().and_then(|v| v.as_object())
                        {
                            let sel = elem
                                .get("selector")
                                .and_then(|s| s.as_str())
                                .unwrap_or("");
                            let idx = elem
                                .get("index")
                                .and_then(|i| i.as_u64())
                                .unwrap_or(0);
                            let sel_json = serde_json::to_string(sel).unwrap();
                            js_parts.push(format!(
                                "(function(){{var el=document.querySelectorAll({sel_json})[{idx}];\
                                 if(el){{var r=el.getBoundingClientRect();\
                                 window.__wdPointerX=r.x+r.width/2+{x};\
                                 window.__wdPointerY=r.y+r.height/2+{y};}}}})();"
                            ));
                        }
                    } else {
                        match origin {
                            "pointer" => {
                                js_parts.push(format!(
                                    "window.__wdPointerX=(window.__wdPointerX||0)+{x};\
                                     window.__wdPointerY=(window.__wdPointerY||0)+{y};"
                                ));
                            }
                            _ => {
                                // "viewport" or any other value
                                js_parts.push(format!(
                                    "window.__wdPointerX={x};window.__wdPointerY={y};"
                                ));
                            }
                        }
                    }

                    // Dispatch mousemove event.
                    js_parts.push(
                        "(function(){var tgt=document.elementFromPoint(\
                         window.__wdPointerX||0,window.__wdPointerY||0)||document.body;\
                         tgt.dispatchEvent(new MouseEvent('mousemove',\
                         {clientX:window.__wdPointerX||0,clientY:window.__wdPointerY||0,\
                         bubbles:true,cancelable:true}))})();"
                            .to_string(),
                    );
                }
                ("pointer", "pointerDown") => {
                    let button =
                        action.get("button").and_then(|v| v.as_u64()).unwrap_or(0);
                    js_parts.push(format!(
                        "(function(){{var tgt=document.elementFromPoint(\
                         window.__wdPointerX||0,window.__wdPointerY||0)||document.body;\
                         tgt.dispatchEvent(new MouseEvent('mousedown',\
                         {{clientX:window.__wdPointerX||0,clientY:window.__wdPointerY||0,\
                         button:{button},bubbles:true,cancelable:true}}))}})();"
                    ));
                }
                ("pointer", "pointerUp") => {
                    let button =
                        action.get("button").and_then(|v| v.as_u64()).unwrap_or(0);
                    js_parts.push(format!(
                        "(function(){{var tgt=document.elementFromPoint(\
                         window.__wdPointerX||0,window.__wdPointerY||0)||document.body;\
                         tgt.dispatchEvent(new MouseEvent('mouseup',\
                         {{clientX:window.__wdPointerX||0,clientY:window.__wdPointerY||0,\
                         button:{button},bubbles:true,cancelable:true}}));\
                         tgt.dispatchEvent(new MouseEvent('click',\
                         {{clientX:window.__wdPointerX||0,clientY:window.__wdPointerY||0,\
                         button:{button},bubbles:true,cancelable:true}}))}})();"
                    ));
                }
                ("wheel", "scroll") => {
                    let x = action.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let y = action.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let delta_x =
                        action.get("deltaX").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let delta_y =
                        action.get("deltaY").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    js_parts.push(format!(
                        "(function(){{var tgt=document.elementFromPoint({x},{y})||document.body;\
                         tgt.dispatchEvent(new WheelEvent('wheel',\
                         {{clientX:{x},clientY:{y},deltaX:{delta_x},deltaY:{delta_y},\
                         bubbles:true,cancelable:true}}))}})();"
                    ));
                }
                (_, "pause") => {
                    let d = action
                        .get("duration")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if d > pause_ms {
                        pause_ms = d;
                    }
                }
                _ => {}
            }
        }

        // Execute the JS for this tick.
        if !js_parts.is_empty() {
            let combined = js_parts.join("");
            let script = format!("{combined}return null");
            eval_js(&state, &script).await?;
        }

        // Apply pause duration for this tick.
        if pause_ms > 0 {
            tokio::time::sleep(Duration::from_millis(pause_ms)).await;
        }
    }

    Ok(Json(json!(null)))
}

async fn actions_release<R: Runtime>(
    AxumState(_state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    // Release all held keys and pointer buttons. Currently returns null
    // as the plugin does not track pressed state across requests.
    Ok(Json(json!(null)))
}

// --- Shadow DOM handlers ---

async fn element_shadow<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "return el.shadowRoot !== null",
    )
    .await?;
    Ok(Json(json!({"hasShadow": result})))
}

#[derive(Deserialize)]
struct ShadowFindReq {
    host_selector: String,
    host_index: usize,
    #[serde(default)]
    host_using: Option<String>,
    #[allow(dead_code)]
    using: String,
    value: String,
}

async fn shadow_find<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ShadowFindReq>,
) -> ApiResult {
    let host_find_fn = if body.host_using.as_deref() == Some("xpath") {
        "findElementByXPath"
    } else {
        "findElement"
    };
    let host_sel_json = serde_json::to_string(&body.host_selector).unwrap();
    let val_json = serde_json::to_string(&body.value).unwrap();

    let script = format!(
        "if(!window.__wdShadowCtr)window.__wdShadowCtr=0;\
         var host=window.__WEBDRIVER__.{host_find_fn}({host_sel_json},{host_index});\
         if(!host)throw new Error('host element not found');\
         var sr=host.shadowRoot;\
         if(!sr)throw new Error('no shadow root');\
         var els=sr.querySelectorAll({val_json});\
         var a=[];for(var i=0;i<els.length;i++){{\
         var id='wds-'+(++window.__wdShadowCtr);\
         window.__WEBDRIVER__.__shadowCache[id]=els[i];\
         a.push({{selector:id,index:0,using:'shadow'}})}}\
         return a",
        host_find_fn = host_find_fn,
        host_sel_json = host_sel_json,
        host_index = body.host_index,
        val_json = val_json,
    );

    let result = eval_js(&state, &script).await?;
    Ok(Json(json!({"elements": result})))
}

// --- Switch to window handler ---

#[derive(Deserialize)]
struct SwitchWindowReq {
    label: String,
}

async fn window_set_current<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<SwitchWindowReq>,
) -> ApiResult {
    // Validate window exists
    let window = state
        .app
        .get_webview_window(&body.label)
        .ok_or_else(|| ApiError::NotFound(format!("window '{}' not found", body.label)))?;
    // Focus the window (W3C spec: Switch To Window brings window to foreground)
    let _ = window.set_focus();
    // Reset frame stack (W3C spec: switching windows resets to top-level context)
    state.frame_stack.lock().expect("lock poisoned").clear();
    *state
        .current_window_label
        .lock()
        .expect("lock poisoned") = Some(body.label.clone());
    Ok(Json(json!(true)))
}

// --- Find element from element (scoped search) ---

#[derive(Deserialize)]
struct FindFromReq {
    parent_selector: String,
    parent_index: usize,
    #[serde(default)]
    parent_using: Option<String>,
    using: String,
    value: String,
}

async fn element_find_from<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<FindFromReq>,
) -> ApiResult {
    let parent_sel_json = serde_json::to_string(&body.parent_selector).unwrap();
    let val_json = serde_json::to_string(&body.value).unwrap();

    // Find parent using document.querySelectorAll/evaluate directly
    // (works in both frame and top-level contexts since eval_js shadows document).
    let parent_js = if body.parent_using.as_deref() == Some("xpath") {
        format!(
            "var __xr=document.evaluate({sel},document,null,\
             XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null);\
             var parent=__xr.snapshotItem({idx});\
             if(!parent)throw new Error('parent element not found');",
            sel = parent_sel_json,
            idx = body.parent_index,
        )
    } else {
        format!(
            "var parent=document.querySelectorAll({sel})[{idx}];\
             if(!parent)throw new Error('parent element not found');",
            sel = parent_sel_json,
            idx = body.parent_index,
        )
    };

    let child_js = if body.using == "xpath" {
        format!(
            "var r=document.evaluate({v},parent,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null);\
             var a=[];for(var i=0;i<r.snapshotLength;i++){{\
             var e=r.snapshotItem(i);var id='wd-'+(++window.__wdFindFromCtr);\
             e.setAttribute('data-wd-id',id);\
             a.push({{selector:'[data-wd-id=\"'+id+'\"]',index:0}})}}\
             return a",
            v = val_json,
        )
    } else {
        format!(
            "var els=parent.querySelectorAll({v});\
             var a=[];for(var i=0;i<els.length;i++){{\
             var id='wd-'+(++window.__wdFindFromCtr);\
             els[i].setAttribute('data-wd-id',id);\
             a.push({{selector:'[data-wd-id=\"'+id+'\"]',index:0}})}}\
             return a",
            v = val_json,
        )
    };

    let script = format!(
        "if(!window.__wdFindFromCtr)window.__wdFindFromCtr=0;\
         {parent_js}{child_js}"
    );

    let result = eval_js(&state, &script).await?;
    Ok(Json(json!({"elements": result})))
}

// --- Computed ARIA role + label handlers ---

async fn element_computed_role<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let js = r#"var tag=el.tagName.toLowerCase();
var role=el.getAttribute('role');
if(role)return role;
var map={button:'button',a:'link',h1:'heading',h2:'heading',h3:'heading',h4:'heading',h5:'heading',h6:'heading',
input:'textbox',textarea:'textbox',select:'combobox',option:'option',ul:'list',ol:'list',li:'listitem',
table:'table',tr:'row',td:'cell',th:'columnheader',img:'img',nav:'navigation',main:'main',header:'banner',
footer:'contentinfo',aside:'complementary',form:'form',details:'group',summary:'button',dialog:'dialog',
progress:'progressbar',meter:'meter'};
if(tag==='input'){var t=(el.getAttribute('type')||'text').toLowerCase();
if(t==='checkbox')return 'checkbox';if(t==='radio')return 'radio';
if(t==='range')return 'slider';if(t==='number')return 'spinbutton';
if(t==='search')return 'searchbox';return 'textbox'}
if(tag==='a'&&el.hasAttribute('href'))return 'link';
return map[tag]||'generic'"#;
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        js,
    )
    .await?;
    Ok(Json(json!({"role": result})))
}

async fn element_computed_label<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let js = r#"var lblBy=el.getAttribute('aria-labelledby');
if(lblBy){var ids=lblBy.split(/\s+/);var parts=[];
for(var i=0;i<ids.length;i++){var e=document.getElementById(ids[i]);if(e)parts.push(e.textContent.trim())}
if(parts.length)return parts.join(' ')}
var lbl=el.getAttribute('aria-label');if(lbl)return lbl;
if(el.id){var labels=document.querySelectorAll('label[for="'+el.id+'"]');
if(labels.length)return labels[0].textContent.trim()}
if(el.placeholder)return el.placeholder;
if(el.alt)return el.alt;
if(el.title)return el.title;
return ''"#;
    let result = eval_on_element(
        &state,
        &body.selector,
        body.index,
        body.using.as_deref(),
        js,
    )
    .await?;
    Ok(Json(json!({"label": result})))
}

// --- Active element handler ---

async fn element_active<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let result =
        eval_js(&state, "return window.__WEBDRIVER__.getActiveElement()").await?;
    Ok(Json(json!({"element": result})))
}

// --- Page source handler ---

async fn get_source<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let result =
        eval_js(&state, "return document.documentElement.outerHTML").await?;
    Ok(Json(json!({"source": result})))
}

// --- Frame handlers ---

#[derive(Deserialize)]
struct FrameSwitchReq {
    id: Value, // null = top, number = index, object = element ref
}

async fn frame_switch<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<FrameSwitchReq>,
) -> ApiResult {
    if body.id.is_null() {
        // Switch to top-level browsing context: clear the frame stack
        state.frame_stack.lock().expect("lock poisoned").clear();
        return Ok(Json(json!(null)));
    }

    if let Some(index) = body.id.as_u64() {
        // Switch by frame index
        state
            .frame_stack
            .lock()
            .expect("lock poisoned")
            .push(FrameRef {
                selector: "iframe".to_string(),
                index: index as usize,
            });
        return Ok(Json(json!(null)));
    }

    if let Some(obj) = body.id.as_object() {
        // Switch by element reference: {selector, index}
        let selector = obj
            .get("selector")
            .and_then(|s| s.as_str())
            .ok_or_else(|| ApiError::Internal("frame element missing selector".into()))?
            .to_string();
        let index = obj
            .get("index")
            .and_then(|i| i.as_u64())
            .unwrap_or(0) as usize;
        state
            .frame_stack
            .lock()
            .expect("lock poisoned")
            .push(FrameRef { selector, index });
        return Ok(Json(json!(null)));
    }

    Err(ApiError::Internal("invalid frame id".into()))
}

async fn frame_parent<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let mut stack = state.frame_stack.lock().expect("lock poisoned");
    stack.pop(); // If already at top, this is a no-op
    Ok(Json(json!(null)))
}

// --- Server entry point ---

pub(crate) async fn start<R: Runtime>(
    app: tauri::AppHandle<R>,
    _webview_created_rx: tokio::sync::broadcast::Receiver<tauri::WebviewWindow<R>>,
) {
    let state: SharedState<R> = Arc::new(ServerState {
        app,
        current_window_label: std::sync::Mutex::new(None),
        frame_stack: std::sync::Mutex::new(Vec::new()),
    });

    let router = Router::new()
        // Window
        .route("/window/handle", post(window_handle::<R>))
        .route("/window/handles", post(window_handles::<R>))
        .route("/window/close", post(window_close::<R>))
        .route("/window/rect", post(window_rect::<R>))
        .route("/window/set-rect", post(window_set_rect::<R>))
        .route("/window/fullscreen", post(window_fullscreen::<R>))
        .route("/window/minimize", post(window_minimize::<R>))
        .route("/window/maximize", post(window_maximize::<R>))
        .route("/window/insets", post(window_insets::<R>))
        .route("/window/set-current", post(window_set_current::<R>))
        .route("/window/new", post(window_new::<R>))
        // Elements
        .route("/element/find", post(element_find::<R>))
        .route("/element/text", post(element_text::<R>))
        .route("/element/attribute", post(element_attribute::<R>))
        .route("/element/property", post(element_property::<R>))
        .route("/element/tag", post(element_tag::<R>))
        .route("/element/rect", post(element_rect::<R>))
        .route("/element/click", post(element_click::<R>))
        .route("/element/clear", post(element_clear::<R>))
        .route("/element/send-keys", post(element_send_keys::<R>))
        .route("/element/set-files", post(element_set_files::<R>))
        .route("/element/displayed", post(element_displayed::<R>))
        .route("/element/enabled", post(element_enabled::<R>))
        .route("/element/selected", post(element_selected::<R>))
        .route("/element/active", post(element_active::<R>))
        .route("/element/find-from", post(element_find_from::<R>))
        .route("/element/shadow", post(element_shadow::<R>))
        .route("/shadow/find", post(shadow_find::<R>))
        .route("/element/computed-role", post(element_computed_role::<R>))
        .route("/element/computed-label", post(element_computed_label::<R>))
        // Scripts
        .route("/script/execute", post(script_execute::<R>))
        .route("/script/execute-async", post(script_execute_async::<R>))
        // Navigation
        .route("/navigate/url", post(navigate_url::<R>))
        .route("/navigate/current", post(navigate_current::<R>))
        .route("/navigate/title", post(navigate_title::<R>))
        .route("/navigate/back", post(navigate_back::<R>))
        .route("/navigate/forward", post(navigate_forward::<R>))
        .route("/navigate/refresh", post(navigate_refresh::<R>))
        // Screenshots
        .route("/screenshot", post(screenshot::<R>))
        .route("/screenshot/element", post(screenshot_element::<R>))
        // Cookies
        .route("/cookie/get-all", post(cookie_get_all::<R>))
        .route("/cookie/get", post(cookie_get::<R>))
        .route("/cookie/add", post(cookie_add::<R>))
        .route("/cookie/delete", post(cookie_delete::<R>))
        .route("/cookie/delete-all", post(cookie_delete_all::<R>))
        // Alerts
        .route("/alert/text", post(alert_get_text::<R>))
        .route("/alert/dismiss", post(alert_dismiss::<R>))
        .route("/alert/accept", post(alert_accept::<R>))
        .route("/alert/send-text", post(alert_send_text::<R>))
        // Page source
        .route("/source", post(get_source::<R>))
        // Print
        .route("/print", post(print_page::<R>))
        // Actions
        .route("/actions/perform", post(actions_perform::<R>))
        .route("/actions/release", post(actions_release::<R>))
        // Frames
        .route("/frame/switch", post(frame_switch::<R>))
        .route("/frame/parent", post(frame_parent::<R>))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind webdriver plugin server");
    let port = listener.local_addr().unwrap().port();
    println!("[webdriver] listening on port {}", port);

    axum::serve(listener, router)
        .await
        .expect("webdriver plugin server error");
}
