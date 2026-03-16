#!/bin/sh
# Read the service JWT from shared volume (written by token-init container)
# and export it as DETRIX_TOKEN before starting the Go app.
set -e

TOKEN_FILE="${TOKEN_FILE:-/shared/service-token}"

if [ -f "$TOKEN_FILE" ]; then
    export DETRIX_TOKEN=$(cat "$TOKEN_FILE")
    echo "Loaded service token from ${TOKEN_FILE} (${#DETRIX_TOKEN} chars)"
else
    echo "ERROR: Service token not found at ${TOKEN_FILE}"
    exit 1
fi

exec /usr/local/bin/detrix_example_app
