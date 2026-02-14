// tauri-plugin-webdriver-automation: Tauri plugin enabling WebDriver-based e2e testing.
//
// This plugin runs an HTTP server inside the Tauri app (debug builds only) that
// allows an external WebDriver server to interact with the webview: find elements,
// click buttons, read text, manage windows, and execute JavaScript.

use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{Manager, Runtime, State};

mod server;

// --- Tauri IPC command: receives script results from the JS bridge ---

#[tauri::command]
async fn resolve<R: Runtime>(
    _app: tauri::AppHandle<R>,
    webdriver: State<'_, WebDriverState>,
    id: String,
    result: Option<serde_json::Value>,
) -> Result<(), ()> {
    webdriver
        .pending_scripts
        .lock()
        .expect("failed to lock pending scripts")
        .remove(&id)
        .expect("no pending script with that id")
        .send(result.unwrap_or_default())
        .expect("failed to send script result");
    Ok(())
}

// --- Internal types ---

pub(crate) struct WebDriverState {
    pub pending_scripts: Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>,
}

// --- Plugin entry point ---

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let (webview_created_tx, webview_created_rx) = tokio::sync::broadcast::channel(16);

    tauri::plugin::Builder::new("webdriver-automation")
        .invoke_handler(tauri::generate_handler![resolve])
        .js_init_script(include_str!("init.js").to_string())
        .on_webview_ready(move |webview| {
            webview_created_tx
                .send(
                    webview
                        .get_webview_window(webview.label())
                        .unwrap_or_else(|| {
                            panic!("failed to get webview window for label {}", webview.label())
                        }),
                )
                .unwrap_or_default();
        })
        .setup(move |app, _api| {
            app.manage(WebDriverState {
                pending_scripts: Mutex::new(HashMap::new()),
            });

            app.add_capability(
                tauri::ipc::CapabilityBuilder::new("webdriver-automation")
                    .local(true)
                    .window("*")
                    .remote("http://*".into())
                    .remote("https://*".into())
                    .permission("webdriver-automation:default"),
            )?;

            // Start the HTTP server that the external WebDriver CLI connects to.
            let app_handle = app.clone();
            let rx = webview_created_rx.resubscribe();
            tauri::async_runtime::spawn(async move {
                server::start(app_handle, rx).await;
            });

            Ok(())
        })
        .build()
}

// --- Helper: resolve a window by label ---

pub(crate) fn window_by_label<R: Runtime>(
    app: &tauri::AppHandle<R>,
    label: Option<&str>,
) -> Option<tauri::WebviewWindow<R>> {
    if let Some(label) = label {
        app.get_webview_window(label)
    } else {
        app.get_webview_window("main")
            .or_else(|| app.webview_windows().into_values().next())
    }
}
