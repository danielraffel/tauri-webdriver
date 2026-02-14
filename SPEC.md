# Technical Specification: tauri-webdriver

## Overview

`tauri-webdriver` is an open-source W3C WebDriver implementation for Tauri desktop applications on macOS. It consists of two Rust crates that work together to enable automated end-to-end testing.

### Design Principles

1. **No closed-source dependencies.** Everything is open source, MIT/Apache-2.0.
2. **Standard protocols only.** W3C WebDriver on the test side, plain HTTP internally.
3. **Minimal footprint.** The plugin adds zero runtime overhead outside of test builds.
4. **Complete feature set.** Tag name, XPath selectors, screenshots, shadow DOM, frames, and DOM snapshots all work correctly.

---

## Crate 1: `tauri-plugin-webdriver-automation`

A Tauri plugin that exposes an HTTP API for interacting with the app's webview.

### Responsibilities

- Start a local HTTP server on a dynamic port during plugin setup
- Communicate the port to the external WebDriver server
- Handle DOM interaction via JavaScript evaluation in the webview
- Handle window management via Tauri's window APIs
- Provide element finding, interaction, and property access
- Track frame/iframe navigation state for scoped JS evaluation
- Support shadow DOM element lookup via an in-memory cache

### Plugin Lifecycle

```
App starts (debug build)
  → Plugin::setup() runs
    → Spawn HTTP server on 127.0.0.1:{random_port}
    → Write port to stdout: "[webdriver] listening on port {port}"
    → Inject init.js into all webviews
  → Plugin::on_webview_ready() fires
    → Notify HTTP server that a webview is available
```

### HTTP API

All endpoints use `POST` with JSON bodies and return JSON responses.
Server binds to `127.0.0.1` only (localhost, not exposed to network).

#### Window Operations

| Endpoint | Request Body | Response | Description |
|----------|-------------|----------|-------------|
| `POST /window/handle` | `{}` | `"main"` | Get current window handle |
| `POST /window/handles` | `{}` | `["main"]` | Get all window handles |
| `POST /window/close` | `{"label": "main"}` | `true` | Close a window |
| `POST /window/rect` | `{"label": "main"}` | `{"x":0,"y":0,"width":800,"height":600}` | Get window rect |
| `POST /window/set-rect` | `{"x":0,"y":0,"width":1024,"height":768}` | `true` | Set window position/size |
| `POST /window/set-current` | `{"label": "main"}` | `true` | Switch to a window by label |
| `POST /window/fullscreen` | `{}` | `true` | Make window fullscreen |
| `POST /window/minimize` | `{}` | `true` | Minimize window |
| `POST /window/maximize` | `{}` | `true` | Maximize window |
| `POST /window/insets` | `{}` | `{"top":28,"bottom":0,"x":0,"y":28}` | Get safe area insets (macOS) |

#### Element Operations

| Endpoint | Request Body | Response | Description |
|----------|-------------|----------|-------------|
| `POST /element/find` | `{"using":"css","value":"#root"}` | `{"elements":[{"selector":"#root","index":0}]}` | Find matching elements |
| `POST /element/find-from` | `{"parent_selector":"#list","parent_index":0,"using":"css","value":"li"}` | `{"elements":[...]}` | Find elements scoped to a parent |
| `POST /element/text` | `{"selector":"#root","index":0}` | `{"text":"Hello"}` | Get element text content |
| `POST /element/attribute` | `{"selector":"#root","index":0,"name":"class"}` | `{"value":"container"}` | Get element attribute |
| `POST /element/property` | `{"selector":"#root","index":0,"name":"checked"}` | `{"value":true}` | Get element JS property |
| `POST /element/tag` | `{"selector":"#root","index":0}` | `{"tag":"div"}` | Get element tag name |
| `POST /element/rect` | `{"selector":"#root","index":0}` | `{"x":0,"y":0,"width":100,"height":50}` | Get element bounding rect |
| `POST /element/click` | `{"selector":"button","index":0}` | `null` | Click an element |
| `POST /element/clear` | `{"selector":"input","index":0}` | `null` | Clear an input element |
| `POST /element/send-keys` | `{"selector":"input","index":0,"text":"hello"}` | `null` | Type into an element |
| `POST /element/displayed` | `{"selector":"#root","index":0}` | `{"displayed":true}` | Check if element is visible |
| `POST /element/enabled` | `{"selector":"button","index":0}` | `{"enabled":true}` | Check if element is enabled |
| `POST /element/selected` | `{"selector":"option","index":0}` | `{"selected":false}` | Check if element is selected |
| `POST /element/active` | `{}` | `{"element":{"selector":"[data-wd-id=\"...\"]","index":0}}` | Get the focused element |
| `POST /element/computed-role` | `{"selector":"button","index":0}` | `{"role":"button"}` | Get computed ARIA role |
| `POST /element/computed-label` | `{"selector":"input","index":0}` | `{"label":"Enter text"}` | Get computed ARIA label |

#### Shadow DOM

| Endpoint | Request Body | Response | Description |
|----------|-------------|----------|-------------|
| `POST /element/shadow` | `{"selector":"#host","index":0}` | `{"hasShadow":true}` | Check if element has a shadow root |
| `POST /shadow/find` | `{"host_selector":"#host","host_index":0,"using":"css","value":".inner"}` | `{"elements":[...]}` | Find elements inside a shadow root |

#### Frame / iframe

| Endpoint | Request Body | Response | Description |
|----------|-------------|----------|-------------|
| `POST /frame/switch` | `{"id":0}` | `null` | Switch to frame by index |
| `POST /frame/switch` | `{"id":null}` | `null` | Switch to top-level document |
| `POST /frame/switch` | `{"id":{"selector":"iframe","index":0}}` | `null` | Switch to frame by element |
| `POST /frame/parent` | `{}` | `null` | Switch to parent frame |

#### Script Execution

| Endpoint | Request Body | Response | Description |
|----------|-------------|----------|-------------|
| `POST /script/execute` | `{"script":"return 1+1","args":[]}` | `{"value":2}` | Execute sync JavaScript |
| `POST /script/execute-async` | `{"script":"...","args":[]}` | `{"value":...}` | Execute async JavaScript |

#### Navigation

| Endpoint | Request Body | Response | Description |
|----------|-------------|----------|-------------|
| `POST /navigate/url` | `{"url":"http://..."}` | `null` | Navigate to URL |
| `POST /navigate/current` | `{}` | `{"url":"http://..."}` | Get current URL |
| `POST /navigate/title` | `{}` | `{"title":"My App"}` | Get page title |
| `POST /navigate/back` | `{}` | `null` | Go back |
| `POST /navigate/forward` | `{}` | `null` | Go forward |
| `POST /navigate/refresh` | `{}` | `null` | Refresh page |

#### Page Source

| Endpoint | Request Body | Response | Description |
|----------|-------------|----------|-------------|
| `POST /source` | `{}` | `{"source":"<html>..."}` | Get full page HTML source |

#### Screenshots

| Endpoint | Request Body | Response | Description |
|----------|-------------|----------|-------------|
| `POST /screenshot` | `{}` | `{"data":"base64..."}` | Full page screenshot |
| `POST /screenshot/element` | `{"selector":"#root","index":0}` | `{"data":"base64..."}` | Element screenshot |

#### Cookies

| Endpoint | Request Body | Response | Description |
|----------|-------------|----------|-------------|
| `POST /cookie/get-all` | `{}` | `{"cookies":[...]}` | Get all cookies |
| `POST /cookie/get` | `{"name":"session"}` | `{"cookie":{...}}` | Get cookie by name |
| `POST /cookie/add` | `{"cookie":{"name":"k","value":"v","path":"/"}}` | `null` | Add a cookie |
| `POST /cookie/delete` | `{"name":"session"}` | `null` | Delete cookie by name |
| `POST /cookie/delete-all` | `{}` | `null` | Delete all cookies |

### JavaScript Bridge (`init.js`)

Injected into every webview on creation. Provides:

```js
window.__WEBDRIVER__ = {
    // Resolve an async script evaluation
    resolve(id, result),

    // Find a DOM element by CSS selector and position index
    findElement(selector, index),

    // Find a DOM element by XPath expression
    findElementByXPath(xpath),

    // Retrieve an element stored in the shadow DOM cache
    findElementInShadow(id),

    // Get the currently focused element
    getActiveElement(),

    // Element cache for performance
    cache: {},

    // In-memory cookie store (tauri:// scheme compatibility)
    cookies: {},

    // Shadow DOM element cache (direct references to shadow-internal elements)
    __shadowCache: {}
};
```

**Element Identity Model:**

Elements are identified by `(css_selector, index)` pairs. When `findElement("button", 2)` is called:
1. Execute `document.querySelectorAll("button")`
2. Return the element at index 2
3. The W3C layer maps this to a stable UUID for the session lifetime

This is simple, stateless, and avoids stale element references across page navigations.

**Shadow DOM Elements:**

Elements inside shadow roots cannot be found via `document.querySelectorAll()`. Instead, they are cached directly in `__shadowCache` keyed by a generated ID. The `using: "shadow"` locator type signals that the element should be resolved from the cache rather than the DOM.

**Frame Context:**

When the frame stack is non-empty, all JS evaluation is wrapped to navigate the iframe hierarchy via `contentDocument` access. The target frame's document is passed as a parameter to the script function, shadowing the global `document` reference without triggering JavaScript hoisting issues.

### Port Communication

The plugin communicates its HTTP port via stdout:
```
[webdriver] listening on port 15087
```

The CLI binary watches the app's stdout for this line to discover the port.

### Dependencies

- `axum` -- HTTP server
- `tokio` -- Async runtime (already required by Tauri)
- `serde` / `serde_json` -- JSON serialization
- `uuid` -- Script evaluation IDs
- `tauri` -- Plugin API

No `libloading`, no FFI, no external binaries.

---

## Crate 2: `tauri-webdriver-automation`

A standalone CLI binary (`tauri-wd`) implementing the W3C WebDriver protocol.

### Responsibilities

- Implement the W3C WebDriver HTTP protocol on port 4444
- Launch and manage the Tauri app process
- Discover the plugin's HTTP port from app stdout
- Translate W3C WebDriver requests to plugin HTTP API calls
- Manage element state (W3C element IDs ↔ selector/index pairs)
- Manage shadow root references (W3C shadow IDs ↔ host element info)
- Handle session lifecycle (create, delete, timeouts)

### W3C WebDriver Endpoints

Implements the [W3C WebDriver specification](https://www.w3.org/TR/webdriver2/):

#### Session

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/status` | GET | Server status |
| `/session` | POST | Create new session |
| `/session/{id}` | DELETE | Delete session |
| `/session/{id}/timeouts` | GET/POST | Get/set timeouts |

#### Navigation

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/session/{id}/url` | GET/POST | Get/set current URL |
| `/session/{id}/title` | GET | Get page title |
| `/session/{id}/source` | GET | Get page source |
| `/session/{id}/back` | POST | Navigate back |
| `/session/{id}/forward` | POST | Navigate forward |
| `/session/{id}/refresh` | POST | Refresh page |

#### Window

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/session/{id}/window` | GET/POST/DELETE | Get handle / Switch / Close |
| `/session/{id}/window/handles` | GET | Get all handles |
| `/session/{id}/window/rect` | GET/POST | Get/set rect |
| `/session/{id}/window/maximize` | POST | Maximize |
| `/session/{id}/window/minimize` | POST | Minimize |
| `/session/{id}/window/fullscreen` | POST | Fullscreen |

#### Elements

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/session/{id}/element` | POST | Find element |
| `/session/{id}/elements` | POST | Find elements |
| `/session/{id}/element/active` | GET | Get active (focused) element |
| `/session/{id}/element/{eid}/element` | POST | Find element from element |
| `/session/{id}/element/{eid}/elements` | POST | Find elements from element |
| `/session/{id}/element/{eid}/click` | POST | Click |
| `/session/{id}/element/{eid}/clear` | POST | Clear |
| `/session/{id}/element/{eid}/value` | POST | Send keys |
| `/session/{id}/element/{eid}/text` | GET | Get text |
| `/session/{id}/element/{eid}/name` | GET | Get tag name |
| `/session/{id}/element/{eid}/attribute/{name}` | GET | Get attribute |
| `/session/{id}/element/{eid}/property/{name}` | GET | Get property |
| `/session/{id}/element/{eid}/css/{name}` | GET | Get CSS value |
| `/session/{id}/element/{eid}/rect` | GET | Get rect |
| `/session/{id}/element/{eid}/enabled` | GET | Is enabled |
| `/session/{id}/element/{eid}/selected` | GET | Is selected |
| `/session/{id}/element/{eid}/displayed` | GET | Is displayed (non-standard, widely used) |
| `/session/{id}/element/{eid}/computedrole` | GET | Computed ARIA role |
| `/session/{id}/element/{eid}/computedlabel` | GET | Computed ARIA label |

#### Shadow DOM

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/session/{id}/element/{eid}/shadow` | GET | Get shadow root |
| `/session/{id}/shadow/{sid}/element` | POST | Find element in shadow root |
| `/session/{id}/shadow/{sid}/elements` | POST | Find elements in shadow root |

#### Frames

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/session/{id}/frame` | POST | Switch to frame (by index, element, or null for top) |
| `/session/{id}/frame/parent` | POST | Switch to parent frame |

#### Script Execution

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/session/{id}/execute/sync` | POST | Execute sync script |
| `/session/{id}/execute/async` | POST | Execute async script |

#### Screenshots

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/session/{id}/screenshot` | GET | Full page screenshot |
| `/session/{id}/element/{eid}/screenshot` | GET | Element screenshot |

#### Cookies

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/session/{id}/cookie` | GET | Get all cookies |
| `/session/{id}/cookie/{name}` | GET | Get cookie by name |
| `/session/{id}/cookie` | POST | Add cookie |
| `/session/{id}/cookie/{name}` | DELETE | Delete cookie by name |
| `/session/{id}/cookie` | DELETE | Delete all cookies |

#### Actions

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/session/{id}/actions` | POST | Perform actions (key, pointer, wheel) |
| `/session/{id}/actions` | DELETE | Release actions |

### Session Creation Flow

```
1. Client sends POST /session with capabilities:
   {
     "capabilities": {
       "alwaysMatch": {
         "tauri:options": {
           "binary": "/path/to/my-app"
         }
       }
     }
   }

2. Server extracts binary path from tauri:options

3. Server launches the binary with env vars:
   TAURI_WEBVIEW_AUTOMATION=true

4. Server watches stdout for:
   [webdriver] listening on port {N}

5. Server connects to plugin HTTP API at 127.0.0.1:{N}

6. Server responds with session ID and capabilities
```

### Element State Management

The W3C spec requires elements to have stable string IDs within a session. The CLI maps these:

```
W3C Element ID (UUID)  ←→  (css_selector: String, index: usize, using: String)
```

When WDIO sends `POST /session/{id}/element` with `{"using":"css selector","value":"button"}`:
1. CLI calls plugin `POST /element/find {"using":"css","value":"button"}`
2. Plugin returns `{"elements":[{"selector":"button","index":0},{"selector":"button","index":1}]}`
3. CLI generates UUIDs for each, stores the mapping
4. CLI returns W3C format: `{"value":{"element-6066-11e4-a52e-4f735466cecf":"uuid-here"}}`

Subsequent operations on that element UUID are resolved back to (selector, index, using) and forwarded to the plugin.

**Shadow root references** follow a similar pattern using `shadow-6066-11e4-a52e-4f735466cecf` as the key. Each shadow ref stores the host element's selector, index, and using type.

### CLI Interface

```
tauri-wd [OPTIONS]

Options:
  --port <PORT>       WebDriver server port [default: 4444]
  --host <HOST>       WebDriver server host [default: 127.0.0.1]
  --log-level <LEVEL> Log level: error, warn, info, debug, trace [default: info]
  --version           Print version
  --help              Print help
```

### Dependencies

- `axum` -- HTTP server (W3C WebDriver protocol)
- `reqwest` -- HTTP client (plugin API calls)
- `tokio` -- Async runtime
- `serde` / `serde_json` -- JSON handling
- `uuid` -- Session and element IDs
- `clap` -- CLI argument parsing
- `tracing` / `tracing-subscriber` -- Structured logging

### Error Handling

All errors follow the W3C WebDriver error format:

```json
{
  "value": {
    "error": "no such element",
    "message": "Element not found with selector: #nonexistent",
    "stacktrace": ""
  }
}
```

Standard W3C error codes:
- `session not created` -- Failed to create session
- `invalid argument` -- Bad request parameters
- `no such element` -- Element not found
- `no such shadow root` -- Shadow root not found or element has no shadow root
- `stale element reference` -- Element no longer exists
- `no such frame` -- Frame not found
- `no such window` -- Window not found
- `javascript error` -- Script execution error
- `unknown error` -- Internal server error
- `timeout` -- Operation timed out

---

## Implementation Phases

### Phase 1: Plugin + Session Lifecycle ✓
**Goal:** Plugin HTTP server works. CLI can create/delete sessions and get window handles.

- [x] `tauri-plugin-webdriver-automation` crate with HTTP server
- [x] Window endpoints: handle, handles, rect, fullscreen, minimize, maximize
- [x] Port communication via stdout
- [x] `tauri-webdriver-automation` CLI with session create/delete
- [x] App launch with env vars
- [x] Port discovery from stdout
- [x] GET /status, GET /session/{id}/window
- [x] Integration test: create session, get window handle, delete session

### Phase 2: Element Operations ✓
**Goal:** Find elements, click them, read their properties.

- [x] Plugin: /element/find, /element/click, /element/text, /element/tag, /element/attribute
- [x] Plugin: init.js with findElement and cache
- [x] CLI: POST /session/{id}/element, /elements
- [x] CLI: Element ID mapping (UUID ↔ selector/index)
- [x] CLI: Click, text, tag name, attribute, property, rect
- [x] W3C error responses for missing elements
- [x] Integration test: find element, click, verify text change

### Phase 3: Scripts, Navigation, Screenshots ✓
**Goal:** Full test capability.

- [x] Plugin: /script/execute, /navigate/*, /screenshot
- [x] CLI: Execute sync/async scripts
- [x] CLI: Navigate to/back/forward/refresh, get URL/title
- [x] CLI: Take screenshot (base64 PNG via SVG foreignObject + Canvas)
- [x] CSS selector + XPath support
- [x] Integration test: navigate, execute script, take screenshot

### Phase 4: Robustness + Compatibility ✓
**Goal:** Production-quality. Drop-in replacement for existing setups.

- [x] Configurable timeouts (W3C GET/POST /session/{id}/timeouts)
- [x] Send keys / clear
- [x] Cookie operations (in-memory store for `tauri://` scheme compatibility)
- [x] Perform actions (pointer/keyboard/wheel via JS event dispatch)
- [x] Graceful shutdown + process cleanup (SIGINT/SIGTERM handling)
- [x] W3C compliance test suite
- [x] WDIO compatibility test suite
- [x] Structured logging
- [x] CI pipeline (build, test, release)
- [x] Published to crates.io

### Phase 5: Extended W3C Coverage ✓
**Goal:** Full W3C endpoint coverage for real-world test frameworks.

- [x] Page source (`GET /session/{id}/source`)
- [x] Active element (`GET /session/{id}/element/active`)
- [x] Find element from element (`POST /session/{id}/element/{eid}/element`)
- [x] Switch to window (`POST /session/{id}/window`)
- [x] Shadow DOM (get shadow root, find in shadow)
- [x] Frame / iframe support (switch to frame, switch to parent)
- [x] Computed ARIA role and label

---

## Compatibility Targets

- **Tauri:** v2.x
- **Rust:** 1.86+ (edition 2024)
- **macOS:** 13+ (Ventura and later)
- **WDIO:** v9.x
- **Selenium:** v4.x (W3C protocol)

## Key Features

- Correct tag name support via `getTagName()`
- XPath selectors via `document.evaluate()`
- Shadow DOM element access via in-memory cache
- Frame / iframe navigation with scoped JS evaluation
- Computed ARIA role and label
- Full page + element screenshots
- Fully open source (MIT/Apache-2.0)
- No cloud dependencies or external accounts required
- Direct 2-hop architecture (CLI → plugin)
- Documented and tested on macOS

---

## Future Considerations

Ideas for future development, roughly ordered by impact.

### Multi-session support
Currently the CLI supports a single active session. Supporting multiple concurrent sessions would allow parallel test execution. Each session would need its own app process, plugin port, and element map.

### Native screenshot via `CGWindowListCreateImage`
The current screenshot approach (SVG foreignObject → Canvas) cannot capture content outside the DOM (native title bars, system dialogs, CSS `backdrop-filter` effects). A native macOS screenshot using `CGWindowListCreateImage` via Tauri's Objective-C bridge would produce pixel-accurate captures.

### Alert / dialog handling
W3C endpoints for `Accept Alert`, `Dismiss Alert`, `Get Alert Text`, `Send Alert Text`. Requires intercepting `window.alert()`, `window.confirm()`, `window.prompt()` via JS injection before the page loads.

### File upload support
W3C `Element Send Keys` on `<input type="file">` should trigger a file upload. This requires special handling since setting `.value` on file inputs is blocked by browsers for security.

### Persistent cookies via `WKHTTPCookieStore`
The current in-memory cookie store works for testing but doesn't survive page navigations that clear JS state. Using WKWebView's native `WKHTTPCookieStore` API via Tauri's Objective-C bridge would provide persistent, spec-compliant cookie behavior.

### Linux / Windows support
The plugin and CLI are platform-agnostic Rust, but testing has only been done on macOS. Linux (WebKitGTK) and Windows (WebView2) use different webview engines. Screenshots and window insets may need platform-specific adjustments.

### Multi-window / multi-webview support
The plugin resolves windows by label (defaulting to `"main"`). Basic `Switch To Window` is implemented. Full multi-webview support for complex Tauri apps with multiple webview windows would extend the current single-label tracking.
