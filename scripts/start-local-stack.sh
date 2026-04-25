#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
STATE_DIR="${REPO_ROOT}/.vibe-kanban-dev"
LOG_DIR="${STATE_DIR}/logs"

mkdir -p "${LOG_DIR}"

export PATH="/opt/homebrew/bin:${HOME}/.cargo/bin:${PATH}"
export FRONTEND_PORT="$(node "${SCRIPT_DIR}/setup-dev-environment.js" frontend)"
export BACKEND_PORT="$(node "${SCRIPT_DIR}/setup-dev-environment.js" backend)"
export PREVIEW_PROXY_PORT="$(node "${SCRIPT_DIR}/setup-dev-environment.js" preview_proxy)"
export VK_ALLOWED_ORIGINS="${VK_ALLOWED_ORIGINS:-http://localhost:${FRONTEND_PORT}}"
export DISABLE_WORKTREE_CLEANUP="${DISABLE_WORKTREE_CLEANUP:-1}"
export RUST_LOG="${RUST_LOG:-debug}"
export VK_SHARED_API_BASE="${VK_SHARED_API_BASE:-}"
export VK_SHARED_RELAY_API_BASE="${VK_SHARED_RELAY_API_BASE:-}"
export VK_TUNNEL="${VK_TUNNEL:-}"
export VITE_VK_SHARED_API_BASE="${VITE_VK_SHARED_API_BASE:-${VK_SHARED_API_BASE:-}}"

cd "${REPO_ROOT}"

start_background() {
  local pid_file="$1"
  local log_file="$2"
  shift 2

  if [[ -f "${pid_file}" ]] && kill -0 "$(cat "${pid_file}")" 2>/dev/null; then
    return 0
  fi

  rm -f "${pid_file}"

  nohup bash -lc "cd '${REPO_ROOT}' && exec $*" > "${log_file}" 2>&1 &
  echo $! > "${pid_file}"
}

start_background \
  "${STATE_DIR}/backend.pid" \
  "${LOG_DIR}/backend.log" \
  "env BACKEND_PORT='${BACKEND_PORT}' PREVIEW_PROXY_PORT='${PREVIEW_PROXY_PORT}' VK_ALLOWED_ORIGINS='${VK_ALLOWED_ORIGINS}' DISABLE_WORKTREE_CLEANUP='${DISABLE_WORKTREE_CLEANUP}' RUST_LOG='${RUST_LOG}' VK_SHARED_API_BASE='${VK_SHARED_API_BASE}' VK_SHARED_RELAY_API_BASE='${VK_SHARED_RELAY_API_BASE}' VK_TUNNEL='${VK_TUNNEL}' pnpm run backend:dev:watch"

start_background \
  "${STATE_DIR}/frontend.pid" \
  "${LOG_DIR}/frontend.log" \
  "env FRONTEND_PORT='${FRONTEND_PORT}' VITE_VK_SHARED_API_BASE='${VITE_VK_SHARED_API_BASE}' pnpm run local-web:dev"

echo "Frontend: http://localhost:${FRONTEND_PORT}"
echo "Backend:  http://localhost:${BACKEND_PORT}"
echo "Logs:     ${LOG_DIR}"
