#!/bin/bash
set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
echo "=== Building tauri-webdriver ==="
cd "$ROOT" && cargo build 2>&1
echo ""
echo "=== Building test app ==="
cd "$ROOT/tests/test-app/src-tauri" && cargo build 2>&1
echo ""
echo "========================================="
echo "=== Running Plugin Tests ==="
echo "========================================="
cd "$ROOT"
bash tests/run_plugin_tests.sh
PLUGIN_EXIT=$?
echo ""
echo "========================================="
echo "=== Running W3C WebDriver Tests ==="
echo "========================================="
bash tests/run_w3c_tests.sh
W3C_EXIT=$?
echo ""
echo "========================================="
if [ $PLUGIN_EXIT -ne 0 ] || [ $W3C_EXIT -ne 0 ]; then
  echo "SOME TESTS FAILED"
  exit 1
else
  echo "ALL TESTS PASSED"
  exit 0
fi
