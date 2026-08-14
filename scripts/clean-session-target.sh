#!/usr/bin/env bash
set -euo pipefail

# Deliberately narrow cleanup for this session's Cargo artifacts.
# No other target directory, cache, image, volume, or user data is touched.
# DETRIX_SESSION_TARGET_DIR may select another disposable child of /private/tmp.
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TARGET_DIR="$("${SCRIPT_DIR}/session-root-dir.sh")"

case "${TARGET_DIR}" in
  /private/tmp|/private/tmp/|*/..|*/../*|*/.)
  echo "refusing unexpected target path: ${TARGET_DIR}" >&2
  exit 1
  ;;
  /private/tmp/*) ;;
  *)
  echo "refusing unexpected target path: ${TARGET_DIR}" >&2
  exit 1
  ;;
esac

if [[ -e "${TARGET_DIR}" && ! -d "${TARGET_DIR}" ]]; then
  echo "refusing non-directory target path: ${TARGET_DIR}" >&2
  exit 1
fi

rm -rf -- "${TARGET_DIR}"
mkdir -p -- \
  "${TARGET_DIR}/host" \
  "${TARGET_DIR}/docker" \
  "${TARGET_DIR}/go-build" \
  "${TARGET_DIR}/go-mod" \
  "${TARGET_DIR}/cargo-home"
echo "cleaned ${TARGET_DIR}"
