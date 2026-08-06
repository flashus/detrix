#!/usr/bin/env bash
# Build a Detrix Dockerfile with base image args from images.env (single source of truth).
#
# Usage:
#   fixtures/docker/build.sh -f fixtures/docker/Dockerfile.server -t detrix-server .
#
# The Dockerfiles declare ARG RUST_IMAGE / GO_IMAGE / ... WITHOUT defaults, so a
# plain `docker build` would fail. This wrapper sources images.env and propagates
# the values via --build-arg KEY (no value = take from the local environment).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

set -a
# shellcheck disable=SC1091
. "$SCRIPT_DIR/images.env"
set +a

docker build \
  --build-arg RUST_IMAGE \
  --build-arg GO_IMAGE \
  --build-arg GO_CLASSIC_IMAGE \
  --build-arg PYTHON_IMAGE \
  --build-arg RUNTIME_IMAGE \
  "$@"