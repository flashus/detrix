#!/bin/sh
# Read the service JWT from shared volume and start the Detrix daemon.
# DETRIX_TOKEN is used for control-plane communication with app containers.
# API authentication uses external JWT (configured in detrix-keycloak.toml).
set -e

TOKEN_FILE="${TOKEN_FILE:-/shared/service-token}"

if [ -f "$TOKEN_FILE" ]; then
    export DETRIX_TOKEN=$(cat "$TOKEN_FILE")
    echo "Loaded service token from ${TOKEN_FILE} (${#DETRIX_TOKEN} chars)"
else
    echo "WARNING: No service token file at ${TOKEN_FILE}"
fi

exec /usr/local/bin/detrix serve --config /data/detrix/detrix.toml
