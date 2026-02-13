pub fn run() {
    let mut builder = tauri::Builder::default();

    #[cfg(debug_assertions)]
    {
        builder = builder.plugin(tauri_plugin_webdriver::init());
    }

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
