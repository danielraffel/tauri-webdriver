// HTTP server for the tauri-plugin-webdriver plugin.
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

struct ServerState<R: Runtime> {
    app: tauri::AppHandle<R>,
}

type SharedState<R> = Arc<ServerState<R>>;

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
    app: &tauri::AppHandle<R>,
    label: Option<&str>,
    script: &str,
) -> Result<Value, ApiError> {
    let window =
        window_by_label(app, label).ok_or_else(|| ApiError::NotFound("no such window".into()))?;

    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let state = app.state::<WebDriverState>();
        state
            .pending_scripts
            .lock()
            .expect("lock poisoned")
            .insert(id.clone(), tx);
    }

    // Wrap user script: execute it, send result back via IPC.
    let wrapped = format!(
        concat!(
            "(function(){{try{{var __r=(function(){{{script}}})();",
            "window.__WEBDRIVER__.resolve(\"{id}\",__r)",
            "}}catch(__e){{window.__WEBDRIVER__.resolve(\"{id}\",",
            "{{error:__e.name,message:__e.message,stacktrace:__e.stack||\"\"}})",
            "}}}})()"
        ),
        script = script,
        id = id,
    );

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
            let state = app.state::<WebDriverState>();
            state.pending_scripts.lock().expect("lock poisoned").remove(&id);
            Err(ApiError::Internal("script timed out".into()))
        }
    }
}

/// Evaluate JS that operates on a located element.
async fn eval_on_element<R: Runtime>(
    app: &tauri::AppHandle<R>,
    selector: &str,
    index: usize,
    using: Option<&str>,
    body: &str,
) -> Result<Value, ApiError> {
    let find_fn = if using == Some("xpath") {
        "findElementByXPath"
    } else {
        "findElement"
    };
    let sel_json = serde_json::to_string(selector).unwrap();
    let script = format!(
        "var el=window.__WEBDRIVER__.{find_fn}({sel_json},{index});\
         if(!el)throw new Error(\"element not found\");\
         {body}"
    );
    eval_js(app, None, &script).await
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
struct ScriptReq {
    script: String,
    #[serde(default)]
    args: Vec<Value>,
}

#[derive(Deserialize)]
struct NavReq {
    url: String,
}

// --- Window handlers ---

async fn window_handle<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let window =
        window_by_label(&state.app, None).ok_or(ApiError::NotFound("no window".into()))?;
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

    let result = eval_js(&state.app, None, &script).await?;
    Ok(Json(json!({"elements": result})))
}

async fn element_text<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    let result = eval_on_element(
        &state.app,
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
        &state.app,
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
        &state.app,
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
        &state.app,
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
        &state.app,
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
        &state.app,
        &body.selector,
        body.index,
        body.using.as_deref(),
        "el.scrollIntoView({block:'center',inline:'center'});el.click();return null",
    )
    .await?;
    Ok(Json(json!(null)))
}

async fn element_clear<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ElemReq>,
) -> ApiResult {
    eval_on_element(
        &state.app,
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
        &state.app,
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
        &state.app,
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
        &state.app,
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
        &state.app,
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
    let result = eval_js(&state.app, None, &script).await?;
    Ok(Json(json!({"value": result})))
}

async fn script_execute_async<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(body): Json<ScriptReq>,
) -> ApiResult {
    let window =
        window_by_label(&state.app, None).ok_or(ApiError::NotFound("no window".into()))?;

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
        &state.app,
        None,
        &format!("window.location.href={url_json};return null"),
    )
    .await?;
    Ok(Json(json!(null)))
}

async fn navigate_current<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let result = eval_js(&state.app, None, "return window.location.href").await?;
    Ok(Json(json!({"url": result})))
}

async fn navigate_title<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    let result = eval_js(&state.app, None, "return document.title").await?;
    Ok(Json(json!({"title": result})))
}

async fn navigate_back<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    eval_js(&state.app, None, "window.history.back();return null").await?;
    Ok(Json(json!(null)))
}

async fn navigate_forward<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    eval_js(&state.app, None, "window.history.forward();return null").await?;
    Ok(Json(json!(null)))
}

async fn navigate_refresh<R: Runtime>(
    AxumState(state): AxumState<SharedState<R>>,
    Json(_body): Json<Value>,
) -> ApiResult {
    eval_js(&state.app, None, "window.location.reload();return null").await?;
    Ok(Json(json!(null)))
}

// --- Screenshot handlers ---

/// Helper: run raw JS that manually calls __WEBDRIVER__.resolve(id, result).
/// Unlike eval_js, the script is NOT wrapped — the caller must call resolve().
async fn eval_js_callback<R: Runtime>(
    app: &tauri::AppHandle<R>,
    script: &str,
) -> Result<Value, ApiError> {
    let window =
        window_by_label(app, None).ok_or_else(|| ApiError::NotFound("no such window".into()))?;

    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let state = app.state::<WebDriverState>();
        state
            .pending_scripts
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
            let state = app.state::<WebDriverState>();
            state
                .pending_scripts
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

    let result = eval_js_callback(&state.app, script).await?;
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

    let result = eval_js_callback(&state.app, &script).await?;
    Ok(Json(json!({"data": result})))
}

// --- Server entry point ---

pub(crate) async fn start<R: Runtime>(
    app: tauri::AppHandle<R>,
    _webview_created_rx: tokio::sync::broadcast::Receiver<tauri::WebviewWindow<R>>,
) {
    let state: SharedState<R> = Arc::new(ServerState { app });

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
        .route("/element/displayed", post(element_displayed::<R>))
        .route("/element/enabled", post(element_enabled::<R>))
        .route("/element/selected", post(element_selected::<R>))
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
