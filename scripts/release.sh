#!/usr/bin/env bash
# Detrix release script
#
# Reads the version from the workspace Cargo.toml, verifies the git tag does not
# already exist (locally or on the remote), then creates and pushes the tag.
# Pushing the tag triggers the docker-publish.yml workflow automatically.
#
# Usage:
#   bash scripts/release.sh          # interactive confirm
#   bash scripts/release.sh --yes    # skip confirm (CI / automation)

set -euo pipefail

YES=false
for arg in "$@"; do
  case "$arg" in
    --yes|-y) YES=true ;;
    *) echo "Unknown argument: $arg" >&2; exit 1 ;;
  esac
done

# ── Read version ──────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

VERSION=$(grep '^version' "$ROOT/Cargo.toml" | head -1 | sed 's/version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/')
if [[ -z "$VERSION" ]]; then
  echo "ERROR: could not read version from Cargo.toml" >&2
  exit 1
fi

TAG="v${VERSION}"
echo "Version : $VERSION"
echo "Tag     : $TAG"

# ── Check local tag ───────────────────────────────────────────────────────────
if git -C "$ROOT" tag -l "$TAG" | grep -q "^${TAG}$"; then
  echo "" >&2
  echo "ERROR: Tag $TAG already exists locally." >&2
  echo "  If you want to re-release, delete it first:" >&2
  echo "    git tag -d $TAG" >&2
  exit 1
fi

# ── Check remote tag ──────────────────────────────────────────────────────────
if git -C "$ROOT" ls-remote --tags origin "refs/tags/${TAG}" 2>/dev/null | grep -q "${TAG}"; then
  echo "" >&2
  echo "ERROR: Tag $TAG already exists on remote." >&2
  echo "  To re-release, delete it there first:" >&2
  echo "    git push origin --delete $TAG" >&2
  exit 1
fi

echo ""
echo "This will:"
echo "  1. Create local git tag   $TAG"
echo "  2. Push tag to origin  →  triggers docker-publish.yml"
echo ""

# ── Confirm ───────────────────────────────────────────────────────────────────
if [[ "$YES" != true ]]; then
  read -r -p "Continue? [y/N] " REPLY
  echo
  if [[ ! "$REPLY" =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 0
  fi
fi

# ── Tag and push ──────────────────────────────────────────────────────────────
git -C "$ROOT" tag "$TAG"
git -C "$ROOT" push origin "$TAG"

echo ""
echo "✅  Tag $TAG pushed."
echo "   Docker publish: https://github.com/flashus/detrix/actions/workflows/docker-publish.yml"
