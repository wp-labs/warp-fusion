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
ERROR_FILE="data/out_dat/error.ndjson"

echo "Using profile: $PROFILE"
echo "  wfl     = $WFL_BIN"
echo "  wfusion = $WFUSION_BIN"
echo

echo "1> lint rule"
"$WFL_BIN" lint rules/hello_detection.wfl --schemas "schemas/*.wfs"

echo "2> run inline tests"
TEST_OUT="$("$WFL_BIN" test rules/hello_detection.wfl --schemas "schemas/*.wfs")"
echo "$TEST_OUT"
if echo "$TEST_OUT" | grep -q '^FAIL[[:space:]]'; then
    echo "ERROR: inline tests failed" >&2
    exit 1
fi

echo "3> clean previous batch output"
if [ -d data/out_dat ]; then
    find data/out_dat -type f -delete 2>/dev/null || true
    rmdir data/out_dat 2>/dev/null || true
fi

echo "4> run batch replay"
"$WFUSION_BIN" batch --config wfusion.toml --work-dir .

echo "5> verify alerts"
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

for kv in '"alert_type":"brute_login_mini"' '"__wfu_entity_id":"10.0.0.5"' '"sip":"10.0.0.5"'; do
    if ! grep -q "$kv" "$ALERT_FILE"; then
        echo "ERROR: missing $kv" >&2
        cat "$ALERT_FILE" >&2
        exit 1
    fi
done

if [ -s "$ERROR_FILE" ]; then
    echo "ERROR: unexpected error sink output" >&2
    cat "$ERROR_FILE" >&2
    exit 1
fi

echo "OK: hello_detection produced 1 alert (3 failed logins in 1m -> brute_login_mini)"
