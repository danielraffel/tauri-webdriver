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
echo "=== Screenshots ==="
run_test "Full page screenshot" "/screenshot" "{}" '"data"'
run_test "Element screenshot (#title)" "/screenshot/element" '{"selector":"#title","index":0}' '"data"'

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
