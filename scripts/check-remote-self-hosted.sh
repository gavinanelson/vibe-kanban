#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
REMOTE_DIR="${REPO_ROOT}/crates/remote"
MANIFEST="${REMOTE_DIR}/Cargo.toml"
MANIFEST_BACKUP="${REMOTE_DIR}/Cargo.toml.selfhost-backup"
LOCKFILE="${REMOTE_DIR}/Cargo.lock"
LOCKFILE_BACKUP="${REMOTE_DIR}/Cargo.lock.selfhost-backup"

export PATH="/opt/homebrew/bin:${HOME}/.cargo/bin:${PATH}"

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck disable=SC1090
  . "${HOME}/.cargo/env"
fi

cleanup() {
  if [[ -f "${MANIFEST_BACKUP}" ]]; then
    mv "${MANIFEST_BACKUP}" "${MANIFEST}"
  fi

  if [[ -f "${LOCKFILE_BACKUP}" ]]; then
    mv "${LOCKFILE_BACKUP}" "${LOCKFILE}"
  fi
}

trap cleanup EXIT

cp "${MANIFEST}" "${MANIFEST_BACKUP}"

python3 - <<'PY' "${MANIFEST_BACKUP}" "${MANIFEST}"
from pathlib import Path
import sys
source = Path(sys.argv[1]).read_text()
source = source.replace(
    'vk-billing = ["dep:billing"]',
    'vk-billing = []',
)
filtered_lines = []
for line in source.splitlines():
    if 'vibe-kanban-private' in line and line.lstrip().startswith('billing ='):
        continue
    if line.strip() == '# private crate for billing functionality':
        continue
    filtered_lines.append(line)
Path(sys.argv[2]).write_text('\n'.join(filtered_lines) + '\n')
PY

if [[ -f "${LOCKFILE}" ]]; then
  mv "${LOCKFILE}" "${LOCKFILE_BACKUP}"
fi

cargo check --manifest-path "${MANIFEST}"
