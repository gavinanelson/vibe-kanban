#!/usr/bin/env bash
set -euo pipefail

export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:${HOME}/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEPLOY_ROOT="${VIBE_KANBAN_DEPLOY_ROOT:-/Users/gavin/.hermes/vibe-kanban}"
BIN_DIR="$DEPLOY_ROOT/bin"
LOG_DIR="$DEPLOY_ROOT/logs"
DEPLOY_DOC="$DEPLOY_ROOT/DEPLOYMENT.md"
PLIST_PATH="${HOME}/Library/LaunchAgents/com.xanadu.vibe-kanban.plist"
SERVICE_LABEL="com.xanadu.vibe-kanban"
PORT="${VIBE_KANBAN_PORT:-3063}"
PREVIEW_PROXY_PORT="${VIBE_KANBAN_PREVIEW_PROXY_PORT:-3064}"
PUBLIC_URL="${VIBE_KANBAN_PUBLIC_URL:-https://vibe.yxanadu.com}"
WORKDIR="${VIBE_KANBAN_WORKDIR:-/Users/gavin/xanadu-storage/workspaces/paddys/projects/projects}"
BRANCH_NAME="${GITHUB_REF_NAME:-$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD)}"
FULL_SHA="$(git -C "$REPO_ROOT" rev-parse HEAD)"
SHORT_SHA="$(git -C "$REPO_ROOT" rev-parse --short=9 HEAD)"
REPO_URL="$(git -C "$REPO_ROOT" remote get-url origin)"

if [[ "$BRANCH_NAME" != "main" ]]; then
  echo "Refusing to deploy Vibe Kanban from branch '$BRANCH_NAME' (allowed: main only)" >&2
  exit 1
fi

if [[ ! -d "$WORKDIR" ]]; then
  echo "Configured workdir does not exist, falling back to deploy root: $WORKDIR" >&2
  WORKDIR="$DEPLOY_ROOT"
fi

mkdir -p "$BIN_DIR" "$LOG_DIR" "$WORKDIR"

echo "Building Vibe Kanban from $BRANCH_NAME @ $SHORT_SHA"
(
  cd "$REPO_ROOT"
  if command -v corepack >/dev/null 2>&1; then
    corepack enable
  fi
  pnpm install --frozen-lockfile
  ALLOW_HEAVY_VIBE_VALIDATION=1 VITE_SOURCEMAP="${VITE_SOURCEMAP:-false}" pnpm run build:npx
)

install -m 0755 "$REPO_ROOT/target/release/server" "$BIN_DIR/vibe-kanban-server"

cat > "$PLIST_PATH" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$SERVICE_LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$BIN_DIR/vibe-kanban-server</string>
  </array>
  <key>WorkingDirectory</key>
  <string>$WORKDIR</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/opt/homebrew/sbin:${HOME}/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>HOST</key>
    <string>127.0.0.1</string>
    <key>PORT</key>
    <string>$PORT</string>
    <key>PREVIEW_PROXY_PORT</key>
    <string>$PREVIEW_PROXY_PORT</string>
    <key>VK_OPEN_BROWSER</key>
    <string>0</string>
    <key>VK_ALLOWED_ORIGINS</key>
    <string>http://127.0.0.1:$PORT,http://localhost:$PORT,$PUBLIC_URL</string>
    <key>RUST_LOG</key>
    <string>info</string>
  </dict>
  <key>StandardOutPath</key>
  <string>$LOG_DIR/server.out.log</string>
  <key>StandardErrorPath</key>
  <string>$LOG_DIR/server.err.log</string>
  <key>KeepAlive</key>
  <true/>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
PLIST

launchctl bootout "gui/$(id -u)/$SERVICE_LABEL" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$(id -u)" "$PLIST_PATH"
launchctl kickstart -k "gui/$(id -u)/$SERVICE_LABEL"

check_url() {
  local url="$1"
  local expected="$2"
  local attempt code
  for attempt in {1..45}; do
    code="$(curl -k -sS -o /tmp/vibe-kanban-check.$$ -w '%{http_code}' "$url" || true)"
    if [[ "$code" == "$expected" ]]; then
      rm -f /tmp/vibe-kanban-check.$$
      return 0
    fi
    sleep 2
  done
  echo "Verification failed for $url (last code=$code, expected=$expected)" >&2
  return 1
}

check_url "http://127.0.0.1:$PORT/health" "200"

if [[ "${VIBE_KANBAN_VERIFY_PUBLIC:-1}" == "1" ]]; then
  check_url "$PUBLIC_URL/health" "200"
fi

python3 - <<'PY' "$DEPLOY_DOC" "$REPO_URL" "$BRANCH_NAME" "$FULL_SHA" "$SHORT_SHA" "$PUBLIC_URL" "$PORT" "$PREVIEW_PROXY_PORT" "$WORKDIR" "$PLIST_PATH"
from pathlib import Path
import sys

path = Path(sys.argv[1])
repo_url, branch, full_sha, short_sha, public_url, port, preview_port, workdir, plist = sys.argv[2:]
path.write_text(f"""# Vibe Kanban live deployment contract

## Canonical source of truth
The live Xanadu Vibe Kanban service is auto-deployed from Gavin's fork through GitHub Actions.

- fork: `{repo_url}`
- auto-deploy branch: `{branch}`
- current deployed commit: `{full_sha}`

## Runtime
- launchd service: `com.xanadu.vibe-kanban`
- installed binary: `{path.parent}/bin/vibe-kanban-server`
- launchd plist: `{plist}`
- working directory: `{workdir}`
- public URL: `{public_url}`
- backend loopback: `http://127.0.0.1:{port}`
- preview proxy loopback: `http://127.0.0.1:{preview_port}`
- last built short SHA: `{short_sha}`

## Deployment method
A repository self-hosted GitHub Actions runner on `xanadu-host` rebuilds and redeploys Vibe Kanban on pushes to `main`.

- workflow: `.github/workflows/deploy-xanadu-vibe-kanban.yml`
- deploy script: `scripts/deploy-xanadu-vibe-kanban.sh`

## Manual verification
```bash
curl -s http://127.0.0.1:{port}/health
curl -ks {public_url}/health
launchctl print gui/$(id -u)/com.xanadu.vibe-kanban
gh run list -R gavinanelson/vibe-kanban --workflow deploy-xanadu-vibe-kanban.yml --limit 5
```
""")
PY

echo "Deploy complete: $FULL_SHA"
