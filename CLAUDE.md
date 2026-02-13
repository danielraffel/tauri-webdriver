# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Open-source W3C WebDriver implementation for Tauri desktop apps on macOS. Two Rust crates work together: a **plugin** (runs inside the Tauri app) and a **CLI** (standalone WebDriver server). No closed-source dependencies.

```
WebDriverIO/Selenium ──HTTP:4444──> tauri-webdriver CLI ──HTTP:{dynamic}──> tauri-plugin-webdriver
                                    (W3C WebDriver)                         (axum server in-app)
```

## Build Commands

```sh
# Build both crates (workspace root)
cargo build

# Build test app (separate workspace, excluded from root)
cd tests/test-app/src-tauri && cargo build

# Run all tests (builds everything, then runs plugin + W3C tests)
bash tests/run_all_tests.sh

# Run just plugin-level tests (direct HTTP against plugin server)
bash tests/run_plugin_tests.sh

# Run just W3C-level tests (full stack: CLI → plugin → app)
bash tests/run_w3c_tests.sh

# Run WDIO compatibility tests (requires npm install in tests/wdio/)
cd tests/wdio && bash run.sh
```

## Architecture

### Crate 1: `tauri-plugin-webdriver` (`crates/tauri-plugin-webdriver/`)

Tauri v2 plugin. Starts an axum HTTP server on `127.0.0.1:{random_port}` during `Plugin::setup()`. Prints `[webdriver] listening on port {N}` to stdout for discovery.

- **`lib.rs`** — Plugin entry point. Registers `resolve` IPC command, injects `init.js`, spawns HTTP server. Manages `WebDriverState` (pending script oneshot channels).
- **`server.rs`** — All HTTP handlers. Every endpoint is `POST` with JSON. Uses `eval_js()` helper that wraps JS in an IIFE, calls `window.__WEBDRIVER__.resolve(id, result)` to return values via Tauri IPC. `eval_js_callback()` variant for async operations (screenshots) where the JS itself calls resolve.
- **`init.js`** — Injected into every webview. Defines `window.__WEBDRIVER__` with `resolve()`, `findElement()`, `findElementByXPath()`, `cache` (element cache), and `cookies` (in-memory cookie store, since `document.cookie` doesn't work on `tauri://` scheme).

Key pattern: All DOM interaction goes through JS evaluation. The plugin evaluates JavaScript in the webview and receives results back via the `plugin:webdriver|resolve` Tauri IPC command.

### Crate 2: `tauri-webdriver` (`crates/tauri-webdriver/`)

Single-file CLI binary (`main.rs`, ~1200 lines). Implements the W3C WebDriver HTTP protocol on port 4444.

- Launches the Tauri app binary, watches stdout for the plugin port
- Translates W3C requests into plugin HTTP API calls via `plugin_post()`
- Manages element state: maps W3C element UUIDs ↔ `(css_selector, index, using)` triples
- Single-session: one active session at a time
- Uses `{param}` path syntax (axum 0.8)

### Test App (`tests/test-app/`)

Minimal Tauri app with testable elements (counter button, text input, dropdown, hidden div). Separate Cargo workspace — build with `cd tests/test-app/src-tauri && cargo build`.

## Key Conventions

- **Element identity**: Elements are `(selector, index)` pairs internally. CSS: `querySelectorAll(sel)[idx]`. XPath: `document.evaluate()` snapshot. The W3C layer assigns UUID strings mapped back to these pairs.
- **W3C element key**: `element-6066-11e4-a52e-4f735466cecf` (defined as `W3C_ELEMENT_KEY` constant)
- **Plugin communication**: The CLI discovers the plugin via stdout line parsing (`[webdriver] listening on port {N}`), then communicates exclusively via HTTP POST to `127.0.0.1:{N}`.
- **Locator strategies**: `css selector`, `tag name`, `xpath`, `link text`, `partial link text` — the latter two convert to XPath internally in `extract_locator()`.
- **Cookie store**: Uses `window.__WEBDRIVER__.cookies` (JS object) instead of `document.cookie` because WKWebView doesn't support `document.cookie` on custom URL schemes like `tauri://`.
- **Actions**: Perform Actions dispatches `KeyboardEvent`, `MouseEvent`, `WheelEvent` via JavaScript `dispatchEvent()` — not native OS input.
- **Screenshots**: SVG foreignObject + Canvas approach (serialize DOM to SVG, render to canvas, export as base64 PNG).
- **Error mapping**: Plugin HTTP 500 → `W3cError`. Script execution errors specifically map to `"javascript error"` W3C error code.
- **Debug-only plugin**: The plugin should only be registered in debug builds via `#[cfg(debug_assertions)]`.
