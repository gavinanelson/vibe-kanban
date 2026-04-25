#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

export PATH="/opt/homebrew/bin:${HOME}/.cargo/bin:${PATH}"

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck disable=SC1090
  . "${HOME}/.cargo/env"
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl not found on PATH" >&2
  exit 1
fi

health_url() {
  local base="${1%/}"
  local path="$2"
  printf '%s%s\n' "${base}" "${path}"
}

check_health() {
  local label="$1"
  local url="$2"

  if ! curl --fail --silent --show-error --max-time 5 "${url}" >/dev/null; then
    cat >&2 <<EOF
${label} is unreachable: ${url}

Check that the tailnet remote stack is running and that the selected URL is the
intended service. On xanadu-host the bridge-safe defaults are:
  VK_SHARED_API_BASE=http://127.0.0.1:3000
  VK_SHARED_RELAY_API_BASE=http://127.0.0.1:8082

If another process is bound to one of those ports, stop it before starting the
host stack.
EOF
    exit 1
  fi
}

# The local host app registers through the bridge. The bridge accepts loopback
# hosts only, so default to loopback endpoints and let callers opt into other
# URLs explicitly via env.
export VK_SHARED_API_BASE="${VK_SHARED_API_BASE:-http://127.0.0.1:3000}"
export VK_SHARED_RELAY_API_BASE="${VK_SHARED_RELAY_API_BASE:-http://127.0.0.1:8082}"
export VK_TUNNEL="${VK_TUNNEL:-1}"
export VITE_VK_SHARED_API_BASE="${VITE_VK_SHARED_API_BASE:-${VK_SHARED_API_BASE}}"

check_health "Remote API health check" "$(health_url "${VK_SHARED_API_BASE}" "/v1/health")"
check_health "Relay health check" "$(health_url "${VK_SHARED_RELAY_API_BASE}" "/health")"

cd "${REPO_ROOT}"
exec "${SCRIPT_DIR}/start-local-stack.sh"
