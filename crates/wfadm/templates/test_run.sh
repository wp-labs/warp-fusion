#!/usr/bin/env bash
set -euo pipefail

# wf-rules/test/run.sh — 有限时长 TCP daemon 联调脚本
# 从项目根目录运行

cd "$(dirname "${BASH_SOURCE[0]}")"  # cd to project root/

DURATION="${1:-${DURATION:-5m}}"
INTERVAL="${INTERVAL:-5}"
RATE_SLEEP="${RATE_SLEEP:-200}"

duration_to_seconds() {
    local value="$1"
    if [[ ! "$value" =~ ^[0-9]+[smh]?$ ]]; then
        echo "ERROR: invalid duration '$value' (use seconds, 30s, 5m, or 1h)" >&2
        return 1
    fi

    case "$value" in
        *s) echo "${value%s}" ;;
        *m) echo "$(( ${value%m} * 60 ))" ;;
        *h) echo "$(( ${value%h} * 3600 ))" ;;
        *) echo "$value" ;;
    esac
}

DURATION_SECONDS="$(duration_to_seconds "$DURATION")"
if [ "$DURATION_SECONDS" -le 0 ]; then
    echo "ERROR: duration must be > 0, got '$DURATION'" >&2
    exit 1
fi

echo "============================================"
echo "  wf-rules tcp integration: wfgen stream → wfusion"
echo "============================================"
echo "  duration=$DURATION (${DURATION_SECONDS}s), interval=${INTERVAL}s, rate_sleep=${RATE_SLEEP}ms"
echo "============================================"

cleanup() {
    [ -n "${WFGEN_PID:-}" ] && kill_wait "$WFGEN_PID" 2>/dev/null || true
    [ -n "${WFUSION_PID:-}" ] && kill_wait "$WFUSION_PID" 2>/dev/null || true
}
trap cleanup EXIT

# ── helpers ──

# kill with grace period, force SIGKILL if needed
kill_wait() {
    local pid="$1"
    local timeout="${2:-15}"
    kill "$pid" 2>/dev/null || true
    local elapsed=0
    while [ "$elapsed" -lt "$timeout" ]; do
        if ! kill -0 "$pid" 2>/dev/null; then
            return 0
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
    echo "   (force killing pid=$pid after ${timeout}s)" >&2
    kill -9 "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
}

mkdir -p data/alerts data/logs
rm -f data/alerts/*.ndjson data/logs/wfusion.log data/logs/wfgen.log

# 1. Start wfusion (loads schemas + rules from wf-rules/)
echo "1> wfusion: starting daemon..."
echo "   log=data/logs/wfusion.log"
wfusion daemon --config conf/wfusion.toml --work-dir . >data/logs/wfusion.log 2>&1 &
WFUSION_PID=$!
sleep 2
if ! kill -0 "$WFUSION_PID" 2>/dev/null; then
    echo "ERROR: wfusion exited before streaming started" >&2
    tail -n 80 data/logs/wfusion.log >&2 || true
    wait "$WFUSION_PID" 2>/dev/null || true
    exit 1
fi
echo "   wfusion PID=$WFUSION_PID"

# 2. wfgen stream — bounded data generation
echo "2> wfgen: streaming scenarios to TCP :9800..."
echo "   running for $DURATION"
echo "   log=data/logs/wfgen.log"
echo ""
WFL_ARGS=()
for f in models/rules/*/*.wfl; do WFL_ARGS+=(--wfl "$f"); done
wfgen stream \
    --scenario-dir models/scenarios/ \
    --ws models/schemas/network.wfs --ws models/schemas/auth.wfs --ws models/schemas/http.wfs --ws models/schemas/dns.wfs --ws models/schemas/management.wfs --ws models/schemas/data.wfs \
    "${WFL_ARGS[@]}" \
    --addr 127.0.0.1:9800 \
    --interval "$INTERVAL" \
    --rate-sleep "$RATE_SLEEP" >data/logs/wfgen.log 2>&1 &
WFGEN_PID=$!

elapsed=0
while [ "$elapsed" -lt "$DURATION_SECONDS" ]; do
    if ! kill -0 "$WFGEN_PID" 2>/dev/null; then
        echo "ERROR: wfgen stream exited before duration completed" >&2
        tail -n 80 data/logs/wfgen.log >&2 || true
        wait "$WFGEN_PID" 2>/dev/null || true
        exit 1
    fi
    sleep 1
    elapsed=$((elapsed + 1))
done

echo "3> stopping wfgen stream..."
kill_wait "$WFGEN_PID"
unset WFGEN_PID

echo "4> stopping wfusion daemon..."
kill_wait "$WFUSION_PID"
unset WFUSION_PID

echo "5> alert counts"
ALERT_FILES=(data/alerts/*.ndjson)
if [ ! -e "${ALERT_FILES[0]}" ]; then
    echo "0 total"
else
    wc -l "${ALERT_FILES[@]}"
fi
