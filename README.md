# tauri-webdriver

[![CI](https://github.com/danielraffel/tauri-webdriver/workflows/CI/badge.svg)](https://github.com/danielraffel/tauri-webdriver/actions)
[![Crates.io](https://img.shields.io/crates/v/tauri-webdriver-automation.svg)](https://crates.io/crates/tauri-webdriver-automation)
[![Plugin Crate](https://img.shields.io/crates/v/tauri-plugin-webdriver-automation.svg?label=plugin)](https://crates.io/crates/tauri-plugin-webdriver-automation)
[![Docs.rs](https://docs.rs/tauri-webdriver-automation/badge.svg)](https://docs.rs/tauri-webdriver-automation)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

**Open-source macOS WebDriver for Tauri apps.**

Enables automated end-to-end testing of Tauri desktop applications on macOS, where no native WKWebView WebDriver exists.

## The Problem

Tauri apps use WKWebView on macOS. Unlike Linux (WebKitWebDriver) and Windows (Edge WebDriver), Apple does not provide a WebDriver implementation for WKWebView. This means Tauri developers cannot run automated e2e tests on macOS using standard WebDriver tools like WebDriverIO or Selenium.

This is a blocker for any Tauri app with platform-specific code (e.g., deep links, native menus, file associations) that must be tested on every platform.

## The Solution

`tauri-webdriver` provides two crates that together bridge the gap:

1. **[`tauri-plugin-webdriver-automation`](https://crates.io/crates/tauri-plugin-webdriver-automation)** -- A Tauri plugin that runs inside your app (debug builds only). It starts a local HTTP server that can interact with your app's webview: find elements, click buttons, read text, manage windows, and execute JavaScript.

2. **[`tauri-webdriver-automation`](https://crates.io/crates/tauri-webdriver-automation)** (CLI binary: `tauri-wd`) -- A standalone CLI binary that implements the [W3C WebDriver protocol](https://www.w3.org/TR/webdriver2/). It launches your Tauri app, connects to the plugin's HTTP server, and translates standard WebDriver commands into plugin API calls. WebDriverIO, Selenium, or any W3C-compatible client can connect to it.

```
WebDriverIO/Selenium                 tauri-wd CLI                Your Tauri App
  (test runner)        ──HTTP──>    (W3C WebDriver)   ──HTTP──>  (plugin server)
                        :4444                                     :{dynamic port}
```

## Who Is This For?

- **Tauri app developers** who need automated e2e tests on macOS
- **CI/CD pipelines** that run tests across macOS, Linux, and Windows
- **Anyone with platform-specific Tauri code** that must be verified on macOS (deep links, native APIs, system integrations)

## Quick Start

### 1. Add the plugin to your Tauri app

```sh
cd src-tauri
cargo add tauri-plugin-webdriver-automation
```

Register it in your app (debug builds only):

```rust
let mut builder = tauri::Builder::default();
#[cfg(debug_assertions)]
{
    builder = builder.plugin(tauri_plugin_webdriver_automation::init());
}
```

### 2. Install the CLI

```sh
cargo install tauri-webdriver-automation
```

### 3. Configure WebDriverIO

```js
// wdio.conf.mjs
export const config = {
    port: 4444,
    capabilities: [{
        'tauri:options': {
            binary: './src-tauri/target/debug/my-app',
        }
    }],
    // ... your test config
};
```

### 4. Run tests

```sh
# Terminal 1: Start the WebDriver server
tauri-wd --port 4444

# Terminal 2: Run your tests
npx wdio run wdio.conf.mjs
```

## Supported W3C WebDriver Operations

All operations follow the [W3C WebDriver specification](https://www.w3.org/TR/webdriver2/). See the [full technical specification](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md) for detailed request/response formats and plugin API documentation.

### Sessions

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/status` | GET | Server readiness status |
| `/session` | POST | [Create a new session](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#session-creation-flow) with `tauri:options` capabilities |
| `/session/{id}` | DELETE | Delete session and terminate the app |
| `/session/{id}/timeouts` | GET | Get current timeout configuration |
| `/session/{id}/timeouts` | POST | Set implicit, page load, and script timeouts |

### Navigation

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/session/{id}/url` | POST | [Navigate to URL](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#navigation) |
| `/session/{id}/url` | GET | Get current page URL |
| `/session/{id}/title` | GET | Get page title |
| `/session/{id}/source` | GET | Get full page HTML source |
| `/session/{id}/back` | POST | Navigate back in history |
| `/session/{id}/forward` | POST | Navigate forward in history |
| `/session/{id}/refresh` | POST | Refresh the current page |

### Windows

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/session/{id}/window` | GET | [Get current window handle](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#window) |
| `/session/{id}/window` | POST | Switch to window by handle |
| `/session/{id}/window` | DELETE | Close current window |
| `/session/{id}/window/handles` | GET | Get all window handles |
| `/session/{id}/window/rect` | GET | Get window position and size |
| `/session/{id}/window/rect` | POST | Set window position and size |
| `/session/{id}/window/maximize` | POST | Maximize window |
| `/session/{id}/window/minimize` | POST | Minimize window |
| `/session/{id}/window/fullscreen` | POST | Make window fullscreen |

### Elements

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/session/{id}/element` | POST | [Find element](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#elements) (CSS, XPath, tag name, link text, partial link text) |
| `/session/{id}/elements` | POST | Find all matching elements |
| `/session/{id}/element/active` | GET | Get the currently focused element |
| `/session/{id}/element/{eid}/element` | POST | Find element scoped to a parent element |
| `/session/{id}/element/{eid}/elements` | POST | Find all elements scoped to a parent |
| `/session/{id}/element/{eid}/click` | POST | Click an element |
| `/session/{id}/element/{eid}/clear` | POST | Clear an input element |
| `/session/{id}/element/{eid}/value` | POST | Send keystrokes to an element |
| `/session/{id}/element/{eid}/text` | GET | Get element's visible text |
| `/session/{id}/element/{eid}/name` | GET | Get element's tag name |
| `/session/{id}/element/{eid}/attribute/{name}` | GET | Get an HTML attribute value |
| `/session/{id}/element/{eid}/property/{name}` | GET | Get a JavaScript property value |
| `/session/{id}/element/{eid}/css/{name}` | GET | Get a computed CSS property value |
| `/session/{id}/element/{eid}/rect` | GET | Get element's bounding rectangle |
| `/session/{id}/element/{eid}/enabled` | GET | Check if element is enabled |
| `/session/{id}/element/{eid}/selected` | GET | Check if element is selected |
| `/session/{id}/element/{eid}/displayed` | GET | Check if element is visible |
| `/session/{id}/element/{eid}/computedrole` | GET | Get computed ARIA role |
| `/session/{id}/element/{eid}/computedlabel` | GET | Get computed ARIA label |

### Shadow DOM

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/session/{id}/element/{eid}/shadow` | GET | [Get shadow root](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#shadow-dom) of a web component |
| `/session/{id}/shadow/{sid}/element` | POST | Find element inside a shadow root |
| `/session/{id}/shadow/{sid}/elements` | POST | Find all elements inside a shadow root |

### Frames

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/session/{id}/frame` | POST | [Switch to frame](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#frames) by index, element reference, or `null` for top |
| `/session/{id}/frame/parent` | POST | Switch to parent frame |

### Script Execution

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/session/{id}/execute/sync` | POST | [Execute synchronous JavaScript](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#script-execution) |
| `/session/{id}/execute/async` | POST | Execute asynchronous JavaScript with callback |

### Screenshots

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/session/{id}/screenshot` | GET | [Full page screenshot](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#screenshots) (base64 PNG) |
| `/session/{id}/element/{eid}/screenshot` | GET | Element screenshot (base64 PNG) |

### Cookies

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/session/{id}/cookie` | GET | [Get all cookies](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#cookies) |
| `/session/{id}/cookie/{name}` | GET | Get a cookie by name |
| `/session/{id}/cookie` | POST | Add a cookie |
| `/session/{id}/cookie/{name}` | DELETE | Delete a cookie by name |
| `/session/{id}/cookie` | DELETE | Delete all cookies |

### Actions

| W3C Endpoint | Method | Description |
|-------------|--------|-------------|
| `/session/{id}/actions` | POST | [Perform actions](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md#actions): key (keyDown/keyUp), pointer (move/down/up), wheel (scroll) |
| `/session/{id}/actions` | DELETE | Release all actions |

## MCP Integration

`tauri-webdriver` works with [mcp-tauri-automation](https://github.com/danielraffel/mcp-tauri-automation) to enable AI-driven automation of Tauri apps via the [Model Context Protocol](https://modelcontextprotocol.io/). This lets AI agents (like Claude Code) launch, inspect, interact with, and screenshot your Tauri app.

```sh
# 1. Start tauri-wd
tauri-wd --port 4444

# 2. Configure mcp-tauri-automation in your MCP client
# The MCP server connects to tauri-wd on port 4444
# and exposes tools like launch_app, click_element, capture_screenshot, etc.
```

> **Note:** This project recommends the [danielraffel/mcp-tauri-automation](https://github.com/danielraffel/mcp-tauri-automation) fork which includes additional tools (execute_script, get_page_title, get_page_url, multi-strategy selectors, configurable screenshot timeouts, wait_for_navigation). PRs have been submitted upstream to [Radek44/mcp-tauri-automation](https://github.com/Radek44/mcp-tauri-automation).

## Architecture

See the [full technical specification (SPEC.md)](https://github.com/danielraffel/tauri-webdriver/blob/main/SPEC.md) for:
- Plugin HTTP API (all endpoints, request/response formats)
- CLI W3C WebDriver endpoint mapping
- JavaScript bridge internals (element identity, shadow DOM cache, frame context)
- Session creation flow
- Element state management
- Error handling

## License

MIT OR Apache-2.0 (dual-licensed, same as Tauri)
