#!/bin/bash
set -e

APP_BIN="/Users/danielraffel/Code/tauri-webdriver/tests/test-app/src-tauri/target/debug/webdriver-test-app"
PORT_FILE="/tmp/tauri-webdriver-test-port"
RESULT_FILE="/tmp/tauri-webdriver-test-results"

rm -f "$PORT_FILE" "$RESULT_FILE"

# Launch app, capture port from stdout
$APP_BIN 2>/dev/null | while IFS= read -r line; do
  echo "$line"
  if echo "$line" | grep -q '^\[webdriver\] listening on port'; then
    PORT=$(echo "$line" | sed 's/.*port //')
    echo "$PORT" > "$PORT_FILE"
    break
  fi
done &
APP_PID=$!

# Wait for port file
for i in $(seq 1 20); do
  if [ -f "$PORT_FILE" ]; then
    break
  fi
  sleep 0.5
done

if [ ! -f "$PORT_FILE" ]; then
  echo "FAIL: Timed out waiting for plugin port"
  kill $APP_PID 2>/dev/null; wait $APP_PID 2>/dev/null
  exit 1
fi

PORT=$(cat "$PORT_FILE")
echo "Plugin server on port $PORT"
echo ""

PASS=0
FAIL=0

run_test() {
  local name="$1"
  local endpoint="$2"
  local body="$3"
  local expected="$4"

  result=$(curl -s -m 5 -X POST "http://127.0.0.1:$PORT$endpoint" \
    -H 'Content-Type: application/json' -d "$body" 2>&1)

  if echo "$result" | grep -q "$expected"; then
    echo "PASS: $name"
    echo "      -> $result"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name"
    echo "      Expected to contain: $expected"
    echo "      Got: $result"
    FAIL=$((FAIL + 1))
  fi
}

echo "=== Window Operations ==="
run_test "GET window handle" "/window/handle" "{}" '"main"'
run_test "GET window handles" "/window/handles" "{}" '"main"'
run_test "GET window rect" "/window/rect" "{}" '"width"'
run_test "GET window insets" "/window/insets" "{}" '"top"'

echo ""
echo "=== Switch To Window ==="
run_test "Switch to main window" "/window/set-current" '{"label":"main"}' 'true'

echo ""
echo "=== Element Finding ==="
run_test "Find element by CSS (#title)" "/element/find" '{"using":"css","value":"#title"}' '"elements"'
run_test "Find multiple elements (option)" "/element/find" '{"using":"css","value":"option"}' '"index":1'
run_test "Find element by XPath (//h1)" "/element/find" '{"using":"xpath","value":"//h1"}' '"xpath"'
run_test "Find no elements (.nonexistent)" "/element/find" '{"using":"css","value":".nonexistent"}' '"elements"'

echo ""
echo "=== Element Properties ==="
run_test "Get element text (#title)" "/element/text" '{"selector":"#title","index":0}' '"Test App"'
run_test "Get tag name (#title)" "/element/tag" '{"selector":"#title","index":0}' '"h1"'
run_test "Get attribute (id)" "/element/attribute" '{"selector":"#title","index":0,"name":"id"}' '"title"'
run_test "Get property (tagName)" "/element/property" '{"selector":"#title","index":0,"name":"tagName"}' '"H1"'
run_test "Element rect (#title)" "/element/rect" '{"selector":"#title","index":0}' '"width"'

echo ""
echo "=== Element State ==="
run_test "Is displayed (visible)" "/element/displayed" '{"selector":"#title","index":0}' '"displayed":true'
run_test "Is displayed (hidden)" "/element/displayed" '{"selector":"#hidden","index":0}' '"displayed":false'
run_test "Is enabled (button)" "/element/enabled" '{"selector":"#increment","index":0}' '"enabled":true'
run_test "Is selected (option B)" "/element/selected" '{"selector":"option","index":1}' '"selected"'

echo ""
echo "=== Element Interaction ==="
run_test "Click increment button" "/element/click" '{"selector":"#increment","index":0}' 'null'
# Small delay for DOM update
sleep 0.2
run_test "Counter after 1 click" "/element/text" '{"selector":"#counter","index":0}' '"Count: 1"'
run_test "Click increment again" "/element/click" '{"selector":"#increment","index":0}' 'null'
sleep 0.2
run_test "Counter after 2 clicks" "/element/text" '{"selector":"#counter","index":0}' '"Count: 2"'

echo ""
echo "=== Script Execution ==="
run_test "Execute sync (1+1)" "/script/execute" '{"script":"return 1+1","args":[]}' '"value":2'
run_test "Execute sync (document.title)" "/script/execute" '{"script":"return document.title","args":[]}' '"WebDriver Test App"'
run_test "Execute async (callback)" "/script/execute-async" '{"script":"var done=arguments[arguments.length-1];done(42)","args":[]}' '"value":42'

echo ""
echo "=== Navigation ==="
run_test "Get page title" "/navigate/title" "{}" '"WebDriver Test App"'
run_test "Get current URL" "/navigate/current" "{}" '"url"'

echo ""
echo "=== Page Source ==="
run_test "Get page source" "/source" "{}" '"<html'

echo ""
echo "=== Shadow DOM ==="
run_test "Check shadow root exists" "/element/shadow" '{"selector":"#shadow-host","index":0}' '"hasShadow":true'
run_test "Find in shadow root" "/shadow/find" '{"host_selector":"#shadow-host","host_index":0,"using":"css","value":".shadow-text"}' '"elements"'

echo ""
echo "=== Frames ==="
run_test "Switch to frame by index" "/frame/switch" '{"id":0}' 'null'
run_test "Find element in frame" "/element/find" '{"using":"css","value":"#frame-title"}' '"elements"'
run_test "Get text in frame (#frame-title)" "/element/text" '{"selector":"#frame-title","index":0}' '"Inside Frame"'
run_test "Switch to parent frame" "/frame/parent" '{}' 'null'
run_test "Find element after parent switch" "/element/find" '{"using":"css","value":"#title"}' '"elements"'
run_test "Get text after parent (#title)" "/element/text" '{"selector":"#title","index":0}' '"Test App"'
run_test "Switch to frame again" "/frame/switch" '{"id":0}' 'null'
run_test "Switch to top (null)" "/frame/switch" '{"id":null}' 'null'
run_test "Find element after top switch" "/element/find" '{"using":"css","value":"#title"}' '"elements"'

echo ""
echo "=== Find Element From Element ==="
run_test "Find options within dropdown" "/element/find-from" '{"parent_selector":"#dropdown","parent_index":0,"using":"css","value":"option"}' '"elements"'

echo ""
echo "=== Computed ARIA Role + Label ==="
run_test "Computed role of button" "/element/computed-role" '{"selector":"#increment","index":0}' '"button"'
run_test "Computed role of h1" "/element/computed-role" '{"selector":"#title","index":0}' '"heading"'
run_test "Computed label of text-input" "/element/computed-label" '{"selector":"#text-input","index":0}' '"Enter text"'

echo ""
echo "=== Active Element ==="
run_test "Click text-input to focus" "/element/click" '{"selector":"#text-input","index":0}' 'null'
sleep 0.2
run_test "Get active element" "/element/active" "{}" '"selector"'

echo ""
echo "=== New Window ==="
run_test "Create new window" "/window/new" '{}' '"handle"'
run_test "Verify window handles (2+)" "/window/handles" "{}" '"wd-'
# Switch back to main for remaining tests
run_test "Switch back to main" "/window/set-current" '{"label":"main"}' 'true'

echo ""
echo "=== Alert/Dialog Handling ==="
# Trigger alert via click, then test alert endpoints
run_test "Click trigger-alert button" "/element/click" '{"selector":"#trigger-alert","index":0}' 'null'
sleep 0.2
run_test "Get alert text" "/alert/text" '{}' '"Hello Alert"'
run_test "Dismiss alert" "/alert/dismiss" '{}' 'null'
# Verify no alert is open
run_test "Trigger confirm dialog" "/element/click" '{"selector":"#trigger-confirm","index":0}' 'null'
sleep 0.2
run_test "Get confirm text" "/alert/text" '{}' '"Are you sure?"'
run_test "Accept confirm" "/alert/accept" '{}' 'null'
# Trigger prompt, send text, accept
run_test "Trigger prompt dialog" "/element/click" '{"selector":"#trigger-prompt","index":0}' 'null'
sleep 0.2
run_test "Get prompt text" "/alert/text" '{}' '"Enter name"'
run_test "Send text to prompt" "/alert/send-text" '{"text":"Bob"}' 'null'
run_test "Accept prompt" "/alert/accept" '{}' 'null'

echo ""
echo "=== File Upload ==="
# Create a temporary test file
echo "hello world" > /tmp/tauri-webdriver-test-upload.txt
# Set files on the file input using base64 data
FILE_B64=$(base64 < /tmp/tauri-webdriver-test-upload.txt | tr -d '\n')
run_test "Set file on input" "/element/set-files" "{\"selector\":\"#file-input\",\"index\":0,\"files\":[{\"name\":\"test.txt\",\"data\":\"$FILE_B64\",\"mime\":\"text/plain\"}]}" 'null'
sleep 0.3
run_test "Verify file status text" "/element/text" '{"selector":"#file-status","index":0}' '"File: test.txt'
rm -f /tmp/tauri-webdriver-test-upload.txt

echo ""
echo "=== Screenshots ==="
run_test "Full page screenshot" "/screenshot" "{}" '"data"'
run_test "Element screenshot (#title)" "/screenshot/element" '{"selector":"#title","index":0}' '"data"'

echo ""
echo "=== Print to PDF ==="
run_test "Print page to PDF" "/print" '{}' '"data"'

echo ""
echo "=== Cookies ==="
run_test "Get all cookies (empty)" "/cookie/get-all" "{}" '"cookies"'
run_test "Add cookie" "/cookie/add" '{"cookie":{"name":"testcookie","value":"testvalue","path":"/"}}' 'null'
sleep 0.3
run_test "Get cookie by name" "/cookie/get" '{"name":"testcookie"}' '"testvalue"'
run_test "Get all cookies (has cookie)" "/cookie/get-all" "{}" '"testcookie"'
run_test "Delete cookie by name" "/cookie/delete" '{"name":"testcookie"}' 'null'
sleep 0.3
run_test "Get cookie after delete" "/cookie/get" '{"name":"testcookie"}' 'null'
run_test "Add cookie for delete-all" "/cookie/add" '{"cookie":{"name":"cookie1","value":"val1","path":"/"}}' 'null'
sleep 0.3
run_test "Delete all cookies" "/cookie/delete-all" "{}" 'null'
sleep 0.3
run_test "Get all after delete-all" "/cookie/get-all" "{}" '"cookies"'

echo ""
echo "=================================="
echo "Results: $PASS passed, $FAIL failed"
echo "=================================="

# Cleanup: kill the app and its children
pkill -f "webdriver-test-app" 2>/dev/null || true
rm -f "$PORT_FILE"

if [ $FAIL -gt 0 ]; then
  exit 1
fi
