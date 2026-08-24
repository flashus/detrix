#!/usr/bin/env bash
set -euo pipefail

# Single source for the disposable build/test target root. Taskfiles and
# runtime test helpers may override it without editing task definitions.
# GitHub Actions supplies RUNNER_TEMP outside /private/tmp; that exact
# disposable subtree is allowed for the native Linux release gate.
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
  /private/tmp/*|/tmp/*) ;;
  *)
    RUNNER_TEMP_ROOT="${RUNNER_TEMP:-}"
    if [[ -z "${RUNNER_TEMP_ROOT}" || "${RUNNER_TEMP_ROOT}" == "/" ||
          "${TARGET_DIR}" != "${RUNNER_TEMP_ROOT}"/* ]]; then
      echo "refusing session target outside approved disposable roots: ${TARGET_DIR}" >&2
      exit 1
    fi
    ;;
esac

case "${1:-root}" in
  root) printf '%s\n' "${TARGET_DIR}" ;;
  host) printf '%s/host\n' "${TARGET_DIR}" ;;
  out) printf '%s/host/out\n' "${TARGET_DIR}" ;;
  docker) printf '%s/host/docker\n' "${TARGET_DIR}" ;;
  *)
    echo "usage: $0 [root|host|out|docker]" >&2
    exit 2
    ;;
esac
