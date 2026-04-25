#!/usr/bin/env bash
set -euo pipefail

pnpm run build:npx-cli
(
  cd npx-cli
  npm pack --dry-run >/dev/null
)
