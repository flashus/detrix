#!/usr/bin/env bash
# Repeat pre-commit N times, logging all output and capturing error tails.
# Shows live tail of the running task with previous run results always visible.
#
# Usage:
#   ./scripts/repeat-precommit.sh        # 5 runs (default)
#   ./scripts/repeat-precommit.sh 10     # 10 runs
#   TASK_CMD="task tests:e2e" ./scripts/repeat-precommit.sh 3  # custom command
#   TAIL_LINES=20 ./scripts/repeat-precommit.sh  # show more tail lines

set -euo pipefail

RUNS="${1:-5}"
TASK_CMD="${TASK_CMD:-task pre-commit}"
TAIL_LINES="${TAIL_LINES:-10}"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
LOG_DIR="logs/${TIMESTAMP}"

mkdir -p "$LOG_DIR"

FULL_LOG="${LOG_DIR}/full.log"
ERROR_LOG="${LOG_DIR}/errors.log"
SUMMARY="${LOG_DIR}/summary.txt"

passed=0
failed=0
failed_runs=""
# Array of completed run result lines
completed_lines=()

fmt_duration() {
    local secs=$1
    printf "%02d:%02d:%02d" $((secs/3600)) $(( (secs%3600)/60 )) $((secs%60))
}

render() {
    tput clear 2>/dev/null || printf '\033[2J\033[H'
    echo "repeat-precommit: '${TASK_CMD}' x${RUNS}  |  Logs: ${LOG_DIR}/"
    echo ""

    # Previous runs
    for line in "${completed_lines[@]+"${completed_lines[@]}"}"; do
        echo "$line"
    done

    # Current run header
    if [ -n "${current_header:-}" ]; then
        local elapsed=$(( $(date +%s) - current_start ))
        echo "${current_header}  $(fmt_duration $elapsed) elapsed ..."
        echo "─────────────────────────────────────────"
        tail -"${TAIL_LINES}" "$current_log" 2>/dev/null || true
    fi
}

total_start=$(date +%s)

for i in $(seq 1 "$RUNS"); do
    current_log="${LOG_DIR}/run_${i}.log"
    current_start=$(date +%s)
    start_time=$(date '+%H:%M:%S')
    current_header="  Run ${i}/${RUNS}  started ${start_time}"

    echo "=== Run ${i}/${RUNS} ===" >> "$FULL_LOG"

    # Run command in background
    $TASK_CMD > "$current_log" 2>&1 &
    cmd_pid=$!

    # Live tail while running
    while kill -0 "$cmd_pid" 2>/dev/null; do
        render
        sleep 1
    done

    # Collect exit code
    if wait "$cmd_pid"; then
        elapsed=$(( $(date +%s) - current_start ))
        status_icon="PASS"
        passed=$((passed + 1))
    else
        exit_code=$?
        elapsed=$(( $(date +%s) - current_start ))
        status_icon="FAIL (exit ${exit_code})"
        failed=$((failed + 1))
        failed_runs="${failed_runs} ${i}"

        {
            echo "=========================================="
            echo "  Run ${i} — FAILED (exit ${exit_code})"
            echo "=========================================="
            echo ""
            tail -100 "$current_log"
            echo ""
        } >> "$ERROR_LOG"
    fi

    cat "$current_log" >> "$FULL_LOG"
    echo "" >> "$FULL_LOG"

    result_line="  Run ${i}/${RUNS}  ${start_time}  $(fmt_duration $elapsed)  ${status_icon}  -> run_${i}.log"
    completed_lines+=("$result_line")
    echo "Run ${i}: ${start_time}  $(fmt_duration $elapsed)  ${status_icon}" >> "$SUMMARY"

    current_header=""
    render
done

# Final summary
total_elapsed=$(( $(date +%s) - total_start ))
echo ""
echo "=========================================="
echo "  ${passed}/${RUNS} passed, ${failed} failed  |  Total: $(fmt_duration $total_elapsed)"
echo "=========================================="

{
    echo ""
    echo "=========================================="
    echo "  ${passed}/${RUNS} passed, ${failed} failed  |  Total: $(fmt_duration $total_elapsed)"
    if [ -n "$failed_runs" ]; then
        echo "  Failed runs:${failed_runs}"
    fi
    echo "=========================================="
} >> "$SUMMARY"

echo ""
echo "Full log:  ${FULL_LOG}"
echo "Summary:   ${SUMMARY}"
if [ "$failed" -gt 0 ]; then
    echo "Errors:    ${ERROR_LOG}"
    if command -v task &>/dev/null; then
        echo ""
        task tests:check-orphans 2>/dev/null || true
    fi
    exit 1
fi
