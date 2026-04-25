#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
REMOTE_DIR="${REPO_ROOT}/crates/remote"

export PATH="/opt/homebrew/bin:${PATH}"

if ! command -v tailscale >/dev/null 2>&1; then
  echo "tailscale CLI not found on PATH" >&2
  exit 1
fi

TS_HOSTNAME="${TS_HOSTNAME:-$(tailscale status --json 2>/dev/null | python3 -c "import sys, json; print(json.load(sys.stdin)['Self']['DNSName'].rstrip('.'))") }"
TS_HOSTNAME="${TS_HOSTNAME% }"

export PUBLIC_BASE_URL="${PUBLIC_BASE_URL:-https://${TS_HOSTNAME}:3001}"
export VITE_RELAY_API_BASE_URL="${VITE_RELAY_API_BASE_URL:-https://${TS_HOSTNAME}:8443}"

"${SCRIPT_DIR}/configure-tailscale-serve.sh"

cd "${REMOTE_DIR}"
docker compose --env-file .env.remote --profile relay up -d --build

docker compose --env-file .env.remote --profile relay ps

echo
curl -fsS http://127.0.0.1:3000/v1/health
curl -fsS http://127.0.0.1:8082/health
