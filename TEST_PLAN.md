# Test Plan: tauri-webdriver

## Testing Strategy

Three levels of testing ensure correctness and compatibility:

1. **Unit tests** -- Rust `#[test]` functions within each crate
2. **Integration tests** -- End-to-end tests using a minimal Tauri test app
3. **Compatibility tests** -- Verify WDIO and Selenium work correctly

---

## Test App

A minimal Tauri app (`tests/test-app/`) used by integration tests:

```html
<!-- Simple page with testable elements -->
<div id="root">
  <h1 id="title">Test App</h1>
  <p id="counter">Count: 0</p>
  <button id="increment">Increment</button>
  <input id="text-input" type="text" placeholder="Type here" />
  <a id="link" href="/page2">Go to page 2</a>
  <select id="dropdown">
    <option value="a">Option A</option>
    <option value="b" selected>Option B</option>
  </select>
  <div id="hidden" style="display:none">Hidden content</div>
</div>
```

With JavaScript:
```js
document.getElementById('increment').addEventListener('click', () => {
  const counter = document.getElementById('counter');
  const count = parseInt(counter.textContent.split(': ')[1]) + 1;
  counter.textContent = `Count: ${count}`;
});
```

---

## Unit Tests

### tauri-plugin-webdriver

| Test | Description | Acceptance Criteria |
|------|-------------|-------------------|
| `test_init_js_injection` | Verify init.js defines `__WEBDRIVER__` global | `__WEBDRIVER__` object exists with `resolve`, `findElement`, `cache` |
| `test_find_element_css` | findElement with CSS selector returns correct element | Returns element at correct index |
| `test_find_element_not_found` | findElement with non-matching selector | Returns appropriate error |
| `test_element_cache` | Cached elements are returned on repeat access | Second call doesn't re-query DOM |
| `test_server_binds_localhost` | HTTP server only binds to 127.0.0.1 | Connection from non-localhost rejected |
| `test_port_stdout_format` | Port output matches expected format | `[webdriver] listening on port {N}` |

### tauri-webdriver (CLI)

| Test | Description | Acceptance Criteria |
|------|-------------|-------------------|
| `test_parse_capabilities` | Parse `tauri:options` from session request | Binary path extracted correctly |
| `test_element_id_mapping` | UUID ↔ (selector, index) roundtrip | Map and retrieve correctly |
| `test_w3c_error_format` | Error responses match W3C spec | Correct JSON structure and error codes |
| `test_w3c_element_format` | Element refs use W3C identifier key | Key is `element-6066-11e4-a52e-4f735466cecf` |
| `test_session_id_format` | Session IDs are valid UUIDs | UUIDv4 format |
| `test_status_endpoint` | GET /status returns correct format | `{"value":{"ready":true,...}}` |
| `test_port_discovery` | Parse port from app stdout | Correctly extracts port number |
| `test_capability_matching` | `alwaysMatch` + `firstMatch` processing | Correct capability merging per W3C spec |

---

## Integration Tests

These tests launch the real test app and exercise the full stack.

### Phase 1: Session + Window

| Test | W3C Endpoints Exercised | Steps | Pass Criteria |
|------|------------------------|-------|--------------|
| `test_create_delete_session` | POST /session, DELETE /session/{id} | Create session, verify ID, delete | Session created and cleaned up, app process terminated |
| `test_status` | GET /status | Check status before/after session | `ready: true` when idle, server info present |
| `test_get_window_handle` | GET /session/{id}/window | Create session, get handle | Returns `"main"` |
| `test_get_window_handles` | GET /session/{id}/window/handles | Get all handles | Returns `["main"]` |
| `test_get_window_rect` | GET /session/{id}/window/rect | Get rect | Returns x, y, width, height (all numbers) |
| `test_set_window_rect` | POST /session/{id}/window/rect | Set to 1024x768, read back | Rect matches within 2px tolerance |
| `test_maximize_window` | POST /session/{id}/window/maximize | Maximize, check rect | Rect equals screen work area |
| `test_minimize_window` | POST /session/{id}/window/minimize | Minimize | Returns success |
| `test_fullscreen_window` | POST /session/{id}/window/fullscreen | Fullscreen, check rect | Rect equals full screen dimensions |

### Phase 2: Elements

| Test | W3C Endpoints Exercised | Steps | Pass Criteria |
|------|------------------------|-------|--------------|
| `test_find_element_by_id` | POST /session/{id}/element | Find `#title` | Returns element ref |
| `test_find_element_not_found` | POST /session/{id}/element | Find `#nonexistent` | Returns `no such element` error |
| `test_find_elements` | POST /session/{id}/elements | Find all `option` elements | Returns array of 2 element refs |
| `test_find_elements_empty` | POST /session/{id}/elements | Find `.nonexistent` | Returns empty array (not error) |
| `test_get_text` | GET /session/{id}/element/{eid}/text | Get text of `#title` | Returns `"Test App"` |
| `test_get_tag_name` | GET /session/{id}/element/{eid}/name | Get tag of `#title` | Returns `"h1"` (lowercase) |
| `test_get_attribute` | GET /session/{id}/element/{eid}/attribute/id | Get `id` attr of title | Returns `"title"` |
| `test_get_attribute_missing` | GET /session/{id}/element/{eid}/attribute/data-foo | Get non-existent attr | Returns `null` |
| `test_get_property` | GET /session/{id}/element/{eid}/property/tagName | Get `tagName` property | Returns `"H1"` |
| `test_click` | POST /session/{id}/element/{eid}/click | Click `#increment`, read `#counter` | Counter text changes to `"Count: 1"` |
| `test_click_multiple` | POST /session/{id}/element/{eid}/click (x3) | Click 3 times, read counter | Counter text is `"Count: 3"` |
| `test_send_keys` | POST /session/{id}/element/{eid}/value | Send "hello" to `#text-input` | Input value is `"hello"` |
| `test_clear` | POST /session/{id}/element/{eid}/clear | Type then clear `#text-input` | Input value is `""` |
| `test_is_displayed_visible` | GET /session/{id}/element/{eid}/displayed | Check `#title` | Returns `true` |
| `test_is_displayed_hidden` | GET /session/{id}/element/{eid}/displayed | Check `#hidden` | Returns `false` |
| `test_is_enabled` | GET /session/{id}/element/{eid}/enabled | Check `#increment` button | Returns `true` |
| `test_is_selected` | GET /session/{id}/element/{eid}/selected | Check selected option | Returns `true` for option B |
| `test_element_rect` | GET /session/{id}/element/{eid}/rect | Get rect of `#title` | Returns x, y, width, height (all >= 0) |
| `test_stale_element` | Navigate away, use old element ref | Find element, navigate, use ref | Returns `stale element reference` error |

### Phase 3: Scripts, Navigation, Screenshots

| Test | W3C Endpoints Exercised | Steps | Pass Criteria |
|------|------------------------|-------|--------------|
| `test_execute_sync_return` | POST /session/{id}/execute/sync | Execute `return 1 + 1` | Returns `2` |
| `test_execute_sync_dom` | POST /session/{id}/execute/sync | Execute `return document.title` | Returns page title |
| `test_execute_sync_args` | POST /session/{id}/execute/sync | Execute with args | Args accessible, result correct |
| `test_execute_sync_element_arg` | POST /session/{id}/execute/sync | Pass element ref as arg | Element resolved in script |
| `test_execute_sync_error` | POST /session/{id}/execute/sync | Execute `throw new Error('test')` | Returns `javascript error` |
| `test_get_title` | GET /session/{id}/title | Get page title | Returns expected title string |
| `test_get_url` | GET /session/{id}/url | Get current URL | Returns valid URL |
| `test_screenshot` | GET /session/{id}/screenshot | Take screenshot | Returns valid base64 PNG |
| `test_element_screenshot` | GET /session/{id}/element/{eid}/screenshot | Screenshot of `#title` | Returns valid base64 PNG, smaller than full page |

---

## Compatibility Tests

### WDIO Compatibility

| Test File | Description | Target |
|-----------|-------------|--------|
| `smoke.spec.mjs` | Basic element operations | All pass |
| `element.spec.mjs` | Comprehensive element tests | All pass |
| `window.spec.mjs` | Window management | All pass |
| `script.spec.mjs` | Script execution | All pass |
| `navigation.spec.mjs` | URL/title/navigation | All pass |
| `screenshot.spec.mjs` | Screenshot capture | All pass |

### Regression Checklist

Before each release, verify:

- [ ] `browser.getWindowHandle()` returns `"main"`
- [ ] `browser.$('#root')` finds the root element
- [ ] `browser.$$('button')` finds all buttons
- [ ] `browser.$('#increment').click()` triggers click handler
- [ ] `browser.$('#counter').getText()` returns updated text
- [ ] `browser.$('#title').getTagName()` returns `"h1"`
- [ ] `browser.execute('return 1+1')` returns `2`
- [ ] `browser.deleteSession()` terminates the app process
- [ ] No orphaned processes after test completion
- [ ] Server exits cleanly on SIGTERM/SIGINT

---

## Performance Targets

| Operation | Target Latency | Notes |
|-----------|---------------|-------|
| Session creation | < 5s | Includes app launch |
| Find element | < 100ms | CSS selector query |
| Click element | < 50ms | DOM click dispatch |
| Get text | < 50ms | Text content read |
| Execute script | < 100ms | Simple JS eval |
| Screenshot | < 500ms | Full page capture |
| Session deletion | < 2s | Includes app shutdown |

---

## CI Pipeline

```yaml
# .github/workflows/test.yml
on: [push, pull_request]
jobs:
  test:
    runs-on: macos-latest
    steps:
      - Build both crates
      - Build test app
      - Run unit tests
      - Run integration tests
      - Run WDIO compatibility tests
```

Note: macOS runner required because WKWebView is macOS-only.
