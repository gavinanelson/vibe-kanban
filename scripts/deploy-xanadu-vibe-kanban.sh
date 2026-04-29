#!/usr/bin/env bash
set -euo pipefail

export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:${HOME}/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEPLOY_ROOT="${VIBE_KANBAN_DEPLOY_ROOT:-${HOME}/.local/state/paddys/vibe-kanban-deploy}"
BIN_DIR="$DEPLOY_ROOT/bin"
LOG_DIR="$DEPLOY_ROOT/logs"
DEPLOY_DOC="$DEPLOY_ROOT/DEPLOYMENT.md"
PID_FILE="$DEPLOY_ROOT/vibe-kanban.pid"
BACKEND_PID_FILE="$DEPLOY_ROOT/vibe-kanban-backend.pid"
FRONTEND_PID_FILE="$DEPLOY_ROOT/vibe-kanban-frontend.pid"
SERVICE_LABEL="com.xanadu.vibe-kanban"
PORT="${VIBE_KANBAN_PORT:-8080}"
BACKEND_PORT="${VIBE_KANBAN_BACKEND_PORT:-8082}"
PREVIEW_PROXY_PORT="${VIBE_KANBAN_PREVIEW_PROXY_PORT:-8081}"
PUBLIC_URL="${VIBE_KANBAN_PUBLIC_URL:-https://vibe.yxanadu.com}"
WORKDIR="${VIBE_KANBAN_WORKDIR:-/workspace}"
DEPLOY_MODE="${VIBE_KANBAN_DEPLOY_MODE:-}"
MOLD_VERSION="${VIBE_KANBAN_MOLD_VERSION:-2.41.0}"
MOLD_ROOT="${VIBE_KANBAN_MOLD_ROOT:-/tmp/mold-${MOLD_VERSION}-aarch64-linux}"
MOLD_URL="https://github.com/rui314/mold/releases/download/v${MOLD_VERSION}/mold-${MOLD_VERSION}-aarch64-linux.tar.gz"
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

if [[ -z "$DEPLOY_MODE" ]]; then
  if [[ "$(uname -s)" == "Darwin" ]]; then
    DEPLOY_MODE="release"
  else
    DEPLOY_MODE="dev"
  fi
fi

ensure_linux_mold() {
  if [[ -x "$MOLD_ROOT/bin/mold" ]]; then
    return 0
  fi

  local archive="/tmp/mold-${MOLD_VERSION}-aarch64-linux.tar.gz"
  curl -fsSL -o "$archive" "$MOLD_URL"
  rm -rf "$MOLD_ROOT"
  tar xzf "$archive" -C /tmp
}

echo "Preparing Vibe Kanban $DEPLOY_MODE deploy from $BRANCH_NAME @ $SHORT_SHA"
(
  cd "$REPO_ROOT"
  if command -v corepack >/dev/null 2>&1; then
    corepack enable
  fi
  CI=true pnpm install --frozen-lockfile
  if [[ "$DEPLOY_MODE" == "release" ]]; then
    ALLOW_HEAVY_VIBE_VALIDATION=1 \
      VITE_REACT_COMPILER="${VITE_REACT_COMPILER:-false}" \
      VITE_SENTRY_PLUGIN="${VITE_SENTRY_PLUGIN:-false}" \
      VITE_MINIFY="${VITE_MINIFY:-false}" \
      VITE_SOURCEMAP="${VITE_SOURCEMAP:-false}" \
      pnpm run build:npx
  elif [[ "$(uname -s)" == "Linux" ]]; then
    ensure_linux_mold
    CARGO_PROFILE_DEV_DEBUG=0 \
      CARGO_BUILD_JOBS=1 \
      scripts/run-rust-with-bindgen-env.sh \
      cargo rustc -p server --bin server -- \
        -C link-arg=-B"$MOLD_ROOT/bin" \
        -C link-arg=-fuse-ld=mold
  fi
)

if [[ "$DEPLOY_MODE" == "release" ]]; then
  install -m 0755 "$REPO_ROOT/target/release/server" "$BIN_DIR/vibe-kanban-server"
fi

stop_pid_file() {
  local pid_file="$1"
  local old_pid

  if [[ ! -f "$pid_file" ]]; then
    return 0
  fi

  old_pid="$(cat "$pid_file" 2>/dev/null || true)"
  if [[ -z "$old_pid" ]] || ! kill -0 "$old_pid" 2>/dev/null; then
    rm -f "$pid_file"
    return 0
  fi

  kill "$old_pid" 2>/dev/null || true
  for _ in {1..30}; do
    if ! kill -0 "$old_pid" 2>/dev/null; then
      rm -f "$pid_file"
      return 0
    fi
    sleep 1
  done

  kill -9 "$old_pid" 2>/dev/null || true
  rm -f "$pid_file"
}

install_linux_dev_shim() {
  local shim="$BIN_DIR/vibe-kanban-dev-shim"

  cat > "$shim" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${VIBE_KANBAN_REPO_ROOT:-/workspace/projects/vibe-kanban}"
DEPLOY_ROOT="${VIBE_KANBAN_DEPLOY_ROOT:-${HOME}/.local/state/paddys/vibe-kanban-deploy}"
LOG_DIR="$DEPLOY_ROOT/logs"
FRONTEND_PORT="${FRONTEND_PORT:-${PORT:-8080}}"
BACKEND_PORT="${BACKEND_PORT:-${VIBE_KANBAN_BACKEND_PORT:-8082}}"
PREVIEW_PROXY_PORT="${PREVIEW_PROXY_PORT:-${VIBE_KANBAN_PREVIEW_PROXY_PORT:-8081}}"
PUBLIC_URL="${VIBE_KANBAN_PUBLIC_URL:-https://vibe.yxanadu.com}"

mkdir -p "$LOG_DIR"
cd "$REPO_ROOT"

export HOST="${HOST:-0.0.0.0}"
export FRONTEND_PORT
export BACKEND_PORT
export PORT="$BACKEND_PORT"
export PREVIEW_PROXY_PORT
export VK_OPEN_BROWSER=0
export DISABLE_WORKTREE_CLEANUP=1
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export RUST_LOG="${RUST_LOG:-info}"
export VITE_REACT_COMPILER="${VITE_REACT_COMPILER:-false}"
export VITE_SENTRY_PLUGIN="${VITE_SENTRY_PLUGIN:-false}"
export VITE_VK_SHARED_API_BASE="${VITE_VK_SHARED_API_BASE:-}"
export VK_ALLOWED_ORIGINS="${VK_ALLOWED_ORIGINS:-http://127.0.0.1:$FRONTEND_PORT,http://localhost:$FRONTEND_PORT,$PUBLIC_URL}"

"$REPO_ROOT/target/debug/server" > "$LOG_DIR/backend.out.log" 2> "$LOG_DIR/backend.err.log" &
backend_pid=$!

cleanup() {
  kill "$backend_pid" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

exec pnpm --filter @vibe/local-web run dev --host 0.0.0.0 --port "$FRONTEND_PORT"
SHIM

  chmod 0755 "$shim"

  python3 - <<'PY' "$shim"
from pathlib import Path
from zipfile import ZipFile, ZIP_DEFLATED
import os
import sys

shim = Path(sys.argv[1])
cache_root = Path.home() / ".vibe-kanban" / "bin"
if not cache_root.exists():
    raise SystemExit(0)

for platform_dir in cache_root.glob("*/linux-arm64"):
    platform_dir.mkdir(parents=True, exist_ok=True)
    binary_path = platform_dir / "vibe-kanban"
    zip_path = platform_dir / "vibe-kanban.zip"
    binary_path.write_bytes(shim.read_bytes())
    os.chmod(binary_path, 0o755)
    with ZipFile(zip_path, "w", ZIP_DEFLATED) as archive:
        archive.write(shim, "vibe-kanban")
PY
}

if [[ "$DEPLOY_MODE" == "release" && "$(uname -s)" == "Darwin" ]]; then
  PLIST_PATH="${HOME}/Library/LaunchAgents/com.xanadu.vibe-kanban.plist"
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
else
  stop_pid_file "$PID_FILE"
  stop_pid_file "$BACKEND_PID_FILE"
  stop_pid_file "$FRONTEND_PID_FILE"

  # Stop npx-launched Vibe Kanban instances that are bound to the same PADDYs
  # port. Those are not linked to this repo deployment and otherwise keep
  # serving stale binaries on PORT=8080.
  if [[ "$DEPLOY_MODE" == "dev" ]]; then
    install_linux_dev_shim
  fi

  ps -eo pid=,args= \
    | awk '/npm exec vibe-kanban|node .*vibe-kanban|\/home\/coder\/\.vibe-kanban\/.*\/vibe-kanban/ { print $1 }' \
    | xargs -r kill >/dev/null 2>&1 || true
  ps -eo pid=,args= \
    | awk '/\/workspace\/projects\/vibe-kanban/ && /target\/debug\/server|pnpm run backend:dev:watch|cargo run --bin server|local-web.*vite|vite.*--port 8080/ && !/deploy-xanadu-vibe-kanban/ { print $1 }' \
    | xargs -r kill >/dev/null 2>&1 || true
  sleep 2

  if [[ "$DEPLOY_MODE" == "dev" ]]; then
    (
      cd "$WORKDIR"
      FRONTEND_PORT="$PORT" \
        BACKEND_PORT="$BACKEND_PORT" \
        PREVIEW_PROXY_PORT="$PREVIEW_PROXY_PORT" \
        VIBE_KANBAN_REPO_ROOT="$REPO_ROOT" \
        nohup "$BIN_DIR/vibe-kanban-dev-shim" > "$LOG_DIR/server.out.log" 2> "$LOG_DIR/server.err.log" &
      echo $! > "$PID_FILE"
    )
  else
    if [[ ! -x "$BIN_DIR/vibe-kanban-server" ]]; then
      echo "Missing release binary: $BIN_DIR/vibe-kanban-server" >&2
      exit 1
    fi

    (
      cd "$WORKDIR"
      export HOST="${HOST:-0.0.0.0}"
      export PORT="$PORT"
      export PREVIEW_PROXY_PORT="$PREVIEW_PROXY_PORT"
      export VK_OPEN_BROWSER=0
      export RUST_LOG="${RUST_LOG:-info}"
      export VK_ALLOWED_ORIGINS="${VK_ALLOWED_ORIGINS:-http://127.0.0.1:$PORT,http://localhost:$PORT,$PUBLIC_URL}"
      nohup "$BIN_DIR/vibe-kanban-server" > "$LOG_DIR/server.out.log" 2> "$LOG_DIR/server.err.log" &
      echo $! > "$PID_FILE"
    )
  fi
fi

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

check_url "http://127.0.0.1:$PORT/" "200"
check_url "http://127.0.0.1:$PORT/api/info" "200"
check_url "http://127.0.0.1:$BACKEND_PORT/api/info" "200"

if [[ "${VIBE_KANBAN_VERIFY_PUBLIC:-1}" == "1" ]]; then
  check_url "$PUBLIC_URL/api/info" "200"
fi

python3 - <<'PY' "$DEPLOY_DOC" "$REPO_URL" "$BRANCH_NAME" "$FULL_SHA" "$SHORT_SHA" "$PUBLIC_URL" "$PORT" "$BACKEND_PORT" "$PREVIEW_PROXY_PORT" "$WORKDIR" "$PID_FILE" "$DEPLOY_MODE"
from pathlib import Path
import sys

path = Path(sys.argv[1])
repo_url, branch, full_sha, short_sha, public_url, port, backend_port, preview_port, workdir, pid_file, deploy_mode = sys.argv[2:]
path.write_text(f"""# Vibe Kanban live deployment contract

## Canonical source of truth
The live Xanadu Vibe Kanban service is auto-deployed from Gavin's fork through GitHub Actions.

- fork: `{repo_url}`
- auto-deploy branch: `{branch}`
- current deployed commit: `{full_sha}`

## Runtime
- installed binary: `{path.parent}/bin/vibe-kanban-server`
- pid file: `{pid_file}`
- deploy mode: `{deploy_mode}`
- working directory: `{workdir}`
- public URL: `{public_url}`
- backend loopback: `http://127.0.0.1:{port}`
- backend API loopback: `http://127.0.0.1:{backend_port}`
- preview proxy loopback: `http://127.0.0.1:{preview_port}`
- last built short SHA: `{short_sha}`

## Deployment method
A repository self-hosted GitHub Actions runner on `xanadu-host` rebuilds and redeploys Vibe Kanban on pushes to `main`.

- workflow: `.github/workflows/deploy-xanadu-vibe-kanban.yml`
- deploy script: `scripts/deploy-xanadu-vibe-kanban.sh`

## Manual verification
```bash
curl -s http://127.0.0.1:{port}/
curl -s http://127.0.0.1:{port}/api/info
curl -s http://127.0.0.1:{backend_port}/api/info
curl -ks {public_url}/api/info
cat {pid_file}
gh run list -R gavinanelson/vibe-kanban --workflow deploy-xanadu-vibe-kanban.yml --limit 5
```
""")
PY

echo "Deploy complete: $FULL_SHA"
