#!/bin/bash
set -e

CLI_BIN="/Users/danielraffel/Code/tauri-webdriver/target/debug/tauri-webdriver"
APP_BIN="/Users/danielraffel/Code/tauri-webdriver/tests/test-app/src-tauri/target/debug/webdriver-test-app"
PORT=4444
BASE="http://127.0.0.1:$PORT"

PASS=0
FAIL=0
SESSION_ID=""

run_test() {
  local name="$1"
  local method="$2"
  local path="$3"
  local body="$4"
  local expected="$5"

  if [ "$method" = "GET" ]; then
    result=$(curl -s -m 10 "$BASE$path" 2>&1)
  elif [ "$method" = "DELETE" ]; then
    result=$(curl -s -m 10 -X DELETE "$BASE$path" 2>&1)
  else
    result=$(curl -s -m 10 -X POST "$BASE$path" \
      -H 'Content-Type: application/json' -d "$body" 2>&1)
  fi

  if echo "$result" | grep -q "$expected"; then
    echo "PASS: $name"
    echo "      -> $(echo "$result" | head -c 200)"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name"
    echo "      Expected to contain: $expected"
    echo "      Got: $(echo "$result" | head -c 300)"
    FAIL=$((FAIL + 1))
  fi

  # Return the result for parsing
  echo "$result" > /tmp/tauri-webdriver-last-result
}

extract_session_id() {
  SESSION_ID=$(cat /tmp/tauri-webdriver-last-result | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(d.get('value',{}).get('sessionId',''))
" 2>/dev/null)
}

extract_element_id() {
  local var_name="$1"
  local eid=$(cat /tmp/tauri-webdriver-last-result | python3 -c "
import json,sys
d=json.load(sys.stdin)
v=d.get('value',{})
# For single element
key='element-6066-11e4-a52e-4f735466cecf'
if key in v:
  print(v[key])
elif isinstance(v,list) and len(v)>0 and key in v[0]:
  print(v[0][key])
else:
  print('')
" 2>/dev/null)
  eval "$var_name='$eid'"
}

# Start CLI server in background
echo "Starting tauri-webdriver CLI on port $PORT..."
$CLI_BIN --port $PORT --log-level debug &
CLI_PID=$!
sleep 1

# Verify server is running
if ! kill -0 $CLI_PID 2>/dev/null; then
  echo "FAIL: CLI server did not start"
  exit 1
fi
echo "CLI server running (PID $CLI_PID)"
echo ""

echo "=== Server Status ==="
run_test "GET /status (ready)" "GET" "/status" "" '"ready":true'

echo ""
echo "=== Session Creation ==="
run_test "POST /session" "POST" "/session" "{\"capabilities\":{\"alwaysMatch\":{\"tauri:options\":{\"binary\":\"$APP_BIN\"}}}}" '"sessionId"'
extract_session_id
echo "      Session ID: $SESSION_ID"

if [ -z "$SESSION_ID" ]; then
  echo "FAIL: No session ID returned, cannot continue"
  kill $CLI_PID 2>/dev/null; wait $CLI_PID 2>/dev/null
  exit 1
fi

# Wait for app to fully load
sleep 2

echo ""
echo "=== Server Status (busy) ==="
run_test "GET /status (busy)" "GET" "/status" "" '"ready":false'

echo ""
echo "=== Window Operations ==="
run_test "GET window handle" "GET" "/session/$SESSION_ID/window" "" '"main"'
run_test "GET window handles" "GET" "/session/$SESSION_ID/window/handles" "" '"main"'
run_test "GET window rect" "GET" "/session/$SESSION_ID/window/rect" "" '"width"'
run_test "SET window rect" "POST" "/session/$SESSION_ID/window/rect" '{"width":1024,"height":768}' '"width"'

echo ""
echo "=== Navigation ==="
run_test "GET title" "GET" "/session/$SESSION_ID/title" "" '"WebDriver Test App"'
run_test "GET url" "GET" "/session/$SESSION_ID/url" "" 'tauri'

echo ""
echo "=== Find Elements ==="
run_test "Find element (#title)" "POST" "/session/$SESSION_ID/element" '{"using":"css selector","value":"#title"}' '"element-6066-11e4-a52e-4f735466cecf"'
extract_element_id TITLE_EID
echo "      Element ID: $TITLE_EID"

run_test "Find element (#increment)" "POST" "/session/$SESSION_ID/element" '{"using":"css selector","value":"#increment"}' '"element-6066'
extract_element_id BTN_EID
echo "      Element ID: $BTN_EID"

run_test "Find element (#counter)" "POST" "/session/$SESSION_ID/element" '{"using":"css selector","value":"#counter"}' '"element-6066'
extract_element_id CTR_EID
echo "      Element ID: $CTR_EID"

run_test "Find element (#hidden)" "POST" "/session/$SESSION_ID/element" '{"using":"css selector","value":"#hidden"}' '"element-6066'
extract_element_id HIDDEN_EID
echo "      Element ID: $HIDDEN_EID"

run_test "Find element (#text-input)" "POST" "/session/$SESSION_ID/element" '{"using":"css selector","value":"#text-input"}' '"element-6066'
extract_element_id INPUT_EID
echo "      Element ID: $INPUT_EID"

run_test "Find elements (option)" "POST" "/session/$SESSION_ID/elements" '{"using":"css selector","value":"option"}' '"element-6066'

run_test "Find element not found" "POST" "/session/$SESSION_ID/element" '{"using":"css selector","value":"#nonexistent"}' '"no such element"'

echo ""
echo "=== Element Properties ==="
if [ -n "$TITLE_EID" ]; then
  run_test "Get text (#title)" "GET" "/session/$SESSION_ID/element/$TITLE_EID/text" "" '"Test App"'
  run_test "Get tag name (#title)" "GET" "/session/$SESSION_ID/element/$TITLE_EID/name" "" '"h1"'
  run_test "Get attribute id" "GET" "/session/$SESSION_ID/element/$TITLE_EID/attribute/id" "" '"title"'
  run_test "Get property tagName" "GET" "/session/$SESSION_ID/element/$TITLE_EID/property/tagName" "" '"H1"'
  run_test "Get element rect" "GET" "/session/$SESSION_ID/element/$TITLE_EID/rect" "" '"width"'
fi

echo ""
echo "=== Element State ==="
if [ -n "$TITLE_EID" ] && [ -n "$HIDDEN_EID" ]; then
  run_test "Is displayed (visible)" "GET" "/session/$SESSION_ID/element/$TITLE_EID/displayed" "" 'true'
  run_test "Is displayed (hidden)" "GET" "/session/$SESSION_ID/element/$HIDDEN_EID/displayed" "" 'false'
fi
if [ -n "$BTN_EID" ]; then
  run_test "Is enabled (button)" "GET" "/session/$SESSION_ID/element/$BTN_EID/enabled" "" 'true'
fi

echo ""
echo "=== Element Interaction ==="
if [ -n "$BTN_EID" ] && [ -n "$CTR_EID" ]; then
  run_test "Click increment" "POST" "/session/$SESSION_ID/element/$BTN_EID/click" "" 'null'
  sleep 0.3
  run_test "Counter is Count: 1" "GET" "/session/$SESSION_ID/element/$CTR_EID/text" "" '"Count: 1"'

  run_test "Click increment (2)" "POST" "/session/$SESSION_ID/element/$BTN_EID/click" "" 'null'
  sleep 0.3
  run_test "Counter is Count: 2" "GET" "/session/$SESSION_ID/element/$CTR_EID/text" "" '"Count: 2"'

  run_test "Click increment (3)" "POST" "/session/$SESSION_ID/element/$BTN_EID/click" "" 'null'
  sleep 0.3
  run_test "Counter is Count: 3" "GET" "/session/$SESSION_ID/element/$CTR_EID/text" "" '"Count: 3"'
fi

if [ -n "$INPUT_EID" ]; then
  run_test "Send keys to input" "POST" "/session/$SESSION_ID/element/$INPUT_EID/value" '{"text":"hello"}' 'null'
  sleep 0.2
  run_test "Clear input" "POST" "/session/$SESSION_ID/element/$INPUT_EID/clear" "" 'null'
fi

echo ""
echo "=== Script Execution ==="
run_test "Execute sync (1+1)" "POST" "/session/$SESSION_ID/execute/sync" '{"script":"return 1+1","args":[]}' '"value":2'
run_test "Execute sync (title)" "POST" "/session/$SESSION_ID/execute/sync" '{"script":"return document.title","args":[]}' '"WebDriver Test App"'
run_test "Execute sync (with args)" "POST" "/session/$SESSION_ID/execute/sync" '{"script":"return arguments[0]+arguments[1]","args":[10,20]}' '"value":30'
run_test "Execute async" "POST" "/session/$SESSION_ID/execute/async" '{"script":"var done=arguments[arguments.length-1];setTimeout(function(){done(99)},100)","args":[]}' '"value":99'

echo ""
echo "=== Timeouts ==="
run_test "GET timeouts" "GET" "/session/$SESSION_ID/timeouts" "" '"script":30000'
run_test "SET timeouts" "POST" "/session/$SESSION_ID/timeouts" '{"script":60000,"implicit":5000}' 'null'
run_test "GET timeouts (updated)" "GET" "/session/$SESSION_ID/timeouts" "" '"script":60000'

echo ""
echo "=== Screenshots ==="
run_test "Full page screenshot" "GET" "/session/$SESSION_ID/screenshot" "" '"value"'
if [ -n "$TITLE_EID" ]; then
  run_test "Element screenshot (#title)" "GET" "/session/$SESSION_ID/element/$TITLE_EID/screenshot" "" '"value"'
fi

echo ""
echo "=== Session Cleanup ==="
run_test "DELETE session" "DELETE" "/session/$SESSION_ID" "" 'null'
sleep 1
run_test "GET /status (ready again)" "GET" "/status" "" '"ready":true'

echo ""
echo "=================================="
echo "W3C WebDriver Results: $PASS passed, $FAIL failed"
echo "=================================="

# Cleanup
kill $CLI_PID 2>/dev/null; wait $CLI_PID 2>/dev/null
pkill -f "webdriver-test-app" 2>/dev/null || true
rm -f /tmp/tauri-webdriver-last-result

if [ $FAIL -gt 0 ]; then
  exit 1
fi
