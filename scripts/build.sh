#!/usr/bin/env bash
# sView リリースビルドスクリプト (macOS / Linux)
set -euo pipefail
cd "$(dirname "$0")/.."

command -v cargo >/dev/null || { echo "Rust が必要です: https://rustup.rs/"; exit 1; }
command -v npm >/dev/null || { echo "Node.js が必要です: https://nodejs.org/"; exit 1; }

npm install
npm run build

echo ""
echo "ビルド完了。成果物: src-tauri/target/release/bundle/"
