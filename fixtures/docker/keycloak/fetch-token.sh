#!/bin/sh
# Fetch a JWT from Keycloak and write it to a shared volume.
# Used as an init container to provide a service token for both the
# Detrix daemon and Go app before they start.
set -e

KEYCLOAK_URL="${KEYCLOAK_URL:-http://keycloak:8080}"
REALM="${KEYCLOAK_REALM:-detrix}"
CLIENT_ID="${KEYCLOAK_CLIENT_ID:-detrix-service}"
USERNAME="${KEYCLOAK_USERNAME:-go-service}"
PASSWORD="${KEYCLOAK_PASSWORD:-go-service-pass}"
TOKEN_FILE="${TOKEN_FILE:-/shared/service-token}"

TOKEN_URL="${KEYCLOAK_URL}/realms/${REALM}/protocol/openid-connect/token"

echo "Waiting for Keycloak at ${KEYCLOAK_URL}..."
for i in $(seq 1 90); do
    if curl -sf "${KEYCLOAK_URL}/health/ready" > /dev/null 2>&1; then
        echo "Keycloak ready"
        break
    fi
    sleep 1
done

echo "Fetching JWT from Keycloak (client=${CLIENT_ID}, user=${USERNAME})..."
RESPONSE=$(curl -sf -X POST "${TOKEN_URL}" \
    -d "grant_type=password" \
    -d "client_id=${CLIENT_ID}" \
    -d "username=${USERNAME}" \
    -d "password=${PASSWORD}")

TOKEN=$(echo "${RESPONSE}" | sed 's/.*"access_token":"\([^"]*\)".*/\1/')

if [ -z "$TOKEN" ] || [ "$TOKEN" = "$RESPONSE" ]; then
    echo "ERROR: Failed to get JWT from Keycloak"
    echo "Response: ${RESPONSE}"
    exit 1
fi

echo -n "$TOKEN" > "$TOKEN_FILE"
echo "Service token written to ${TOKEN_FILE} (${#TOKEN} chars)"
