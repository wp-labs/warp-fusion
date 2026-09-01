#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

PROFILE="debug"
case "${1:-}" in
    "" ) ;;
    release | --release ) PROFILE="release" ;;
    debug | --debug ) PROFILE="debug" ;;
    * )
        echo "Usage: ./run.sh [debug|release]" >&2
        exit 2
        ;;
esac

REPO_ROOT="$(cd ../../.. && pwd)"
WFL_BIN="$REPO_ROOT/target/$PROFILE/wfl"
WFUSION_BIN="$REPO_ROOT/target/$PROFILE/wfusion"

if [ ! -x "$WFL_BIN" ]; then
    echo "ERROR: wfl binary not found or not executable: $WFL_BIN" >&2
    echo "       build it first, for example: cargo build --bin wfl" >&2
    exit 1
fi

if [ ! -x "$WFUSION_BIN" ]; then
    echo "ERROR: wfusion binary not found or not executable: $WFUSION_BIN" >&2
    echo "       build it first, for example: cargo build --bin wfusion" >&2
    exit 1
fi

ALERT_FILE="data/out_dat/alerts.ndjson"
MONITOR_FILE="data/out_dat/metrics.ndjson"
WFUSION_PID=""

cleanup() {
    if [ -n "$WFUSION_PID" ] && kill -0 "$WFUSION_PID" 2>/dev/null; then
        kill "$WFUSION_PID" 2>/dev/null || true
        wait "$WFUSION_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

echo "Using profile: $PROFILE"
echo "  wfl     = $WFL_BIN"
echo "  wfusion = $WFUSION_BIN"
echo

echo "1> lint rule"
"$WFL_BIN" lint rules/window_miss.wfl --schemas "schemas/*.wfs"

echo "2> run inline tests"
TEST_OUT="$("$WFL_BIN" test rules/window_miss.wfl --schemas "schemas/*.wfs")"
echo "$TEST_OUT"
if echo "$TEST_OUT" | grep -q '^FAIL[[:space:]]'; then
    echo "ERROR: inline tests failed" >&2
    exit 1
fi

echo "3> clean previous batch output"
rm -rf data/out_dat

echo "4> run daemon replay and wait for monitor metrics"
"$WFUSION_BIN" daemon --config wfusion.toml &
WFUSION_PID=$!

for _ in $(seq 1 10); do
    sleep 1
    if ! kill -0 "$WFUSION_PID" 2>/dev/null; then
        wait "$WFUSION_PID" 2>/dev/null || true
        WFUSION_PID=""
        echo "ERROR: wfusion daemon exited before monitor metrics were observed" >&2
        exit 1
    fi
    if [ -s "$MONITOR_FILE" ] \
        && grep -q '"name":"window_miss_total"' "$MONITOR_FILE" \
        && grep -q '"reason":"unknown_stream_schema"' "$MONITOR_FILE" \
        && grep -q '"reason":"missing_stream_tag_field"' "$MONITOR_FILE"; then
        break
    fi
done

if [ -n "$WFUSION_PID" ] && kill -0 "$WFUSION_PID" 2>/dev/null; then
    kill "$WFUSION_PID" 2>/dev/null || true
    wait "$WFUSION_PID" 2>/dev/null || true
    WFUSION_PID=""
fi

echo "5> verify known stream alert"
if [ ! -f "$ALERT_FILE" ]; then
    echo "ERROR: missing alert output: $ALERT_FILE" >&2
    exit 1
fi

ALERT_COUNT="$(wc -l < "$ALERT_FILE" | tr -d ' ')"
if [ "$ALERT_COUNT" != "1" ]; then
    echo "ERROR: expected 1 alert, got $ALERT_COUNT" >&2
    cat "$ALERT_FILE" >&2
    exit 1
fi

if ! grep -q '"__wfu_rule_name":"known_netflow_still_routes"' "$ALERT_FILE"; then
    echo "ERROR: missing known_netflow_still_routes alert" >&2
    cat "$ALERT_FILE" >&2
    exit 1
fi

if [ -s data/out_dat/error.ndjson ]; then
    echo "ERROR: unexpected error sink output" >&2
    cat data/out_dat/error.ndjson >&2
    exit 1
fi

echo "6> verify monitor window miss metrics"
if [ ! -f "$MONITOR_FILE" ]; then
    echo "ERROR: missing monitor output: $MONITOR_FILE" >&2
    exit 1
fi

if ! grep -q '"name":"window_miss_total"' "$MONITOR_FILE"; then
    echo "ERROR: missing window_miss_total metrics" >&2
    cat "$MONITOR_FILE" >&2
    exit 1
fi

UNKNOWN_MISS_COUNT="$(awk '/"name":"window_miss_total"/ && /"reason":"unknown_stream_schema"/ && /"label":"window_miss_source"/ { if (match($0, /"value":"[0-9]+"/)) { v=substr($0, RSTART + 9, RLENGTH - 10); sum += v } } END { print sum + 0 }' "$MONITOR_FILE")"
MISSING_TAG_COUNT="$(awk '/"name":"window_miss_total"/ && /"reason":"missing_stream_tag_field"/ && /"label":"window_miss_source"/ { if (match($0, /"value":"[0-9]+"/)) { v=substr($0, RSTART + 9, RLENGTH - 10); sum += v } } END { print sum + 0 }' "$MONITOR_FILE")"

if [ "$UNKNOWN_MISS_COUNT" != "1" ]; then
    echo "ERROR: expected unknown_stream_schema metric count 1, got $UNKNOWN_MISS_COUNT" >&2
    cat "$MONITOR_FILE" >&2
    exit 1
fi

if [ "$MISSING_TAG_COUNT" != "1" ]; then
    echo "ERROR: expected missing_stream_tag_field metric count 1, got $MISSING_TAG_COUNT" >&2
    cat "$MONITOR_FILE" >&2
    exit 1
fi

echo "OK: window miss rows were skipped, counted, and the known stream still routed"
