#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Install deps if needed
if [ ! -d node_modules ]; then
  echo "Installing WDIO dependencies..."
  npm install
fi

echo "Running WDIO tests..."
npx wdio run wdio.conf.mjs
