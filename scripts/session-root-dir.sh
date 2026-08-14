#!/usr/bin/env bash
set -euo pipefail

# Single source for the disposable build/test target root. Taskfiles and
# runtime test helpers may override it without editing task definitions.
DEFAULT_TARGET_DIR="/private/tmp/detrix-session-target"
TARGET_DIR="${DETRIX_SESSION_TARGET_DIR:-${DEFAULT_TARGET_DIR}}"

if [[ -z "${TARGET_DIR}" || "${TARGET_DIR}" == "/" ]]; then
  echo "refusing unsafe empty/root session target" >&2
  exit 1
fi

case "${TARGET_DIR}" in
  /private/tmp|/private/tmp/|*/..|*/../*|*/.)
    echo "refusing unsafe session target: ${TARGET_DIR}" >&2
    exit 1
    ;;
  /private/tmp/*) ;;
  *)
    echo "refusing session target outside /private/tmp: ${TARGET_DIR}" >&2
    exit 1
    ;;
esac

if [[ "${1:-}" == "host" ]]; then
  printf '%s/host\n' "${TARGET_DIR}"
else
  printf '%s\n' "${TARGET_DIR}"
fi
