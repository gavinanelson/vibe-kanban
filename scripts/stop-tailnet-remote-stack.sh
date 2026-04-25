#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
REMOTE_DIR="${REPO_ROOT}/crates/remote"

cd "${REMOTE_DIR}"
docker compose --env-file .env.remote --profile relay down
