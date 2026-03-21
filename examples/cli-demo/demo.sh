#!/usr/bin/env bash
# Detrix CLI demo — observe a running Python app without restarts or code changes
#
# What this script does:
#   1. Starts a trading bot fixture with debugpy
#   2. Starts the Detrix daemon
#   3. Creates a debugger connection to the bot
#   4. Adds observation points on key variables (multiple per location)
#   5. Waits a few seconds for events to accumulate
#   6. Queries and displays the captured values
#   7. Cleans up
#
# Requirements:
#   - detrix binary on PATH (or set DETRIX_BIN below)
#   - python3 with debugpy installed: pip install debugpy
#
# Usage:
#   chmod +x demo.sh && ./demo.sh

set -e

DETRIX_BIN="${DETRIX_BIN:-detrix}"
PYTHON="${PYTHON:-uv run --with debugpy python}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE="$SCRIPT_DIR/../../fixtures/python/detrix_example_app_continuous.py"
DEBUGPY_PORT=5679
OBSERVE_SECONDS=5
# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
PURPLE='\033[0;35m'
NC='\033[0m'

log()  { echo -e "${BLUE}[detrix-demo]${NC} $*"; }
ok()   { echo -e "${GREEN}[detrix-demo]${NC} $*"; }
info() { echo -e "${YELLOW}[detrix-demo]${NC} $*"; }

cleanup() {
    log "Cleaning up..."
    [ -n "$APP_PID" ]    && kill "$APP_PID"    2>/dev/null || true
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null || true
    wait 2>/dev/null || true
    ok "Done."
}
trap cleanup EXIT INT TERM

# ── 1. Start trading bot with debugpy ────────────────────────────────────────
log "Starting trading bot fixture with debugpy on port $DEBUGPY_PORT..."
$PYTHON -m debugpy --listen "$DEBUGPY_PORT" "$FIXTURE" \
    > >(while IFS= read -r line; do echo -e "${PURPLE}[detrix-demo-app]${NC} $line"; done) \
    2>&1 &
APP_PID=$!
sleep 1
ok "Trading bot started (PID $APP_PID)"

# ── 2. Start Detrix daemon (or use existing) ──────────────────────────────────
log "Starting Detrix daemon..."
"$DETRIX_BIN" daemon start 2>/dev/null || true
sleep 1
ok "Daemon ready"

# ── 3a. Remove previous demo metrics ─────────────────────────────────────────
# ON DELETE CASCADE removes all their captured events — clean slate per run
log "Clearing previous demo metrics..."
"$DETRIX_BIN" metric remove order_placed   2>/dev/null || true
"$DETRIX_BIN" metric remove trade_executed 2>/dev/null || true
"$DETRIX_BIN" metric remove pnl_snapshot   2>/dev/null || true

# ── 3. Create debugger connection ─────────────────────────────────────────────
log "Connecting to debugpy on port $DEBUGPY_PORT..."
CONN_ID=$("$DETRIX_BIN" connection create \
    --port "$DEBUGPY_PORT" \
    --language python \
    --quiet)
ok "Connection created: $CONN_ID"
sleep 1

# ── 4. Add observation points ─────────────────────────────────────────────────
# Multiple expressions per location — all captured in a single logpoint

log "Adding observation points..."

# Order placement — observe on return line where all variables are in scope
"$DETRIX_BIN" metric add order_placed \
    -l "$FIXTURE#57" \
    -e "order_info" \
    -C "$CONN_ID"
ok "  order_placed @ line 57: order_info"

# Trade execution — capture on return line where trade_result is fully in scope
"$DETRIX_BIN" metric add trade_executed \
    -l "$FIXTURE#83" \
    -e "trade_result" \
    -e "portfolio.total_trades" \
    -e "portfolio.cash" \
    -C "$CONN_ID"
ok "  trade_executed @ line 83: trade_result, portfolio.total_trades, portfolio.cash"

# P&L calculation — capture on return line where pnl_data is fully in scope
"$DETRIX_BIN" metric add pnl_snapshot \
    -l "$FIXTURE#102" \
    -e "pnl_data" \
    -e "portfolio_value" \
    -C "$CONN_ID"
ok "  pnl_snapshot @ line 102: pnl_data, portfolio_value"

# ── 5. Wait for events ────────────────────────────────────────────────────────
info "Observing for $OBSERVE_SECONDS seconds (bot is running, no restarts)..."
sleep "$OBSERVE_SECONDS"

# ── 6. Query results ──────────────────────────────────────────────────────────
echo ""
ok "=== Captured values ==="
echo ""

log "Latest order_placed:"
"$DETRIX_BIN" event latest order_placed --format table

echo ""
log "Latest trade_executed:"
"$DETRIX_BIN" event latest trade_executed --format table

echo ""
log "Latest pnl_snapshot:"
"$DETRIX_BIN" event latest pnl_snapshot --format table

echo ""
log "All recent events (last 10):"
"$DETRIX_BIN" event query --limit 10 --format table

echo ""
ok "=== Done — bot ran untouched, no code changes, no restarts ==="
