#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
STATE_DIR="${REPO_ROOT}/.vibe-kanban-dev"

stop_pid_file() {
  local pid_file="$1"

  if [[ ! -f "${pid_file}" ]]; then
    return 0
  fi

  local pid
  pid="$(cat "${pid_file}")"

  if kill -0 "${pid}" 2>/dev/null; then
    kill "${pid}" 2>/dev/null || true
  fi

  rm -f "${pid_file}"
}

stop_pid_file "${STATE_DIR}/frontend.pid"
stop_pid_file "${STATE_DIR}/backend.pid"

echo "Stopped local Vibe Kanban stack."
