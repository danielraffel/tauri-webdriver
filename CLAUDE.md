# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Open-source W3C WebDriver implementation for Tauri desktop apps on macOS. Two Rust crates work together: a **plugin** (runs inside the Tauri app) and a **CLI** (standalone WebDriver server). No closed-source dependencies.

```
WebDriverIO/Selenium ──HTTP:4444──> tauri-wd CLI ──HTTP:{dynamic}──> tauri-plugin-webdriver-automation
                                    (W3C WebDriver)                   (axum server in-app)
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

### Crate 1: `tauri-plugin-webdriver-automation` (`crates/tauri-plugin-webdriver-automation/`)

Tauri v2 plugin. Starts an axum HTTP server on `127.0.0.1:{random_port}` during `Plugin::setup()`. Prints `[webdriver] listening on port {N}` to stdout for discovery.

- **`lib.rs`** — Plugin entry point. Registers `resolve` IPC command, injects `init.js`, spawns HTTP server. Manages `WebDriverState` (pending script oneshot channels).
- **`server.rs`** — All HTTP handlers. Every endpoint is `POST` with JSON. Uses `eval_js()` helper that wraps JS in an IIFE, calls `window.__WEBDRIVER__.resolve(id, result)` to return values via Tauri IPC. `eval_js_callback()` variant for async operations (screenshots) where the JS itself calls resolve. Manages frame stack state for iframe navigation and current window label for multi-window support.
- **`init.js`** — Injected into every webview. Defines `window.__WEBDRIVER__` with `resolve()`, `findElement()`, `findElementByXPath()`, `findElementInShadow()`, `getActiveElement()`, `cache` (element cache), `cookies` (in-memory cookie store), and `__shadowCache` (shadow DOM element cache).

Key pattern: All DOM interaction goes through JS evaluation. The plugin evaluates JavaScript in the webview and receives results back via the `plugin:webdriver-automation|resolve` Tauri IPC command.

### Crate 2: `tauri-webdriver-automation` (`crates/tauri-webdriver-automation/`)

Single-file CLI binary (`main.rs`). Implements the W3C WebDriver HTTP protocol on port 4444. Binary name: `tauri-wd`.

- Launches the Tauri app binary, watches stdout for the plugin port
- Translates W3C requests into plugin HTTP API calls via `plugin_post()`
- Manages element state: maps W3C element UUIDs ↔ `(css_selector, index, using)` triples
- Manages shadow root refs: maps W3C shadow UUIDs ↔ host element info
- Single-session: one active session at a time
- Uses `{param}` path syntax (axum 0.8)

### Test App (`tests/test-app/`)

Minimal Tauri app with testable elements (counter button, text input, dropdown, hidden div, shadow DOM web component, iframe). Separate Cargo workspace — build with `cd tests/test-app/src-tauri && cargo build`.

## Key Conventions

- **Element identity**: Elements are `(selector, index, using)` triples internally. CSS: `querySelectorAll(sel)[idx]`. XPath: `document.evaluate()` snapshot. Shadow: direct cache lookup. The W3C layer assigns UUID strings mapped back to these triples.
- **W3C element key**: `element-6066-11e4-a52e-4f735466cecf` (defined as `W3C_ELEMENT_KEY` constant)
- **W3C shadow key**: `shadow-6066-11e4-a52e-4f735466cecf` (defined as `W3C_SHADOW_KEY` constant)
- **Plugin communication**: The CLI discovers the plugin via stdout line parsing (`[webdriver] listening on port {N}`), then communicates exclusively via HTTP POST to `127.0.0.1:{N}`.
- **Locator strategies**: `css selector`, `tag name`, `xpath`, `link text`, `partial link text` — the latter two convert to XPath internally in `extract_locator()`.
- **Cookie store**: Uses `window.__WEBDRIVER__.cookies` (JS object) instead of `document.cookie` because WKWebView doesn't support `document.cookie` on custom URL schemes like `tauri://`.
- **Actions**: Perform Actions dispatches `KeyboardEvent`, `MouseEvent`, `WheelEvent` via JavaScript `dispatchEvent()` — not native OS input.
- **Screenshots**: SVG foreignObject + Canvas approach (serialize DOM to SVG, render to canvas, export as base64 PNG).
- **Shadow DOM**: Elements inside shadow roots are cached in `window.__WEBDRIVER__.__shadowCache` keyed by generated IDs. The `using: "shadow"` locator type resolves elements from this cache rather than `document.querySelectorAll()`.
- **Frame/iframe**: Plugin tracks a frame stack (`Vec<FrameRef>`). When non-empty, `eval_js()` prepends JS that navigates the iframe hierarchy via `contentDocument` and passes the target frame's document as a function parameter to avoid JS hoisting issues.
- **Error mapping**: Plugin HTTP 500 → `W3cError`. Script execution errors specifically map to `"javascript error"` W3C error code.
- **Debug-only plugin**: The plugin should only be registered in debug builds via `#[cfg(debug_assertions)]`.
