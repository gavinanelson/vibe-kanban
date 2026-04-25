#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

export PATH="/opt/homebrew/bin:${HOME}/.cargo/bin:${PATH}"

if [[ -f "${HOME}/.cargo/env" ]]; then
  # shellcheck disable=SC1090
  . "${HOME}/.cargo/env"
fi

has_libclang_library() {
  local candidate="$1"

  [[ -n "${candidate}" ]] || return 1

  compgen -G "${candidate}/libclang.so*" > /dev/null || \
    compgen -G "${candidate}/libclang.dylib*" > /dev/null || \
    compgen -G "${candidate}/libclang.dll*" > /dev/null
}

find_system_libclang_path() {
  local candidate

  if [[ -n "${LIBCLANG_PATH:-}" ]] && has_libclang_library "${LIBCLANG_PATH}"; then
    printf '%s\n' "${LIBCLANG_PATH}"
    return 0
  fi

  for candidate in \
    /Library/Developer/CommandLineTools/usr/lib \
    /Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib \
    /usr/lib/llvm-*/lib \
    /usr/local/lib/llvm-*/lib \
    /opt/homebrew/opt/llvm/lib \
    "${REPO_ROOT}"/.cache/libclang-venv/lib/python*/site-packages/clang/native
  do
    if has_libclang_library "${candidate}"; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done

  return 1
}

ensure_repo_libclang() {
  local venv_dir="${VK_LIBCLANG_VENV_DIR:-${REPO_ROOT}/.cache/libclang-venv}"
  local native_path=""

  if ! command -v python3 > /dev/null 2>&1; then
    echo "python3 is required to bootstrap libclang automatically." >&2
    return 1
  fi

  if [[ ! -x "${venv_dir}/bin/python" ]]; then
    python3 -m venv "${venv_dir}"
  fi

  if [[ ! -f "${venv_dir}/.libclang-installed" ]]; then
    "${venv_dir}/bin/pip" install --quiet libclang
    touch "${venv_dir}/.libclang-installed"
  fi

  native_path="$(compgen -G "${venv_dir}/lib/python*/site-packages/clang/native" | head -n 1 || true)"

  if [[ -z "${native_path}" ]]; then
    echo "Unable to locate the libclang Python package inside ${venv_dir}." >&2
    return 1
  fi

  printf '%s\n' "${native_path}"
}

configure_bindgen_env() {
  local libclang_path=""
  local gcc_machine=""
  local gcc_include_dir=""
  local sys_include_dir=""

  if ! libclang_path="$(find_system_libclang_path)"; then
    libclang_path="$(ensure_repo_libclang)"
  fi

  export LIBCLANG_PATH="${libclang_path}"

  if [[ -z "${BINDGEN_EXTRA_CLANG_ARGS:-}" ]]; then
    if command -v cc > /dev/null 2>&1; then
      gcc_machine="$(cc -dumpmachine 2>/dev/null || true)"
    fi

    if [[ -n "${gcc_machine}" ]]; then
      gcc_include_dir="/usr/lib/gcc/${gcc_machine}/$(cc -dumpversion 2>/dev/null || true)/include"
      sys_include_dir="/usr/include/${gcc_machine}"
    fi

    BINDGEN_EXTRA_CLANG_ARGS="-I/usr/include"

    if [[ -n "${sys_include_dir}" && -d "${sys_include_dir}" ]]; then
      BINDGEN_EXTRA_CLANG_ARGS="${BINDGEN_EXTRA_CLANG_ARGS} -I${sys_include_dir}"
    fi

    if [[ -n "${gcc_include_dir}" && -d "${gcc_include_dir}" ]]; then
      BINDGEN_EXTRA_CLANG_ARGS="${BINDGEN_EXTRA_CLANG_ARGS} -I${gcc_include_dir}"
    fi

    export BINDGEN_EXTRA_CLANG_ARGS
  fi
}

configure_bindgen_env
exec "$@"
