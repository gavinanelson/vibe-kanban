#!/usr/bin/env bash

set -euo pipefail

APP_PORT="${VK_TAILSCALE_APP_PORT:-3001}"
RELAY_PORT="${VK_TAILSCALE_RELAY_PORT:-8443}"
APP_TARGET="${VK_TAILSCALE_APP_TARGET:-http://127.0.0.1:3000}"
RELAY_TARGET="${VK_TAILSCALE_RELAY_TARGET:-http://127.0.0.1:8082}"

if ! command -v tailscale >/dev/null 2>&1; then
  echo "tailscale CLI not found on PATH" >&2
  exit 1
fi

tailscale serve --bg --https="${APP_PORT}" "${APP_TARGET}"
tailscale serve --bg --https="${RELAY_PORT}" "${RELAY_TARGET}"

tailscale serve status
