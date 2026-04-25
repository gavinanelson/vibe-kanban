#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"

if command -v cargo-watch > /dev/null 2>&1; then
  exec "${SCRIPT_DIR}/run-rust-with-bindgen-env.sh" \
    cargo watch -w crates -x "run --bin server"
fi

echo "cargo-watch not found, starting the server without file watching." >&2
exec "${SCRIPT_DIR}/run-rust-with-bindgen-env.sh" cargo run --bin server
