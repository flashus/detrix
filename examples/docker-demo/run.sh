#!/usr/bin/env bash
set -euo pipefail

MODE="client"
ACTION="up"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      MODE="${2:-}"
      shift 2
      ;;
    --down)
      ACTION="down"
      shift
      ;;
    -h|--help)
      cat <<'EOF'
Usage:
  ./examples/docker-demo/run.sh [--mode client|agent] [--down]

Modes:
  client  Existing demo mode: order-service embeds the Detrix Go client.
  agent   eBPF agent mode: a privileged detrix-agent sidecar observes order-service.
EOF
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ "$MODE" != "client" && "$MODE" != "agent" ]]; then
  echo "--mode must be 'client' or 'agent'" >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PRICING_COMPOSE="$SCRIPT_DIR/pricing-api/docker-compose.yml"
CLIENT_COMPOSE="$SCRIPT_DIR/client-app/docker-compose.yml"

if [[ "$ACTION" == "down" ]]; then
  docker compose -f "$CLIENT_COMPOSE" --profile agent down -v
  docker compose -f "$PRICING_COMPOSE" down -v
  exit 0
fi

docker network create docker-demo >/dev/null 2>&1 || true
docker compose -f "$PRICING_COMPOSE" up -d --build

if [[ "$MODE" == "agent" ]]; then
  DETRIX_CLIENT_ENABLED=0 docker compose -f "$CLIENT_COMPOSE" --profile agent up -d --build
else
  docker compose -f "$CLIENT_COMPOSE" up -d --build
fi

docker compose -f "$CLIENT_COMPOSE" ps
