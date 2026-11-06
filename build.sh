#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "==> Running tests..."
cargo test

echo "==> Building WebAssembly component (wasm32-wasip2)..."
rustup target list --installed | grep -qx wasm32-wasip2 || rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2

mkdir -p dist
cp target/wasm32-wasip2/release/openidea_plugin_latte.wasm dist/plugin.wasm

sha256() { if command -v sha256sum >/dev/null; then sha256sum; else shasum -a 256; fi; }
sha256 < dist/plugin.wasm | cut -d' ' -f1 > dist/plugin.wasm.sources

echo "==> Packaging and signing plugin..."
cargo run --manifest-path tools/signer/Cargo.toml
