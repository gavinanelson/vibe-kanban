#!/usr/bin/env bash

set -euo pipefail

# Release/native Rust builds can exceed the RAM available on 16GB Linux
# laptops when Cargo fans out multiple rustc/linker jobs and emits debug info.
# Keep local release-style builds serialized by default. Operators can override
# any setting by exporting it before invoking this script, or disable the guard
# entirely with VK_CARGO_MEMORY_SAFE=0.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ "$#" -eq 0 ]]; then
  echo "usage: $0 <cargo-subcommand> [args...]" >&2
  exit 64
fi

is_release_or_native_build() {
  local arg

  for arg in "$@"; do
    if [[ "${arg}" == "--release" ]]; then
      return 0
    fi
  done

  # `cargo tauri build` performs a native desktop release build by default and
  # is the riskiest local path on low-memory laptops.
  if [[ "${1:-}" == "tauri" && "${2:-}" == "build" ]]; then
    return 0
  fi

  return 1
}

setting_source() {
  local name="$1"

  if [[ -n "${!name:-}" ]]; then
    printf 'user'
  else
    printf 'default'
  fi
}

if [[ "${VK_CARGO_MEMORY_SAFE:-1}" != "0" ]] && is_release_or_native_build "$@"; then
  cargo_build_jobs_source="$(setting_source CARGO_BUILD_JOBS)"
  cargo_incremental_source="$(setting_source CARGO_INCREMENTAL)"
  release_debug_source="$(setting_source CARGO_PROFILE_RELEASE_DEBUG)"
  release_split_debuginfo_source="$(setting_source CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO)"
  rustflags_source="$(setting_source RUSTFLAGS)"

  export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
  export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
  export CARGO_PROFILE_RELEASE_DEBUG="${CARGO_PROFILE_RELEASE_DEBUG:-0}"
  export CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO="${CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO:-off}"
  export RUSTFLAGS="${RUSTFLAGS:--C debuginfo=0}"

  echo "resource-safe cargo: enabled for release/native build" >&2
  echo "  CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} (${cargo_build_jobs_source})" >&2
  echo "  CARGO_INCREMENTAL=${CARGO_INCREMENTAL} (${cargo_incremental_source})" >&2
  echo "  CARGO_PROFILE_RELEASE_DEBUG=${CARGO_PROFILE_RELEASE_DEBUG} (${release_debug_source})" >&2
  echo "  CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=${CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO} (${release_split_debuginfo_source})" >&2
  echo "  RUSTFLAGS=${RUSTFLAGS} (${rustflags_source})" >&2
  echo "  override: set env vars explicitly, or VK_CARGO_MEMORY_SAFE=0 to disable" >&2
else
  echo "resource-safe cargo: no release/native guard applied" >&2
fi

exec "${SCRIPT_DIR}/run-rust-with-bindgen-env.sh" cargo "$@"
