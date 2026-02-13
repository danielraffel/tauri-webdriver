# tauri-webdriver

**Open-source macOS WebDriver for Tauri apps.**

Enables automated end-to-end testing of Tauri desktop applications on macOS, where no native WKWebView WebDriver exists.

## The Problem

Tauri apps use WKWebView on macOS. Unlike Linux (WebKitWebDriver) and Windows (Edge WebDriver), Apple does not provide a WebDriver implementation for WKWebView. This means Tauri developers cannot run automated e2e tests on macOS using standard WebDriver tools like WebDriverIO or Selenium.

This is a blocker for any Tauri app with platform-specific code (e.g., deep links, native menus, file associations) that must be tested on every platform.

## The Solution

`tauri-webdriver` provides two crates that together bridge the gap:

1. **`tauri-plugin-webdriver`** -- A Tauri plugin that runs inside your app (debug builds only). It starts a local HTTP server that can interact with your app's webview: find elements, click buttons, read text, manage windows, and execute JavaScript.

2. **`tauri-webdriver`** -- A standalone CLI binary that implements the [W3C WebDriver protocol](https://www.w3.org/TR/webdriver2/). It launches your Tauri app, connects to the plugin's HTTP server, and translates standard WebDriver commands into plugin API calls. WebDriverIO, Selenium, or any W3C-compatible client can connect to it.

```
WebDriverIO/Selenium                tauri-webdriver CLI              Your Tauri App
  (test runner)        ──HTTP──>     (W3C WebDriver)    ──HTTP──>   (plugin server)
                        :4444                                        :{dynamic port}
```

## Who Is This For?

- **Tauri app developers** who need automated e2e tests on macOS
- **CI/CD pipelines** that run tests across macOS, Linux, and Windows
- **Anyone with platform-specific Tauri code** that must be verified on macOS (deep links, native APIs, system integrations)

## Quick Start

### 1. Add the plugin to your Tauri app

```sh
cd src-tauri
cargo add tauri-plugin-webdriver
```

Register it in your app (debug builds only):

```rust
let mut builder = tauri::Builder::default();
#[cfg(debug_assertions)]
{
    builder = builder.plugin(tauri_plugin_webdriver::init());
}
```

### 2. Install the CLI

```sh
cargo install tauri-webdriver
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
tauri-webdriver --port 4444

# Terminal 2: Run your tests
npx wdio run wdio.conf.mjs
```

## Supported Operations

| Category | Operations | Status |
|----------|-----------|--------|
| **Sessions** | Create, Delete, Status, Timeouts | Done |
| **Elements** | Find (CSS + XPath), Find All, Click, Clear, Send Keys, Get Text, Get Attribute, Get Property, Get Tag Name, Get Rect, Is Displayed, Is Enabled, Is Selected | Done |
| **Windows** | Get Handle, Get All Handles, Get/Set Rect, Close, Fullscreen, Minimize, Maximize | Done |
| **Scripts** | Execute Sync, Execute Async | Done |
| **Navigation** | Get URL, Get Title, Navigate, Back, Forward, Refresh | Done |
| **Screenshots** | Take Screenshot, Element Screenshot | Done |
| **Cookies** | Get All, Get Named, Add, Delete, Delete All | Done |
| **Actions** | Key (keyDown/keyUp), Pointer (move/down/up), Wheel (scroll) | Done |

## MCP Integration

`tauri-webdriver` works with [mcp-tauri-automation](https://github.com/Radek44/mcp-tauri-automation) to enable AI-driven automation of Tauri apps via the [Model Context Protocol](https://modelcontextprotocol.io/). This lets AI agents (like Claude Code) launch, inspect, interact with, and screenshot your Tauri app.

```sh
# 1. Start tauri-webdriver
tauri-webdriver --port 4444

# 2. Configure mcp-tauri-automation in your MCP client
# The MCP server connects to tauri-webdriver on port 4444
# and exposes tools like launch_app, click_element, capture_screenshot, etc.
```

## Architecture

See [SPEC.md](SPEC.md) for the complete technical specification.

## License

MIT OR Apache-2.0 (dual-licensed, same as Tauri)
