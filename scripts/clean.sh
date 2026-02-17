#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: bash scripts/clean.sh [--deep]

Default cleanup:
- Runs cargo clean for this workspace
- Runs cargo clean for tests/test-app/src-tauri
- Removes .DS_Store files

Deep cleanup (--deep):
- Removes tests/wdio/node_modules
- Removes generated files in screenshots/
EOF
}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEEP=0

for arg in "$@"; do
  case "$arg" in
    --deep)
      DEEP=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      usage
      exit 1
      ;;
  esac
done

echo "Cleaning Rust build artifacts..."
cargo clean --manifest-path "$ROOT/Cargo.toml"
cargo clean --manifest-path "$ROOT/tests/test-app/src-tauri/Cargo.toml"

echo "Removing .DS_Store files..."
find "$ROOT" -name ".DS_Store" -type f -delete

if [ "$DEEP" -eq 1 ]; then
  if [ -d "$ROOT/tests/wdio/node_modules" ]; then
    echo "Removing tests/wdio/node_modules..."
    rm -rf "$ROOT/tests/wdio/node_modules"
  fi

  if [ -d "$ROOT/screenshots" ]; then
    echo "Removing generated screenshots..."
    find "$ROOT/screenshots" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  fi
fi

echo "Cleanup complete."
du -sh "$ROOT"
